//! Tests for `transcribe` and `hum`, including the PRD acceptance criteria.

use super::eval::{match_notes, EvalNote};
use super::fixtures::*;
use super::*;
use ferraus_core::testing::{self, TempDir};

const SR: u32 = 22050;

fn opts() -> TranscribeOptions {
    TranscribeOptions::default()
}

fn buf(x: Vec<f32>) -> AudioBuffer {
    AudioBuffer::from_mono(SR, x)
}

fn estimated(t: &Transcription) -> Vec<EvalNote> {
    t.notes.iter().map(|n| EvalNote { onset_s: n.start_s, offset_s: n.end_s, midi: n.pitch as f64 }).collect()
}

fn detail(fix: &[FixNote], t: &Transcription) -> String {
    let r: Vec<String> = fix.iter().map(|n| format!("{:.2}:{}", n.start, n.midi)).collect();
    let e: Vec<String> = t.notes.iter().map(|n| format!("{:.2}:{}", n.start_s, n.pitch)).collect();
    format!("ref {r:?}\nest {e:?}")
}

fn spec(name: &str) -> OpSpec {
    crate::ops().into_iter().find(|o| o.name == name).unwrap()
}

/// Run an op with files; returns (result, ctx).
fn run_op(name: &str, pairs: &[(&str, &str)], input: &Path, out: &Path) -> Result<(Value, OpCtx<'static>)> {
    let s = spec(name);
    let a = testing::args(&s, pairs);
    let mut c = testing::ctx();
    c.command = name.to_string();
    c.inputs = vec![input.to_path_buf()];
    c.globals.output = Some(out.display().to_string());
    let f = match s.runner {
        ferraus_core::op::Runner::Custom(f) => f,
        _ => unreachable!(),
    };
    let v = f(&a, &mut c)?;
    Ok((v, c))
}

// -------------------------------------------------------------------------------------
// acceptance
// -------------------------------------------------------------------------------------

#[test]
fn acceptance_sung_melodies_f_measure_at_least_95() {
    let cases: Vec<(&str, Vec<FixNote>, Voice)> = vec![
        ("A /a/ soprano", melody_a(0.0), Voice::sung_a()),
        ("A /u/ tenor", melody_a(-12.0), Voice::sung_u()),
        ("B /a/ alto", melody_b(0.0), Voice { seed: 11, ..Voice::sung_a() }),
        ("B /u/ baritone", melody_b(-12.0), Voice { seed: 12, ..Voice::sung_u() }),
        ("C /a/ soprano", melody_c(0.0), Voice { seed: 13, vib_cents: 45.0, ..Voice::sung_a() }),
        ("C /u/ tenor", melody_c(-12.0), Voice { seed: 14, ..Voice::sung_u() }),
    ];
    let mut total = (0usize, 0usize, 0usize);
    for (name, fix, voice) in cases {
        let x = sing(SR, &fix, &voice);
        let t = transcribe(&buf(x), &opts()).unwrap();
        let s = match_notes(&reference(&fix), &estimated(&t), 0.05, 1.0);
        eprintln!("sung {name}: F={:.3} P={:.3} R={:.3} ({} ref, {} est)", s.f_measure, s.precision, s.recall, s.reference, s.estimated);
        assert!(s.f_measure >= 0.95, "{name}: F = {:.3}\n{}", s.f_measure, detail(&fix, &t));
        total.0 += s.matched;
        total.1 += s.reference;
        total.2 += s.estimated;
    }
    let p = total.0 as f64 / total.2 as f64;
    let r = total.0 as f64 / total.1 as f64;
    eprintln!("sung overall: F={:.3}", 2.0 * p * r / (p + r));
}

