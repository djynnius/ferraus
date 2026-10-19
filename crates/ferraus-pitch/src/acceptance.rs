//! Op-level tests, including the PRD acceptance criteria for pitch and time.

use crate::fixtures::*;
use crate::notes::{segment, NoteOptions};
use crate::pyin::{track, PyinOptions};
use ferraus_core::exec::apply_spec;
use ferraus_core::testing::{self, TempDir};
use ferraus_core::{AudioBuffer, ErrorKind, OpCtx, OpSpec};
use serde_json::Value;

fn spec(name: &str) -> OpSpec {
    crate::ops().into_iter().find(|o| o.name == name).unwrap()
}

fn run_ctx(name: &str, pairs: &[(&str, &str)], buf: AudioBuffer) -> (AudioBuffer, OpCtx<'static>) {
    let s = spec(name);
    let a = testing::args(&s, pairs);
    let mut c = testing::ctx();
    let out = apply_spec(&s, &a, buf, &mut c).expect("op ran");
    (out, c)
}

/// (start_s, end_s, median midi) of the notes found in `y`.
fn note_medians(y: &[f32], sr: u32) -> Vec<(f64, f64, f64)> {
    let fr = track(y, sr, &PyinOptions::default());
    segment(&fr, &NoteOptions::default()).into_iter().map(|n| (n.start_s, n.end_s, n.midi)).collect()
}

/// Like [`note_medians`], but the median skips each note's first 100 ms (the onset glide
/// that `--retune-speed` deliberately leaves in) and last 20 ms.
fn steady_note_medians(y: &[f32], sr: u32) -> Vec<(f64, f64, f64)> {
    let fr = track(y, sr, &PyinOptions::default());
    segment(&fr, &NoteOptions::default())
        .into_iter()
        .map(|n| {
            let v: Vec<f64> = fr[n.start_frame..n.end_frame]
                .iter()
                .filter(|f| f.voiced && f.time_s >= n.start_s + 0.1 && f.time_s <= n.end_s - 0.02)
                .map(|f| ferraus_core::units::hz_to_midi(f.freq_hz))
                .collect();
            (n.start_s, n.end_s, crate::util::median(&v).unwrap_or(n.midi))
        })
        .collect()
}

const MELODY: [f64; 8] = [60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 67.0, 64.0];

fn detuned_vocal(sr: u32) -> Vec<f32> {
    let notes: Vec<(f64, f64)> = MELODY
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let det = [0.4, -0.4, -0.4, 0.4, -0.4, 0.4, -0.4, 0.4][i];
            (midi_hz(m + det), 0.35)
        })
        .collect();
    vocal(sr, &notes, Vowel::A, 0.0, 0.0)
}

fn residuals(y: &[f32], sr: u32) -> Vec<f64> {
    let got = steady_note_medians(y, sr);
    assert_eq!(got.len(), MELODY.len(), "notes found: {got:?}");
    got.iter().zip(MELODY).map(|(g, m)| (g.2 - m) * 100.0).collect()
}

#[test]
fn tune_full_strength_within_5_cents() {
    let sr = 22050;
    let x = detuned_vocal(sr);
    let before = residuals(&x, sr);
    assert!(before.iter().all(|r| (r.abs() - 40.0).abs() < 3.0), "{before:?}");
    let (y, c) = run_ctx("tune", &[("key", "C"), ("scale", "major")], AudioBuffer::from_mono(sr, x));
    assert_eq!(c.metrics["resynthesis"], "td_psola");
    let after = residuals(&y.data[0], sr);
    let worst = after.iter().fold(0.0f64, |m, r| m.max(r.abs()));
    eprintln!("tune 100%: residual cents {after:.2?} (worst {worst:.2})");
    assert!(worst <= 5.0, "{after:?}");
}

#[test]
fn tune_half_strength_halves_the_error() {
    let sr = 22050;
    let x = detuned_vocal(sr);
    let (y, _) =
        run_ctx("tune", &[("key", "C"), ("scale", "major"), ("strength", "50%")], AudioBuffer::from_mono(sr, x));
    let after = residuals(&y.data[0], sr);
    let worst = after.iter().fold(0.0f64, |m, r| m.max(r.abs()));
    eprintln!("tune 50%: residual cents {after:.2?} (worst {worst:.2})");
    // strength is linear, so a 40-cent error is left at exactly 20 cents: the PRD bound
    // is met up to the ±0.5 cent measurement error of the tracker
    assert!(worst <= 20.5, "{after:?}");
    assert!(after.iter().all(|r| r.abs() >= 19.0), "{after:?}");
}

