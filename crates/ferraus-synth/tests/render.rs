//! Rendering acceptance tests: output length = MIDI length + tail (±1 ms), note onsets in
//! the audio within 1 ms of the MIDI onsets for the built-in synth, a minimal SF2 written
//! here, and an SFZ + WAV written here; switching instruments changes only the sound;
//! `--map`, `--swap`, aliases, scanned sound folders, `--fx`, dry runs, and plugin errors.

use ferraus_core::prelude::*;
use ferraus_core::testing::{self, TempDir};
use ferraus_core::Runner;
use ferraus_midi::{MidiFile, SecondsNote};
use ferraus_synth::{render, InstrumentSource, RenderOptions, Resolver, Sf2Preset};
use std::path::{Path, PathBuf};
use std::sync::Once;

static ISOLATE: Once = Once::new();

/// Point the data/config dirs at an empty temp dir so the user's sounds are not used.
fn isolate() {
    ISOLATE.call_once(|| {
        let d = std::env::temp_dir().join(format!("ferraus-synth-test-home-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("FERRAUS_HOME", &d);
        std::env::remove_var("FERRAUS_CONFIG");
    });
}

// ------------------------------------------------------------------------------------
// fixtures
// ------------------------------------------------------------------------------------

/// Sparse notes at onsets that are not on any sample or tick grid.
const ONSETS: [f64; 4] = [0.2513, 1.8377, 3.4011, 4.9629];

fn sparse_midi(bpm: f64) -> MidiFile {
    sparse_midi_at(bpm, &ONSETS)
}

fn sparse_midi_at(bpm: f64, onsets: &[f64]) -> MidiFile {
    let notes: Vec<SecondsNote> = onsets
        .iter()
        .zip([60u8, 64, 67, 72])
        .map(|(&s, p)| SecondsNote { pitch: p, start_s: s, duration_s: 0.3, velocity: 100, channel: 1, track: 0 })
        .collect();
    MidiFile::from_notes(&notes, bpm, 960, None)
}

/// Onset times (s) of a signal: first sample where an 8-sample RMS exceeds -60 dB of the
/// peak after at least 40 ms below it.
fn detect_onsets(x: &[f32], sr: u32) -> Vec<f64> {
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs())) as f64;
    let thr = peak * 1e-3;
    let win = 8;
    let gap = (0.04 * sr as f64) as usize;
    let mut quiet = usize::MAX / 2;
    let mut out = Vec::new();
    let mut acc: f64 = x.iter().take(win).map(|v| (*v as f64).powi(2)).sum();
    for n in 0..x.len().saturating_sub(win) {
        let rms = (acc / win as f64).sqrt();
        if rms > thr {
            // a real onset rises well above the threshold soon after (not a beating tail)
            if quiet >= gap && rises(x, n, thr * 30.0, sr) {
                out.push(n as f64 / sr as f64);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
        acc += (x[n + win] as f64).powi(2) - (x[n] as f64).powi(2);
        acc = acc.max(0.0);
    }
    out
}

fn rises(x: &[f32], n: usize, level: f64, sr: u32) -> bool {
    let end = (n + (0.05 * sr as f64) as usize).min(x.len());
    x[n..end].iter().any(|v| (*v as f64).abs() > level)
}

fn assert_onsets(buf: &AudioBuffer, midi: &MidiFile, what: &str) {
    let mono = buf.mono_mix();
    let got = detect_onsets(&mono, buf.sample_rate);
    let want: Vec<f64> = midi.notes_seconds().iter().map(|n| n.start_s).collect();
    assert_eq!(got.len(), want.len(), "{what}: detected {got:?}, expected {want:?}");
    let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0.0, f64::max);
    eprintln!("{what}: worst onset error {:.3} ms", worst * 1000.0);
    for (g, w) in got.iter().zip(&want) {
        assert!((g - w).abs() <= 0.001, "{what}: onset {g:.5}s vs MIDI {w:.5}s (error {:.3} ms)", (g - w) * 1000.0);
    }
}

fn assert_length(buf: &AudioBuffer, midi: &MidiFile, tail: f64, what: &str) {
    let want = midi.duration_seconds() + tail;
    assert!((buf.duration_s() - want).abs() <= 0.001, "{what}: {} s vs {} s", buf.duration_s(), want);
}

fn le16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn le32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn name20(v: &mut Vec<u8>, s: &str) {
    let mut b = [0u8; 20];
    b[..s.len().min(19)].copy_from_slice(&s.as_bytes()[..s.len().min(19)]);
    v.extend_from_slice(&b);
}
fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    le32(&mut v, body.len() as u32);
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}
fn list(kind: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
    let mut body = kind.to_vec();
    for p in parts {
        body.extend_from_slice(p);
    }
    chunk(b"LIST", &body)
}