#[test]
fn acceptance_hummed_melodies_f_measure_at_least_85() {
    let cases: Vec<(&str, Vec<FixNote>, Voice)> = vec![
        ("A hum high", melody_a(0.0), Voice::hummed()),
        ("A hum low", melody_a(-12.0), Voice { seed: 21, ..Voice::hummed() }),
        ("B hum", melody_b(0.0), Voice { seed: 22, ..Voice::hummed() }),
        ("B hum low", melody_b(-12.0), Voice { seed: 23, ..Voice::hummed() }),
        ("C hum", melody_c(0.0), Voice { seed: 24, ..Voice::hummed() }),
        ("C hum low", melody_c(-12.0), Voice { seed: 25, ..Voice::hummed() }),
    ];
    let mut total = (0usize, 0usize, 0usize);
    for (name, fix, voice) in cases {
        let x = sing(SR, &fix, &voice);
        let t = transcribe(&buf(x), &opts()).unwrap();
        let s = match_notes(&reference(&fix), &estimated(&t), 0.05, 1.0);
        eprintln!("hum {name}: F={:.3} P={:.3} R={:.3} ({} ref, {} est)", s.f_measure, s.precision, s.recall, s.reference, s.estimated);
        assert!(s.f_measure >= 0.85, "{name}: F = {:.3}\n{}", s.f_measure, detail(&fix, &t));
        total.0 += s.matched;
        total.1 += s.reference;
        total.2 += s.estimated;
    }
    let p = total.0 as f64 / total.2 as f64;
    let r = total.0 as f64 / total.1 as f64;
    eprintln!("hum overall: F={:.3}", 2.0 * p * r / (p + r));
}

/// Transcribe seeded random melodies and return the pooled F-measure.
fn pooled(label: &str, seeds: std::ops::Range<u64>, voice: &dyn Fn(u64) -> Voice, centre: &dyn Fn(u64) -> i32) -> f64 {
    let (mut m, mut r, mut e) = (0usize, 0usize, 0usize);
    for seed in seeds {
        let (fix, bpm) = random_melody(seed, centre(seed), 16);
        let x = sing(SR, &fix, &voice(seed));
        let t = transcribe(&buf(x), &opts()).unwrap();
        let s = match_notes(&reference(&fix), &estimated(&t), 0.05, 1.0);
        eprintln!("{label} seed {seed} ({bpm} BPM, centre {}): F={:.3} ({} ref, {} est, {} matched)", centre(seed), s.f_measure, s.reference, s.estimated, s.matched);
        if s.f_measure < 0.9 {
            eprintln!("{}", detail(&fix, &t));
        }
        m += s.matched;
        r += s.reference;
        e += s.estimated;
    }
    let p = m as f64 / e as f64;
    let rc = m as f64 / r as f64;
    let f = 2.0 * p * rc / (p + rc);
    eprintln!("{label} pooled: F={f:.3} P={p:.3} R={rc:.3} ({r} reference notes)");
    f
}

#[test]
fn acceptance_random_sung_melodies() {
    let f = pooled(
        "random sung",
        100..110,
        &|s| if s % 2 == 0 { Voice { seed: s, ..Voice::sung_a() } } else { Voice { seed: s, ..Voice::sung_u() } },
        &|s| if s % 3 == 0 { 50 } else { 64 },
    );
    assert!(f >= 0.95, "pooled F = {f:.3}");
}

#[test]
fn acceptance_random_hummed_melodies() {
    let f = pooled("random hum", 200..210, &|s| Voice { seed: s, ..Voice::hummed() }, &|s| if s % 2 == 0 { 50 } else { 62 });
    assert!(f >= 0.85, "pooled F = {f:.3}");
    // harsher than the PRD fixture: 100 ms onsets, ±30 cents drift, breath at −12 dB, room
    // noise at 20 dB SNR
    let harsh = |s: u64| Voice {
        seed: s,
        breath_db: Some(-12.0),
        room_db: Some(-20.0),
        drift_cents: 30.0,
        attack: 0.1,
        ..Voice::hummed()
    };
    let f = pooled("random harsh hum", 300..310, &harsh, &|s| if s % 2 == 0 { 50 } else { 62 });
    assert!(f >= 0.85, "pooled F = {f:.3}");
}