#[test]
fn tune_detects_key_and_hard_effect() {
    let sr = 22050;
    let x = detuned_vocal(sr);
    let (y, c) = run_ctx("tune", &[("effect", "hard")], AudioBuffer::from_mono(sr, x));
    let key = c.notes["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("C major") || key.contains("not used"), "{key}");
    let after = residuals(&y.data[0], sr);
    assert!(after.iter().all(|r| r.abs() <= 5.0), "{after:?} ({key})");
}

#[test]
fn low_key_confidence_falls_back_to_chromatic() {
    let sr = 16000;
    // a chromatic run: no key fits
    let notes: Vec<(f64, f64)> = (0..12).map(|i| (midi_hz(60.0 + i as f64 + 0.3), 0.2)).collect();
    let x = vocal(sr, &notes, Vowel::A, 0.0, 0.0);
    let (y, c) = run_ctx("tune", &[], AudioBuffer::from_mono(sr, x));
    assert!(c.warnings.iter().any(|w| w.code == "low_key_confidence"), "{:?}", c.warnings);
    assert_eq!(c.notes["scale"], "chromatic");
    assert!(c.metrics["key_fallback_reason"].as_str().unwrap().contains("below 0.6"));
    let got = note_medians(&y.data[0], sr);
    for (i, g) in got.iter().enumerate() {
        let r = (g.2 - g.2.round()) * 100.0;
        assert!(r.abs() < 6.0, "note {i}: {g:?}");
    }
}

#[test]
fn formants_move_less_than_3_percent_after_4_semitones() {
    let sr = 16000;
    let x = vocal(sr, &[(110.0, 1.0)], Vowel::A, 5.5, 50.0);
    let (y, c) = run_ctx("pitch", &[("semitones", "4")], AudioBuffer::from_mono(sr, x.clone()));
    // the shift really happened
    let f_in = note_medians(&x, sr)[0].2;
    let f_out = note_medians(&y.data[0], sr)[0].2;
    assert!(((f_out - f_in) * 100.0 - 400.0).abs() < 5.0, "{f_in} → {f_out}");
    let a = lpc_formants(&x[2000..x.len() - 2000], sr, 14);
    let b = lpc_formants(&y.data[0][2000..x.len() - 2000], sr, 14);
    let dev: Vec<f64> = (0..3).map(|i| 100.0 * (b[i] / a[i] - 1.0)).collect();
    eprintln!("formants before {:?} after {:?} change {dev:.2?} % ({})", &a[..3], &b[..3], c.metrics["method"]);
    assert!(dev.iter().all(|d| d.abs() < 3.0), "{dev:?}");
    // --formant shift moves them
    let (z, _) = run_ctx("pitch", &[("semitones", "4"), ("formant", "shift")], AudioBuffer::from_mono(sr, x.clone()));
    let s = lpc_formants(&z.data[0][2000..x.len() - 2000], sr, 14);
    assert!(s[0] / a[0] > 1.12, "F1 {} vs {}", s[0], a[0]);
}

/// Amplitude (cents) of the 6 Hz component of a pitch track, by least squares.
fn vibrato_amp(y: &[f32], sr: u32, t0: f64, t1: f64) -> f64 {
    let fr = track(y, sr, &PyinOptions::default());
    let pts: Vec<(f64, f64)> = fr
        .iter()
        .filter(|f| f.voiced && f.time_s >= t0 && f.time_s <= t1)
        .map(|f| (f.time_s, ferraus_core::units::hz_to_midi(f.freq_hz) * 100.0))
        .collect();
    let mean = pts.iter().map(|p| p.1).sum::<f64>() / pts.len() as f64;
    let w = 2.0 * std::f64::consts::PI * 6.0;
    let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (t, v) in &pts {
        let (s, c) = ((w * t).sin(), (w * t).cos());
        ss += s * s;
        sc += s * c;
        cc += c * c;
        ys += (v - mean) * s;
        yc += (v - mean) * c;
    }
    let det = ss * cc - sc * sc;
    let a = (ys * cc - yc * sc) / det;
    let b = (yc * ss - ys * sc) / det;
    (a * a + b * b).sqrt()
}

#[test]
fn vibrato_survives_correction() {
    let sr = 22050;
    let x = vocal(sr, &[(midi_hz(57.3), 1.6)], Vowel::A, 6.0, 40.0);
    let before = vibrato_amp(&x, sr, 0.3, 1.3);
    let (y, _) = run_ctx("tune", &[("key", "A"), ("scale", "minor")], AudioBuffer::from_mono(sr, x.clone()));
    let after = vibrato_amp(&y.data[0], sr, 0.3, 1.3);
    let centre = note_medians(&y.data[0], sr)[0].2;
    eprintln!("vibrato amplitude {before:.1} → {after:.1} cents; centre {centre:.3}");
    assert!((before - 40.0).abs() < 4.0);
    assert!((after / before - 1.0).abs() <= 0.2, "{before} → {after}");
    assert!((centre - 57.0).abs() * 100.0 < 5.0, "{centre}");
    // without preservation a 25 ms retune flattens part of it
    let (z, _) = run_ctx("tune", &[("key", "A"), ("vibrato-preserve", "off")], AudioBuffer::from_mono(sr, x));
    assert!(vibrato_amp(&z.data[0], sr, 0.3, 1.3) < 0.85 * before);
}

#[test]
fn tune_and_pitch_null_and_edge_cases() {
    let sr = 16000;
    let x = AudioBuffer::from_mono(sr, vocal(sr, &[(200.0, 0.5)], Vowel::A, 0.0, 0.0));
    for op in ["tune", "pitch"] {
        let y = testing::run(
            &spec(op),
            &[("mix", "0%"), ("semitones", "3")]
                .iter()
                .filter(|p| op == "pitch" || p.0 == "mix")
                .cloned()
                .collect::<Vec<_>>(),
            x.clone(),
        )
        .unwrap();
        assert_eq!(y, x, "{op} mix=0% must be the identity");
        // silence and a very short input are returned unchanged
        let s = AudioBuffer::new(sr, 1, 8000);
        assert_eq!(testing::run(&spec(op), &[], s.clone()).unwrap(), s);
        let short = AudioBuffer::from_mono(sr, vec![0.1; 10]);
        let o = testing::run(&spec(op), &[], short.clone()).unwrap();
        assert_eq!(o.frames(), 10);
    }
    // pitch 0 is exact; stereo keeps channels and length
    assert_eq!(testing::run(&spec("pitch"), &[], x.clone()).unwrap(), x);
    let st = testing::stereo(x.clone());
    let y = testing::run(&spec("pitch"), &[("semitones", "-2")], st.clone()).unwrap();
    assert_eq!((y.channels(), y.frames()), (2, st.frames()));
    let y = testing::run(&spec("tune"), &[("key", "G")], st.clone()).unwrap();
    assert_eq!((y.channels(), y.frames()), (2, st.frames()));
}

#[test]
fn model_tracker_needs_model() {
    let sr = 16000;
    let x = AudioBuffer::from_mono(sr, vocal(sr, &[(200.0, 0.3)], Vowel::A, 0.0, 0.0));
    let e = testing::run(&spec("tune"), &[("tracker", "crepe")], x).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ModelNotCached);
    assert_eq!(e.hint.as_deref(), Some("ferraus models pull pitch/crepe"));
}