/// A minimal valid SoundFont 2: one looped 440 Hz sine sample, one instrument, and presets
/// at bank 0 programs 0 and 48 and the drum bank 128 program 0.
fn write_sf2(path: &Path) {
    write_sf2_with(path, false)
}

/// `zero_delay` sets delayVolEnv to its minimum instead of the 1 ms spec default.
fn write_sf2_with(path: &Path, zero_delay: bool) {
    let sr = 44000u32;
    let cycles = 20usize;
    let per = 100usize; // 440 Hz at 44 kHz
    let n = cycles * per;
    let mut smpl = Vec::new();
    for i in 0..n {
        let v = (2.0 * std::f64::consts::PI * i as f64 / per as f64).sin() * 0.8;
        smpl.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
    }
    smpl.extend(std::iter::repeat_n(0u8, 46 * 2));
    let mut ifil = Vec::new();
    le16(&mut ifil, 2);
    le16(&mut ifil, 1);
    let info =
        list(b"INFO", &[chunk(b"ifil", &ifil), chunk(b"isng", b"EMU8000\0"), chunk(b"INAM", b"ferraus test\0\0")]);
    let sdta = list(b"sdta", &[chunk(b"smpl", &smpl)]);
    // presets: (name, program, bank)
    let presets = [("Sine Piano", 0u16, 0u16), ("Sine Strings", 48, 0), ("Sine Kit", 0, 128)];
    let mut phdr = Vec::new();
    for (i, (nm, prog, bank)) in presets.iter().enumerate() {
        name20(&mut phdr, nm);
        le16(&mut phdr, *prog);
        le16(&mut phdr, *bank);
        le16(&mut phdr, i as u16);
        le32(&mut phdr, 0);
        le32(&mut phdr, 0);
        le32(&mut phdr, 0);
    }
    name20(&mut phdr, "EOP");
    le16(&mut phdr, 0);
    le16(&mut phdr, 0);
    le16(&mut phdr, presets.len() as u16);
    le32(&mut phdr, 0);
    le32(&mut phdr, 0);
    le32(&mut phdr, 0);
    let mut pbag = Vec::new();
    for i in 0..=presets.len() {
        le16(&mut pbag, i as u16); // one generator per preset zone
        le16(&mut pbag, 0);
    }
    let pmod = vec![0u8; 10];
    let mut pgen = Vec::new();
    for _ in 0..presets.len() {
        le16(&mut pgen, 41); // instrument
        le16(&mut pgen, 0);
    }
    le32(&mut pgen, 0);
    let mut inst = Vec::new();
    name20(&mut inst, "Sine");
    le16(&mut inst, 0);
    name20(&mut inst, "EOI");
    le16(&mut inst, 1);
    let ngen = if zero_delay { 5 } else { 4 };
    let mut ibag = Vec::new();
    le16(&mut ibag, 0);
    le16(&mut ibag, 0);
    le16(&mut ibag, ngen);
    le16(&mut ibag, 0);
    let imod = vec![0u8; 10];
    let mut igen = Vec::new();
    if zero_delay {
        le16(&mut igen, 33); // delayVolEnv: minimum
        igen.extend_from_slice(&(-32768i16).to_le_bytes());
    }
    le16(&mut igen, 38); // releaseVolEnv: 0.1 s
    igen.extend_from_slice(&(-3986i16).to_le_bytes());
    le16(&mut igen, 54); // sampleModes: loop continuously
    le16(&mut igen, 1);
    le16(&mut igen, 58); // overridingRootKey
    le16(&mut igen, 69);
    le16(&mut igen, 53); // sampleID (last)
    le16(&mut igen, 0);
    le32(&mut igen, 0);
    let mut shdr = Vec::new();
    name20(&mut shdr, "sine440");
    le32(&mut shdr, 0);
    le32(&mut shdr, n as u32);
    le32(&mut shdr, 0);
    le32(&mut shdr, n as u32);
    le32(&mut shdr, sr);
    shdr.push(69);
    shdr.push(0);
    le16(&mut shdr, 0);
    le16(&mut shdr, 1);
    name20(&mut shdr, "EOS");
    shdr.extend(std::iter::repeat_n(0u8, 26));
    let pdta = list(
        b"pdta",
        &[
            chunk(b"phdr", &phdr),
            chunk(b"pbag", &pbag),
            chunk(b"pmod", &pmod),
            chunk(b"pgen", &pgen),
            chunk(b"inst", &inst),
            chunk(b"ibag", &ibag),
            chunk(b"imod", &imod),
            chunk(b"igen", &igen),
            chunk(b"shdr", &shdr),
        ],
    );
    let mut body = b"sfbk".to_vec();
    body.extend(info);
    body.extend(sdta);
    body.extend(pdta);
    std::fs::write(path, chunk(b"RIFF", &body)).unwrap();
}