#[test]
fn tempo_auto_matches_the_fixture_tempo() {
    for (name, fix) in [("A", melody_a(0.0)), ("B", melody_b(-12.0)), ("C", melody_c(0.0))] {
        let x = sing(SR, &fix, &Voice::sung_a());
        let t = transcribe(&buf(x), &opts()).unwrap();
        eprintln!("tempo {name}: {:?}", t.tempo);
        assert!((t.tempo.bpm - 100.0).abs() <= 2.0, "{name}: {:?}", t.tempo);
        // the main tempo of the map is the detected one, and beats line up with the melody:
        // every note starts within 50 ms of an eighth-note position
        let last = t.midi.tempos.last().unwrap().data.us_per_quarter;
        assert!((60e6 / last as f64 - t.tempo.bpm).abs() < 0.01);
        for n in &t.notes {
            let pos = n.start_beats * 2.0;
            let err_beats = (pos - pos.round()).abs() / 2.0;
            assert!(err_beats * 0.6 < 0.05, "{name}: note at {} beats is off the eighth grid", n.start_beats);
        }
    }
}

// -------------------------------------------------------------------------------------
// parameters
// -------------------------------------------------------------------------------------

#[test]
fn quantize_puts_every_note_on_the_grid() {
    let fix = melody_a(0.0);
    let x = sing(SR, &fix, &Voice::sung_a());
    let o = TranscribeOptions { quantize: QuantizeSetting::Grid(Grid::parse("1/8").unwrap()), bpm: Some(100.0), ..opts() };
    let t = transcribe(&buf(x), &o).unwrap();
    assert!(t.quantize.is_some());
    let step = PPQ as u64 / 2;
    let notes = &t.midi.tracks[0].notes;
    assert!(notes.len() >= fix.len() - 1);
    for n in notes {
        assert_eq!(n.tick % step, 0, "note at tick {} is off the 1/8 grid", n.tick);
    }
    assert_eq!(t.tempo.source, "fixed");
    assert_eq!(t.tempo.bpm, 100.0);
}

/// C major line with the E sung 40 cents flat (63.6 → rounds to 64) and once 60 cents flat
/// (63.4 → rounds to Eb, outside C major).
fn c_major_with_flat_e() -> Vec<FixNote> {
    let mut fix = melody(
        100.0,
        0.3,
        &[
            (60.0, 1.0, 0.15, 0.0, 0.0),
            (62.0, 1.0, 0.15, 0.0, 0.0),
            (64.0, 1.0, 0.15, 0.0, 0.0),
            (65.0, 1.0, 0.15, 0.0, 0.0),
            (67.0, 1.0, 0.15, 0.0, 0.0),
            (69.0, 1.0, 0.15, 0.0, 0.0),
            (71.0, 1.0, 0.15, 0.0, 0.0),
            (72.0, 2.0, 0.15, 0.0, 0.0),
            (67.0, 1.0, 0.15, 0.0, 0.0),
            (64.0, 1.0, 0.15, 0.0, 0.0),
            (60.0, 2.0, 0.3, 0.0, 0.0),
            (64.0, 1.0, 0.15, 0.0, 0.0),
            (67.0, 1.0, 0.15, 0.0, 0.0),
            (60.0, 2.0, 0.3, 0.0, 0.0),
        ],
    );
    fix[9].detune_cents = -60.0;
    fix
}

#[test]
fn key_snap_moves_out_of_key_notes_when_confident() {
    let fix = c_major_with_flat_e();
    let x = sing(SR, &fix, &Voice { vib_cents: 0.0, ..Voice::sung_a() });
    let on = transcribe(&buf(x.clone()), &opts()).unwrap();
    let k = on.key.as_ref().unwrap();
    assert_eq!(k.key, "C major", "{k:?}");
    assert!(k.confidence > 0.6 && k.snapped, "{k:?}");
    let snapped: Vec<&TranscribedNote> = on.notes.iter().filter(|n| n.key_snapped).collect();
    assert_eq!(snapped.len(), 1, "{:?}", on.notes);
    assert_eq!(snapped[0].pitch, 64);
    assert!(on.notes.iter().all(|n| n.pitch != 63));
    assert_eq!(on.midi.key_signatures.len(), 1);
    assert_eq!(on.midi.key_signatures[0].data.sharps, 0);

    let off = transcribe(&buf(x), &TranscribeOptions { key_snap: false, ..opts() }).unwrap();
    assert_eq!(off.notes.iter().filter(|n| n.pitch == 63).count(), 1, "{:?}", off.notes);
    assert!(!off.key.as_ref().unwrap().snapped);
}