#[test]
fn tune_to_midi_target_and_notes_out() {
    use midly::num::{u15, u24, u28, u4, u7};
    use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
    let sr = 16000;
    // sung: 60.3, 62.4, 66.0 ; written: 60, 62, 64 (third note is 200 cents away)
    let x = vocal(sr, &[(midi_hz(60.3), 0.4), (midi_hz(62.4), 0.4), (midi_hz(66.0), 0.4)], Vowel::A, 0.0, 0.0);
    let ev = |d: u32, kind| TrackEvent { delta: u28::new(d), kind };
    let note = |on: bool, k: u8| TrackEventKind::Midi {
        channel: u4::new(0),
        message: if on {
            MidiMessage::NoteOn { key: u7::new(k), vel: u7::new(90) }
        } else {
            MidiMessage::NoteOff { key: u7::new(k), vel: u7::new(0) }
        },
    };
    // 150 bpm at 480 ppq: 0.4 s = 480 ticks
    let track = vec![
        ev(0, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(400_000)))),
        ev(0, note(true, 60)),
        ev(480, note(false, 60)),
        ev(0, note(true, 62)),
        ev(480, note(false, 62)),
        ev(0, note(true, 64)),
        ev(480, note(false, 64)),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];
    let smf = Smf { header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))), tracks: vec![track] };
    let dir = TempDir::new("tune-target");
    let mid = dir.path("melody.mid");
    let mut bytes = Vec::new();
    smf.write(&mut bytes).unwrap();
    std::fs::write(&mid, &bytes).unwrap();
    let notes_json = dir.path("notes.json");
    let (y, c) = run_ctx(
        "tune",
        &[("target", mid.to_str().unwrap()), ("notes-out", notes_json.to_str().unwrap())],
        AudioBuffer::from_mono(sr, x),
    );
    let left = c.metrics["left_alone"].as_array().unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(c.warnings.iter().any(|w| w.code == "notes_left_alone"));
    let got = note_medians(&y.data[0], sr);
    assert!((got[0].2 - 60.0).abs() < 0.05 && (got[1].2 - 62.0).abs() < 0.05, "{got:?}");
    assert!((got[2].2 - 66.0).abs() < 0.05, "left alone: {got:?}");
    let doc: Value = serde_json::from_slice(&std::fs::read(&notes_json).unwrap()).unwrap();
    assert_eq!(doc["notes"].as_array().unwrap().len(), 3);
    assert!(c.outputs.iter().any(|o| o.role.as_deref() == Some("notes")));
}