/// An SFZ instrument with one 16-bit WAV sine sample.
fn write_sfz(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("samples")).unwrap();
    let sine = testing::sine(48000, 440.0, 1.0, 0.7);
    std::fs::write(dir.join("samples/sine 440.wav"), ferraus_core::wav::encode(&sine, 16, false).unwrap()).unwrap();
    let p = dir.join("Test Strings.sfz");
    std::fs::write(
        &p,
        "// test library\n<control> default_path=samples/\n<group> ampeg_release=0.05 amp_veltrack=50\n<region> sample=sine 440.wav pitch_keycenter=a4 lokey=0 hikey=127\n",
    )
    .unwrap();
    p
}

// ------------------------------------------------------------------------------------
// library-level acceptance
// ------------------------------------------------------------------------------------

#[test]
fn builtin_length_and_onsets() {
    // long-release timbres (pad, bell) need wider gaps for a silence-based detector
    let midi = sparse_midi_at(97.0, &[0.2513, 3.4377, 6.6011, 9.7629]);
    for inst in ["piano", "strings", "organ", "guitar", "bass", "lead", "pad", "brass", "bell"] {
        let opts = RenderOptions { tail_s: 1.5, ..Default::default() };
        let buf = render(&midi, &InstrumentSource::builtin(inst), &opts).unwrap();
        assert_eq!(buf.channels(), 2);
        assert_length(&buf, &midi, 1.5, inst);
        assert_onsets(&buf, &midi, inst);
    }
}

#[test]
fn builtin_drums_onsets() {
    let notes: Vec<SecondsNote> = ONSETS
        .iter()
        .zip([36u8, 38, 42, 49])
        .map(|(&s, p)| SecondsNote { pitch: p, start_s: s, duration_s: 0.1, velocity: 110, channel: 10, track: 0 })
        .collect();
    let midi = MidiFile::from_notes(&notes, 120.0, 480, None);
    let buf = render(&midi, &InstrumentSource::builtin("gm"), &RenderOptions::default()).unwrap();
    // drums ring longer than the gaps for cymbals, so check the first three only
    let got = detect_onsets(&buf.mono_mix()[..(4.5 * 48000.0) as usize], 48000);
    for (g, w) in got.iter().zip(&ONSETS[..3]) {
        assert!((g - w).abs() <= 0.001, "drum onset {g} vs {w}");
    }
    assert_eq!(got.len(), 3);
}

#[test]
fn sf2_length_and_onsets() {
    let d = TempDir::new("sf2");
    let sf = d.path("test.sf2");
    write_sf2(&sf);
    let sf0 = d.path("zero-delay.sf2");
    write_sf2_with(&sf0, true);
    let presets = ferraus_synth::sf2::list_presets(&sf).unwrap();
    assert_eq!(presets.len(), 3);
    let midi = sparse_midi(120.0);
    for (file, preset) in [(&sf, None), (&sf, Some(Sf2Preset { bank: 0, program: 48 })), (&sf0, None)] {
        let src = InstrumentSource::Sf2 { path: file.clone(), preset };
        let opts = RenderOptions { sample_rate: 44100, tail_s: 1.0, ..Default::default() };
        let buf = render(&midi, &src, &opts).unwrap();
        assert_length(&buf, &midi, 1.0, "sf2");
        assert_onsets(&buf, &midi, "sf2");
        // the sample is a 440 Hz sine at root key 69, so MIDI 60 plays C4
        let s = (ONSETS[0] * 44100.0) as usize;
        let seg = &buf.data[0][s + 2000..s + 10000];
        let c4 = ferraus_core::testing::tone_level(seg, 44100, 261.63);
        let a4 = ferraus_core::testing::tone_level(seg, 44100, 440.0);
        assert!(c4 > 10.0 * a4, "pitch: {c4} vs {a4}");
    }
}