#[test]
fn pitch_bend_encodes_slides() {
    // C4 → D4 with a 150 ms portamento, then a separate E4
    let fix = melody(
        100.0,
        0.3,
        &[(60.0, 1.0, 0.0, 0.0, 0.0), (62.0, 1.0, 0.15, 0.0, 0.15), (64.0, 1.0, 0.2, 0.0, 0.0)],
    );
    let x = sing(SR, &fix, &Voice { vib_cents: 0.0, ..Voice::sung_a() });
    let off = transcribe(&buf(x.clone()), &TranscribeOptions { key_snap: false, ..opts() }).unwrap();
    let pitches: Vec<u8> = off.notes.iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, vec![60, 62, 64]);
    assert!(off.midi.tracks[0].pitch_bends.is_empty());
    assert_eq!(off.pitch_bends, 0);

    let on = transcribe(&buf(x), &TranscribeOptions { key_snap: false, pitch_bend: true, ..opts() }).unwrap();
    let pitches: Vec<u8> = on.notes.iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, vec![60, 64], "slide C4→D4 becomes one note with bends");
    let bends = &on.midi.tracks[0].pitch_bends;
    assert!(on.pitch_bends > 0 && !bends.is_empty());
    // the slide reaches +2 semitones = full-scale bend (8191) on the first note
    let first_end = on.midi.tracks[0].notes[0].end();
    let max = bends.iter().filter(|b| b.tick < first_end).map(|b| b.data.value).max().unwrap();
    assert!(max > 7800, "max bend {max}");
    // bends are reset before the next note and the bend range RPN is written
    assert!(bends.iter().any(|b| b.data.value == 0 && b.tick <= on.midi.tracks[0].notes[1].tick));
    assert!(on.midi.tracks[0].controllers.iter().any(|c| c.data.controller == 6 && c.data.value == 2));
}

#[test]
fn octave_fixed_and_auto() {
    let fix = melody_c(0.0);
    let x = sing(SR, &fix, &Voice::sung_a());
    let base = transcribe(&buf(x.clone()), &opts()).unwrap();
    assert_eq!(base.octave_shift, 0, "a soprano line is already in the piano range");
    let up = transcribe(&buf(x.clone()), &TranscribeOptions { octave: Octave::Shift(1), ..opts() }).unwrap();
    assert_eq!(up.octave_shift, 1);
    let a: Vec<u8> = base.notes.iter().map(|n| n.pitch + 12).collect();
    let b: Vec<u8> = up.notes.iter().map(|n| n.pitch).collect();
    assert_eq!(a, b);
    // a bass program moves the line down into G1..D3 (median ~67 → two octaves down)
    let bass = transcribe(&buf(x), &TranscribeOptions { program: 33, ..opts() }).unwrap();
    assert_eq!(bass.octave_shift, -2);
    let mut p: Vec<u8> = bass.notes.iter().map(|n| n.pitch).collect();
    p.sort();
    let median = p[p.len() / 2] as i32;
    let (lo, hi) = comfortable_range(33);
    assert!(median >= lo && median <= hi);
    assert_eq!(bass.midi.tracks[0].programs[0].data.program, 33);
}

#[test]
fn velocity_fixed_and_from_rms() {
    // one pitch, four levels (the formant filter would colour different pitches differently)
    let fix = melody(
        100.0,
        0.3,
        &[(64.0, 1.0, 0.2, 0.0, 0.0), (64.0, 1.0, 0.2, -12.0, 0.0), (64.0, 1.0, 0.2, -6.0, 0.0), (64.0, 1.0, 0.2, -20.0, 0.0)],
    );
    let x = sing(SR, &fix, &Voice::sung_a());
    let t = transcribe(&buf(x.clone()), &TranscribeOptions { key_snap: false, ..opts() }).unwrap();
    let v: Vec<u8> = t.notes.iter().map(|n| n.velocity).collect();
    assert_eq!(v.len(), 4);
    assert_eq!(v[0], 112, "{v:?}");
    assert!(v[0] > v[2] && v[2] > v[1] && v[1] > v[3], "{v:?}");
    // the curve: 112 × 10^(dB/40) → −6 dB 79, −12 dB 56, −20 dB 35
    for (got, want) in v.iter().zip([112, 56, 79, 35]) {
        assert!((*got as i32 - want).abs() <= 6, "{v:?}");
    }
    let f = transcribe(&buf(x), &TranscribeOptions { velocity: Velocity::Fixed(100), ..opts() }).unwrap();
    assert!(f.notes.iter().all(|n| n.velocity == 100));
    assert!(f.midi.tracks[0].notes.iter().all(|n| n.velocity == 100));
}