#[test]
fn stretch_op_identity_click_track_and_targets() {
    let sr = 44100;
    let x = testing::clicks(sr, 0.25, 2.0, 0.9);
    // factor 1.0 is the bit-exact identity
    let y = testing::run(&spec("stretch"), &[], x.clone()).unwrap();
    assert_eq!(y, x);
    let y = testing::run(&spec("stretch"), &[("factor", "1.0"), ("mode", "tonal")], x.clone()).unwrap();
    assert_eq!(y, x);
    // factor 2.0: every click within 1 ms of 2 × its time
    let (y, c) = run_ctx("stretch", &[("factor", "2.0")], x.clone());
    assert_eq!(y.frames(), x.frames() * 2);
    let mut found = Vec::new();
    let d = &y.data[0];
    let mut i = 0;
    while i < d.len() {
        if d[i].abs() > 0.3 {
            let end = (i + 200).min(d.len());
            let p = (i..end).max_by(|a, b| d[*a].abs().total_cmp(&d[*b].abs())).unwrap();
            found.push(p);
            i = end;
        } else {
            i += 1;
        }
    }
    let expected: Vec<usize> = (0..8).map(|k| k * 2 * 11025).collect();
    assert_eq!(found.len(), expected.len(), "{found:?} {:?}", c.metrics);
    for (f, e) in found.iter().zip(&expected) {
        assert!((*f as f64 - *e as f64).abs() <= 0.001 * sr as f64, "click at {f}, want {e}");
    }
    // --to and --bpm/--to-bpm
    let tone = AudioBuffer::from_mono(16000, sine_wave(16000, 330.0, 1.0, 0.4));
    let y = testing::run(&spec("stretch"), &[("to", "1.5s")], tone.clone()).unwrap();
    assert_eq!(y.frames(), 24000);
    let y = testing::run(&spec("stretch"), &[("bpm", "120"), ("to-bpm", "100")], tone.clone()).unwrap();
    assert_eq!(y.frames(), 19200);
    let e = testing::run(&spec("stretch"), &[("factor", "2"), ("to", "3s")], tone).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
}

#[test]
fn stretch_to_bpm_estimates_source_tempo() {
    let sr = 22050;
    let x = testing::clicks(sr, 0.5, 6.0, 0.8); // 120 BPM
    let (y, c) = run_ctx("stretch", &[("to-bpm", "60"), ("mode", "percussive")], x.clone());
    assert!(c.warnings.iter().any(|w| w.code == "bpm_estimated"));
    let ratio = y.frames() as f64 / x.frames() as f64;
    assert!((ratio - 2.0).abs() < 0.04, "{ratio}");
}

fn custom_ctx(cmd: &str, input: &std::path::Path, output: Option<String>) -> OpCtx<'static> {
    let mut c = testing::ctx();
    c.command = cmd.to_string();
    c.inputs = vec![input.to_path_buf()];
    c.globals.output = output;
    c
}

fn run_custom(name: &str, pairs: &[(&str, &str)], c: &mut OpCtx) -> ferraus_core::Result<Value> {
    let s = spec(name);
    let a = testing::args(&s, pairs);
    match s.runner {
        ferraus_core::Runner::Custom(f) => f(&a, c),
        _ => unreachable!(),
    }
}