#[test]
fn sfz_length_and_onsets() {
    let d = TempDir::new("sfz");
    let p = write_sfz(&d.0);
    let midi = sparse_midi(133.0);
    let opts = RenderOptions { tail_s: 0.5, ..Default::default() };
    let buf = render(&midi, &InstrumentSource::Sfz { path: p }, &opts).unwrap();
    assert_length(&buf, &midi, 0.5, "sfz");
    assert_onsets(&buf, &midi, "sfz");
    let s = (ONSETS[1] * 48000.0) as usize; // E4 = 329.63 Hz
    let seg = &buf.data[0][s + 1000..s + 12000];
    assert!(
        ferraus_core::testing::tone_level(seg, 48000, 329.63)
            > 10.0 * ferraus_core::testing::tone_level(seg, 48000, 440.0)
    );
}

#[test]
fn switching_instruments_changes_only_the_sound() {
    let midi = sparse_midi(111.0);
    let opts = RenderOptions::default();
    let a = render(&midi, &InstrumentSource::builtin("piano"), &opts).unwrap();
    let b = render(&midi, &InstrumentSource::builtin("strings"), &opts).unwrap();
    assert_eq!(a.frames(), b.frames());
    let oa = detect_onsets(&a.mono_mix(), 48000);
    let ob = detect_onsets(&b.mono_mix(), 48000);
    assert_eq!(oa.len(), ob.len());
    for (x, y) in oa.iter().zip(&ob) {
        assert!((x - y).abs() <= 0.001);
    }
    // the note-on/off schedule is identical apart from the forced program
    let notes = |src: &str| -> Vec<(u64, u8, u8)> {
        ferraus_synth::render::plan(&midi, &InstrumentSource::builtin(src), &opts)[0]
            .events
            .iter()
            .filter(|e| e.status == 0x8 || e.status == 0x9)
            .map(|e| (e.sample, e.status, e.data1))
            .collect()
    };
    assert_eq!(notes("piano"), notes("strings"));
    assert!(ferraus_core::testing::rms_diff(&a, &b) > 1e-3, "the sound changed");
}