#[test]
fn min_note_and_onset_sensitivity() {
    // a 108 ms passing note between two longer notes
    let fix = melody(100.0, 0.3, &[(60.0, 1.0, 0.2, 0.0, 0.0), (62.0, 0.18, 0.0, 0.0, 0.0), (64.0, 1.0, 0.2, 0.0, 0.0)]);
    let x = sing(SR, &fix, &Voice { vib_cents: 0.0, ..Voice::sung_a() });
    let t = transcribe(&buf(x.clone()), &TranscribeOptions { key_snap: false, ..opts() }).unwrap();
    let p: Vec<u8> = t.notes.iter().map(|n| n.pitch).collect();
    assert_eq!(p, vec![60, 62, 64]);
    // --min-note 150ms merges it into a neighbour
    let t = transcribe(&buf(x.clone()), &TranscribeOptions { key_snap: false, min_note_s: 0.15, ..opts() }).unwrap();
    let p: Vec<u8> = t.notes.iter().map(|n| n.pitch).collect();
    assert_eq!(p, vec![60, 64], "{:?}", t.notes);
    // a repeated note with a shallow 6 dB dip splits only at high onset sensitivity
    let mut fix = melody(100.0, 0.3, &[(67.0, 1.0, 0.0, 0.0, 0.0), (67.0, 1.0, 0.2, -6.0, 0.0)]);
    fix[1].gain_db = 0.0;
    let mut y = sing(SR, &fix, &Voice { vib_cents: 0.0, ..Voice::sung_a() });
    let b = (fix[1].start * SR as f64) as usize;
    for (i, v) in y.iter_mut().enumerate() {
        let d = (i as f64 - b as f64).abs() / SR as f64;
        if d < 0.03 {
            *v *= 0.5; // −6 dB for 60 ms around the boundary
        }
    }
    let lo = transcribe(&buf(y.clone()), &TranscribeOptions { onset_sensitivity: 0.2, ..opts() }).unwrap();
    let hi = transcribe(&buf(y), &TranscribeOptions { onset_sensitivity: 1.0, ..opts() }).unwrap();
    assert_eq!(lo.notes.len(), 1, "{:?}", lo.notes);
    assert_eq!(hi.notes.len(), 2, "{:?}", hi.notes);
}

#[test]
fn option_parsing() {
    assert_eq!(Velocity::parse("fixed:100").unwrap(), Velocity::Fixed(100));
    assert_eq!(Velocity::parse("from-rms").unwrap(), Velocity::FromRms);
    assert!(Velocity::parse("fixed:0").is_err());
    assert!(Velocity::parse("loud").is_err());
    assert_eq!(Octave::parse("+1").unwrap(), Octave::Shift(1));
    assert_eq!(Octave::parse("-2").unwrap(), Octave::Shift(-2));
    assert_eq!(Octave::parse("auto").unwrap(), Octave::Auto);
    assert!(Octave::parse("9").is_err());
    let s = spec("transcribe");
    let c = testing::ctx();
    let o = options_from_args(&testing::args(&s, &[]), &c).unwrap();
    assert_eq!(o.mode, Mode::Auto);
    assert_eq!(o.bpm, None);
    assert_eq!(o.quantize, QuantizeSetting::Off);
    assert!(o.key_snap && !o.pitch_bend);
    assert!((o.min_note_s - 0.06).abs() < 1e-12);
    let o = options_from_args(&testing::args(&s, &[("bpm", "96"), ("quantize", "1/16"), ("velocity", "fixed:90")]), &c).unwrap();
    assert_eq!(o.bpm, Some(96.0));
    assert_eq!(o.quantize, QuantizeSetting::Grid(Grid::parse("1/16").unwrap()));
    assert_eq!(o.velocity, Velocity::Fixed(90));
    let o = options_from_args(&testing::args(&s, &[("quantize", "auto")]), &c).unwrap();
    assert_eq!(o.quantize, QuantizeSetting::Auto);
    assert!(options_from_args(&testing::args(&s, &[("bpm", "fast")]), &c).is_err());
    // help and examples
    assert!(s.examples.len() >= 2 && !s.about.is_empty());
    for p in s.effective_params() {
        assert!(!p.help.is_empty(), "{} has no help", p.name);
    }
    let h = spec("hum");
    assert!(h.examples.len() >= 2 && !h.about.is_empty());
}