#[test]
fn analyze_pitch_json_and_csv() {
    let sr = 16000;
    let dir = TempDir::new("analyze-pitch");
    let wav = dir.path("v.wav");
    let x = vocal(sr, &[(midi_hz(64.0), 0.4), (midi_hz(67.0), 0.4)], Vowel::A, 0.0, 0.0);
    testing::write_wav(&wav, &AudioBuffer::from_mono(sr, x));
    let mut c = custom_ctx("analyze pitch", &wav, None);
    let v = run_custom("analyze pitch", &[], &mut c).unwrap();
    assert_eq!(v["notes"].as_array().unwrap().len(), 2);
    assert_eq!(v["notes"][0]["note"], "E4", "{}", v["notes"]);
    assert!(v["frames"].as_array().unwrap().len() >= 79);
    assert!(v["summary"]["voiced_fraction"].as_f64().unwrap() > 0.9, "{}", v["summary"]);
    let csv = dir.path("curve.csv");
    let mut c = custom_ctx("analyze pitch", &wav, Some(csv.display().to_string()));
    let v = run_custom("analyze pitch", &[("hop", "5ms")], &mut c).unwrap();
    assert_eq!(v["format"], "csv");
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.starts_with("time_s,freq_hz,midi,confidence,voiced,rms\n"));
    assert!(text.lines().count() > 150);
    // model tracker → error with hint
    let mut c = custom_ctx("analyze pitch", &wav, None);
    let e = run_custom("analyze pitch", &[("tracker", "rmvpe")], &mut c);
    assert!(e.is_err());
}

#[test]
fn analyze_key_reports_alternatives() {
    let sr = 11025;
    let dir = TempDir::new("analyze-key");
    let wav = dir.path("m.wav");
    // D major melody
    let mel = [62.0, 64.0, 66.0, 67.0, 69.0, 71.0, 73.0, 74.0, 69.0, 66.0, 62.0];
    let notes: Vec<(f64, f64)> = mel.iter().map(|m| (midi_hz(*m), 0.25)).collect();
    testing::write_wav(&wav, &AudioBuffer::from_mono(sr, vocal(sr, &notes, Vowel::U, 0.0, 0.0)));
    for method in ["chroma", "notes"] {
        let mut c = custom_ctx("analyze key", &wav, None);
        let v = run_custom("analyze key", &[("method", method)], &mut c).unwrap();
        assert_eq!(v["key"], "D major", "{method}: {v}");
        assert_eq!(v["alternatives"].as_array().unwrap().len(), 3);
        assert!(v["confidence"].as_f64().unwrap() > 0.0);
    }
}

#[test]
fn every_op_is_documented() {
    let ops = crate::ops();
    assert_eq!(ops.len(), 5);
    for o in &ops {
        assert!(!o.about.is_empty(), "{}", o.name);
        assert!(o.examples.len() >= 2, "{}", o.name);
        for p in &o.params {
            assert!(!p.help.is_empty(), "{} --{}", o.name, p.name);
        }
    }
    assert!(!spec("stretch").auto_mix);
    assert!(spec("tune").auto_mix && spec("pitch").auto_mix);
}

/// Release benchmark: `cargo test -p ferraus-pitch --release -- --ignored --nocapture bench`
#[test]
#[ignore]
fn bench_tune_60s() {
    let sr = 44100;
    let mut notes = Vec::new();
    let scale = [60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0];
    for i in 0..150 {
        let det = ((i * 37) % 81) as f64 / 100.0 - 0.4;
        notes.push((midi_hz(scale[i % 8] + det - 12.0), 0.4));
    }
    let x = vocal(sr, &notes, Vowel::A, 5.5, 30.0);
    let secs = x.len() as f64 / sr as f64;
    let buf = AudioBuffer::from_mono(sr, x.clone());
    let t = std::time::Instant::now();
    let fr = track(&x, sr, &PyinOptions::default());
    let t_track = t.elapsed().as_secs_f64();
    let _ = fr;
    let t = std::time::Instant::now();
    let (_y, c) = run_ctx("tune", &[("key", "C")], buf.clone());
    let t_tune = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let (_y2, _) = run_ctx("tune", &[("key", "C"), ("formant", "shift")], buf.clone());
    let t_pv = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let _ = testing::run(&spec("pitch"), &[("semitones", "3")], buf.clone()).unwrap();
    let t_pitch = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let _ = testing::run(&spec("stretch"), &[("factor", "1.25")], buf).unwrap();
    let t_stretch = t.elapsed().as_secs_f64();
    eprintln!(
        "{secs:.0} s @ {sr} Hz: pyin {t_track:.2}s; tune (psola, {}) {t_tune:.2}s → 4 min ≈ {:.1}s; tune via phase vocoder {t_pv:.2}s → 4 min ≈ {:.1}s; pitch +3 st {t_pitch:.2}s; stretch 1.25 {t_stretch:.2}s",
        c.metrics["resynthesis"],
        t_tune * 240.0 / secs,
        t_pv * 240.0 / secs
    );
    assert!(t_tune * 240.0 / secs < 60.0);
}