#[test]
fn deterministic_renders() {
    let midi = sparse_midi(120.0);
    let a = render(&midi, &InstrumentSource::builtin("gm"), &RenderOptions::default()).unwrap();
    let b = render(&midi, &InstrumentSource::builtin("gm"), &RenderOptions::default()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn empty_and_tiny_files() {
    let midi = MidiFile::new(480, 120.0);
    let buf = render(&midi, &InstrumentSource::builtin("piano"), &RenderOptions { tail_s: 0.25, ..Default::default() })
        .unwrap();
    assert_eq!(buf.frames(), 12000);
    assert!(buf.peak() == 0.0);
}

// ------------------------------------------------------------------------------------
// the render op
// ------------------------------------------------------------------------------------

fn gain_op(a: &Args, mut b: AudioBuffer, _c: &mut OpCtx) -> Result<AudioBuffer> {
    b.scale(a.gain("by")?.linear() as f32);
    Ok(b)
}

fn registry() -> &'static ferraus_core::Registry {
    let mut ops = ferraus_synth::ops();
    ops.push(
        OpSpec::transform("gain", Category::Edit, "Gain", gain_op).param(param("by", ParamKind::Gain).default("0dB")),
    );
    testing::leak_registry(ops)
}

#[derive(Default)]
struct Run {
    sound_paths: Vec<PathBuf>,
    dry_run: bool,
    sample_rate: Option<u32>,
}

fn run_render(pairs: &[(&str, &str)], input: &Path, out: &Path, r: Run) -> Result<(Value, Map<String, Value>)> {
    isolate();
    let reg = registry();
    let spec = reg.get("render").unwrap();
    let a = testing::args(spec, pairs);
    let mut ctx = testing::ctx_with(reg);
    ctx.command = "render".into();
    ctx.inputs = vec![input.to_path_buf()];
    ctx.globals.output = Some(out.display().to_string());
    ctx.globals.sound_paths = r.sound_paths;
    ctx.globals.dry_run = r.dry_run;
    ctx.globals.sample_rate = r.sample_rate;
    let v = match spec.runner {
        Runner::Custom(f) => f(&a, &mut ctx)?,
        _ => unreachable!(),
    };
    Ok((v, ctx.notes.clone()))
}

use serde_json::Map;

fn write_midi(d: &TempDir, name: &str, f: &MidiFile) -> PathBuf {
    let p = d.path(name);
    std::fs::write(&p, f.to_smf_bytes()).unwrap();
    p
}

#[test]
fn render_op_writes_24_bit_wav_of_the_right_length() {
    let d = TempDir::new("op");
    let midi = sparse_midi(120.0);
    let inp = write_midi(&d, "part.mid", &midi);
    let out = d.path("part.wav");
    let (v, notes) = run_render(&[("instrument", "piano"), ("tail", "2s")], &inp, &out, Run::default()).unwrap();
    assert_eq!(v["instrument"], "builtin:piano");
    assert_eq!(notes["instrument"]["given"], "piano");
    assert_eq!(notes["instrument"]["via"], "gm");
    assert_eq!(notes["instrument"]["program"], 0);
    let bytes = std::fs::read(&out).unwrap();
    let (buf, bits, _) = ferraus_core::wav::decode(&bytes).unwrap();
    assert_eq!(bits, 24);
    assert_eq!(buf.sample_rate, 48000);
    assert_length(&buf, &midi, 2.0, "op");
    assert_onsets(&buf, &midi, "op");
    // global sample rate
    let out2 = d.path("part44.wav");
    run_render(&[], &inp, &out2, Run { sample_rate: Some(44100), ..Default::default() }).unwrap();
    let (b2, _, _) = ferraus_core::wav::decode(&std::fs::read(&out2).unwrap()).unwrap();
    assert_eq!(b2.sample_rate, 44100);
    assert_length(&b2, &midi, 2.0, "op 44.1k");
}

#[test]
fn render_op_fx_stage_and_dry_run() {
    let d = TempDir::new("fx");
    let midi = sparse_midi(120.0);
    let inp = write_midi(&d, "part.mid", &midi);
    let plain = d.path("plain.wav");
    let quiet = d.path("quiet.wav");
    run_render(&[], &inp, &plain, Run::default()).unwrap();
    run_render(&[("fx", "gain:by=-6dB")], &inp, &quiet, Run::default()).unwrap();
    let p = testing::read_wav(&plain).peak();
    let q = testing::read_wav(&quiet).peak();
    assert!((q / p - 0.501).abs() < 0.01, "{q} / {p}");
    let dry = d.path("dry.wav");
    let (v, _) = run_render(&[], &inp, &dry, Run { dry_run: true, ..Default::default() }).unwrap();
    assert!(!dry.exists());
    assert_eq!(v["plan"][0]["source"]["type"], "built_in");
    assert!((v["duration_s"].as_f64().unwrap() - (midi.duration_seconds() + 2.0)).abs() < 1e-9);
}

#[test]
fn render_op_map_and_swap() {
    let d = TempDir::new("map");
    let mut midi = MidiFile::from_notes(
        &[
            SecondsNote { pitch: 60, start_s: 0.2, duration_s: 0.3, velocity: 100, channel: 1, track: 0 },
            SecondsNote { pitch: 48, start_s: 1.7, duration_s: 0.3, velocity: 100, channel: 2, track: 1 },
            SecondsNote { pitch: 38, start_s: 3.5, duration_s: 0.1, velocity: 100, channel: 10, track: 2 },
        ],
        120.0,
        480,
        None,
    );
    midi.tracks[1].programs.push(ferraus_midi::Timed::new(0, ferraus_midi::model::Program { channel: 1, program: 1 }));
    let inp = write_midi(&d, "song.mid", &midi);
    let out = d.path("song.wav");
    let (v, notes) = run_render(&[("map", "1=organ,2=strings,10=drums")], &inp, &out, Run::default()).unwrap();
    let parts: Vec<&str> =
        notes["instruments"].as_array().unwrap().iter().map(|x| x["part"].as_str().unwrap()).collect();
    assert_eq!(parts, vec!["track 1", "track 2", "channel 10"]);
    assert_eq!(v["plan"].as_array().unwrap().len(), 3);
    let buf = testing::read_wav(&out);
    assert_onsets(&buf, &midi, "map");
    // swap: the piano-family channels (1 and 2, program 0 and 1) become strings
    let out2 = d.path("swap.wav");
    let (_, notes) = run_render(&[("swap", "piano=strings")], &inp, &out2, Run::default()).unwrap();
    assert_eq!(notes["instruments"].as_array().unwrap().len(), 2);
    assert!(notes["instruments"][0]["part"].as_str().unwrap().starts_with("channel 1"));
    // bad entries are usage errors
    let e = run_render(&[("map", "17=piano")], &inp, &d.path("x.wav"), Run::default()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
}

#[test]
fn sound_path_names_categories_and_ambiguity() {
    let d = TempDir::new("paths");
    let lib = d.path("lib");
    std::fs::create_dir_all(lib.join("Pianos")).unwrap();
    write_sf2(&lib.join("Pianos/Sine Grand.sf2"));
    write_sfz(&lib.join("Strings"));
    let midi = sparse_midi(120.0);
    let inp = write_midi(&d, "part.mid", &midi);
    let r = Run { sound_paths: vec![lib.clone()], ..Default::default() };
    let (v, notes) = run_render(&[("instrument", "strings")], &inp, &d.path("s.wav"), r).unwrap();
    assert_eq!(notes["instrument"]["via"], "sound");
    assert_eq!(notes["instrument"]["source"]["type"], "sfz");
    assert!(v["instrument"].as_str().unwrap().ends_with("Test Strings.sfz"));
    let r = Run { sound_paths: vec![lib.clone()], ..Default::default() };
    let (_, notes) = run_render(&[("instrument", "piano")], &inp, &d.path("p.wav"), r).unwrap();
    assert_eq!(notes["instrument"]["source"]["type"], "sf2");
    let entries = ferraus_synth::scan_sound_folder(&lib);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].category.as_deref(), Some("keys"));
    assert_eq!(entries[1].category.as_deref(), Some("strings"));
    // two string libraries: the name is ambiguous and the error lists both with hints
    write_sfz(&lib.join("More Strings"));
    let mut res = Resolver::discover(std::slice::from_ref(&lib));
    res.aliases.clear();
    let e = res.resolve("strings").unwrap_err();
    assert_eq!(e.code, "ambiguous_instrument");
    assert!(e.message.matches("hint:").count() >= 2, "{}", e.message);
    assert!(e.hint.unwrap().starts_with("ferraus sounds alias strings="));
}