// -------------------------------------------------------------------------------------
// modes
// -------------------------------------------------------------------------------------

/// Three harmonic tones at once (a C major triad), 2 s.
fn chord(sr: u32) -> Vec<f32> {
    let n = (sr as f64 * 2.0) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / sr as f64;
            let env = (t / 0.02).min(1.0) * ((2.0 - t) / 0.05).min(1.0);
            let mut s = 0.0;
            for m in [60.0, 64.0, 67.0] {
                let f = 440.0 * 2f64.powf((m - 69.0) / 12.0);
                for k in 1..8 {
                    s += (2.0 * std::f64::consts::PI * f * k as f64 * t).sin() / k as f64;
                }
            }
            (0.12 * env * s) as f32
        })
        .collect()
}

#[test]
fn auto_mode_detects_monophony_and_falls_back_on_chords() {
    let fix = melody_a(0.0);
    let t = transcribe(&buf(sing(SR, &fix, &Voice::sung_a())), &opts()).unwrap();
    let p = t.polyphony.as_ref().unwrap();
    eprintln!("sung: {p:?}");
    assert!(p.monophonic, "{p:?}");
    assert_eq!(t.mode, "mono");
    assert!(t.warnings.iter().all(|w| w.0 != "polyphonic_input"));

    let hum = transcribe(&buf(sing(SR, &melody_b(-12.0), &Voice::hummed())), &opts()).unwrap();
    let p = hum.polyphony.as_ref().unwrap();
    eprintln!("hummed: {p:?}");
    assert!(p.monophonic, "{p:?}");

    let c = transcribe(&buf(chord(SR)), &opts()).unwrap();
    let p = c.polyphony.as_ref().unwrap();
    eprintln!("chord: {p:?}");
    assert!(!p.monophonic, "{p:?}");
    assert_eq!(c.mode, "mono");
    let w = c.warnings.iter().find(|w| w.0 == "polyphonic_input").expect("fallback warning");
    assert!(w.1.contains("ferraus models pull transcribe/basic-pitch"), "{}", w.1);
}

#[test]
fn poly_mode_without_model_is_model_not_cached() {
    let e = transcribe(&buf(chord(SR)), &TranscribeOptions { mode: Mode::Poly, ..opts() }).err().unwrap();
    assert_eq!(e.kind, ErrorKind::ModelNotCached);
    assert_eq!(e.kind.exit_code(), 5);
    assert_eq!(e.hint.as_deref(), Some("ferraus models pull transcribe/basic-pitch"));
    let e = transcribe(&buf(chord(SR)), &TranscribeOptions { tracker: "crepe".into(), ..opts() }).err().unwrap();
    assert_eq!(e.kind, ErrorKind::ModelNotCached);
}

#[test]
fn silence_short_and_stereo_inputs() {
    let t = transcribe(&buf(vec![0.0; SR as usize]), &opts()).unwrap();
    assert!(t.notes.is_empty());
    assert!(t.warnings.iter().any(|w| w.0 == "no_notes"));
    let t = transcribe(&buf(vec![0.0; 10]), &opts()).unwrap();
    assert!(t.notes.is_empty());
    // stereo is mixed to mono
    let x = sing(SR, &melody_c(0.0), &Voice::sung_a());
    let st = AudioBuffer::from_planar(SR, vec![x.clone(), x]);
    let t = transcribe(&st, &opts()).unwrap();
    assert!(t.notes.len() >= 10);
}

// -------------------------------------------------------------------------------------
// the op
// -------------------------------------------------------------------------------------

#[test]
fn op_writes_midi_and_notes_sidecar() {
    let d = TempDir::new("transcribe-op");
    let input = d.path("hum.wav");
    let fix = melody_c(0.0);
    testing::write_wav(&input, &buf(sing(SR, &fix, &Voice::sung_a())));
    let out = d.path("part.mid");
    let (v, c) = run_op("transcribe", &[], &input, &out).unwrap();
    assert!(out.exists());
    let notes_path = d.path("part.notes.json");
    assert!(notes_path.exists());
    assert_eq!(v["output"], json!(out.display().to_string()));
    assert_eq!(v["notes_output"], json!(notes_path.display().to_string()));
    assert_eq!(v["mode"], "mono");
    let f = MidiFile::read(&out).unwrap();
    assert_eq!(f.note_count(), v["notes"].as_u64().unwrap() as usize);
    assert_eq!(f.tracks[0].programs[0].data.program, 0);
    let side: Value = serde_json::from_str(&std::fs::read_to_string(&notes_path).unwrap()).unwrap();
    let notes = side["notes"].as_array().unwrap();
    assert_eq!(notes.len(), f.note_count());
    for n in notes {
        let conf = n["confidence"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&conf) && conf > 0.3, "{n}");
        assert!(n["pitch"].as_u64().is_some() && n["name"].as_str().is_some());
    }
    assert!(c.notes.contains_key("bpm") && c.notes.contains_key("key"), "{:?}", c.notes);
    assert!(c.outputs.iter().any(|o| o.role.as_deref() == Some("notes")));
    // JSON output form
    let outj = d.path("part2.json");
    let (_, _) = run_op("transcribe", &[("velocity", "fixed:100"), ("bpm", "100")], &input, &outj).unwrap();
    let fj = MidiFile::read(&outj).unwrap();
    assert!(fj.tracks[0].notes.iter().all(|n| n.velocity == 100));
    assert!((60e6 / fj.tempos.last().unwrap().data.us_per_quarter as f64 - 100.0).abs() < 1e-6);
}

#[test]
fn op_dry_run_and_poly_error() {
    let d = TempDir::new("transcribe-dry");
    let input = d.path("in.wav");
    testing::write_wav(&input, &buf(chord(SR)));
    let s = spec("transcribe");
    let a = testing::args(&s, &[]);
    let mut c = testing::ctx();
    c.command = "transcribe".into();
    c.inputs = vec![input.clone()];
    c.globals.output = Some(d.path("x.mid").display().to_string());
    c.globals.dry_run = true;
    let f = match s.runner {
        ferraus_core::op::Runner::Custom(f) => f,
        _ => unreachable!(),
    };
    let v = f(&a, &mut c).unwrap();
    assert_eq!(v["dry_run"], true);
    assert!(!d.path("x.mid").exists());
    let e = run_op("transcribe", &[("mode", "poly")], &input, &d.path("p.mid")).err().unwrap();
    assert_eq!(e.kind, ErrorKind::ModelNotCached);
    assert!(!d.path("p.mid").exists());
}

#[test]
fn hum_without_render_says_so() {
    let d = TempDir::new("hum-norender");
    let input = d.path("voice.wav");
    testing::write_wav(&input, &buf(sing(SR, &melody_c(0.0), &Voice::sung_a())));
    let reg = testing::leak_registry(crate::ops());
    let s = spec("hum");
    let a = testing::args(&s, &[]);
    let mut c = testing::ctx_with(reg);
    c.command = "hum".into();
    c.inputs = vec![input];
    c.globals.output = Some(d.path("idea.wav").display().to_string());
    let f = match s.runner {
        ferraus_core::op::Runner::Custom(f) => f,
        _ => unreachable!(),
    };
    let e = f(&a, &mut c).err().unwrap();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert!(e.message.contains("'render'"), "{}", e.message);
    assert!(!d.path("idea.mid").exists());
}