#[test]
fn aliases_file_and_plugin_errors() {
    let d = TempDir::new("alias");
    let sf = d.path("font.sf2");
    write_sf2(&sf);
    let at = d.path("aliases.toml");
    std::fs::write(&at, format!("[aliases]\nmy-keys = \"sf2:{}@48\"\nlead = \"Trumpet\"\n", sf.display())).unwrap();
    let r = Resolver { aliases: ferraus_synth::read_aliases(&at), ..Default::default() };
    let x = r.resolve("my keys").unwrap();
    assert_eq!(x.via, "alias");
    assert_eq!(x.source, InstrumentSource::Sf2 { path: sf.clone(), preset: Some(Sf2Preset { bank: 0, program: 48 }) });
    let y = r.resolve("lead").unwrap();
    assert_eq!(y.program, Some(56));
    // a gm-basic soundfont in the sounds dir is used for GM names
    let sd = d.path("sounds");
    std::fs::create_dir_all(sd.join("gm-basic")).unwrap();
    std::fs::copy(&sf, sd.join("gm-basic/GeneralUser.sf2")).unwrap();
    let r2 = Resolver { gm_soundfont: ferraus_synth::source::find_gm_basic(&sd), ..Default::default() };
    assert!(matches!(
        r2.resolve("String Ensemble 1").unwrap().source,
        InstrumentSource::Sf2 { preset: Some(Sf2Preset { program: 48, .. }), .. }
    ));
    assert!(matches!(
        r2.resolve("drums").unwrap().source,
        InstrumentSource::Sf2 { preset: Some(Sf2Preset { bank: 128, .. }), .. }
    ));
    // plugin instruments are not available yet
    let midi = sparse_midi(120.0);
    let inp = write_midi(&d, "p.mid", &midi);
    let e = run_render(&[("instrument", "plugin:\"Surge XT\"")], &inp, &d.path("p.wav"), Run::default()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert_eq!(e.hint.as_deref(), Some("ferraus plugin scan"));
    // a missing explicit SoundFont is an input error
    let e = run_render(&[("instrument", "sf2:/nope/missing.sf2")], &inp, &d.path("m.wav"), Run::default()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::InputMissing);
}

#[test]
fn op_docs() {
    for o in ferraus_synth::ops() {
        assert!(!o.about.is_empty());
        assert!(o.examples.len() >= 2);
        assert!(o.params.iter().all(|p| !p.help.is_empty()));
    }
}
