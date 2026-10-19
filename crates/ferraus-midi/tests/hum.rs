//! End-to-end `hum` and `transcribe` through the executor, with a registry holding the
//! MIDI ops and the `render` op from ferraus-synth (a dev-dependency only).

use ferraus_core::exec::{Invocation, Runtime};
use ferraus_core::testing::{self, TempDir};
use ferraus_core::{AudioBuffer, Globals, Registry};
use ferraus_midi::MidiFile;
use std::f64::consts::PI;
use std::path::Path;

const SR: u32 = 22050;

/// A sung-like tone sequence: (midi, start_s, dur_s) with harmonics and a vowel-ish tilt.
fn voice(notes: &[(f64, f64, f64)]) -> AudioBuffer {
    let end = notes.iter().map(|n| n.1 + n.2).fold(0.0, f64::max) + 0.5;
    let mut x = vec![0.0f32; (end * SR as f64) as usize];
    for &(m, s, d) in notes {
        let f = 440.0 * 2f64.powf((m - 69.0) / 12.0);
        let (a, b) = ((s * SR as f64) as usize, ((s + d) * SR as f64) as usize);
        let mut phase = 0.0f64;
        for (k, v) in x[a..b].iter_mut().enumerate() {
            let t = k as f64 / SR as f64;
            let env = (t / 0.02).min(1.0) * ((d - t) / 0.03).clamp(0.0, 1.0);
            // ±20 cents of vibrato after 300 ms
            let vib = 2f64.powf(0.2 / 12.0 * (2.0 * PI * 5.5 * t).sin() * (t / 0.3).min(1.0));
            phase += 2.0 * PI * f * vib / SR as f64;
            let mut acc = 0.0;
            for h in 1..12 {
                let hf = h as f64;
                acc += (phase * hf).sin() / hf.powf(1.2) * if h == 2 || h == 3 { 1.6 } else { 1.0 };
            }
            *v = (0.15 * env * acc) as f32;
        }
    }
    AudioBuffer::from_mono(SR, x)
}

fn registry() -> &'static Registry {
    let mut ops = ferraus_midi::ops();
    ops.extend(ferraus_synth::ops());
    testing::leak_registry(ops)
}

fn invoke(reg: &Registry, command: &str, args: &[(&str, &str)], input: &Path, out: &Path) -> ferraus_core::Envelope {
    let rt = Runtime {
        registry: reg,
        io: &testing::SIMPLE_IO,
        progress: &testing::NO_PROGRESS,
        presets: &|_, _| None,
        cancel: Default::default(),
    };
    let globals = Globals { output: Some(out.display().to_string()), sidecar: false, ..Globals::default() };
    let inv = Invocation {
        command: command.into(),
        args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        inputs: vec![input.display().to_string()],
        globals,
        argv: vec![],
        stdin: None,
    };
    rt.invoke(inv).envelope
}

/// C D E G at 100 BPM (quarters, sung slightly early and late), then a held C.
fn melody() -> Vec<(f64, f64, f64)> {
    let beat = 0.6;
    let jitter = [0.0, 0.025, -0.02, 0.015, -0.01, 0.02, 0.0, -0.015];
    let pitches = [60.0, 62.0, 64.0, 67.0, 64.0, 62.0, 60.0, 60.0];
    (0..8).map(|i| (pitches[i], 0.6 + i as f64 * beat + jitter[i], if i == 7 { 1.0 } else { 0.48 })).collect()
}

#[test]
fn hum_end_to_end_writes_mid_and_wav() {
    let d = TempDir::new("hum-e2e");
    let input = d.path("voice.wav");
    testing::write_wav(&input, &voice(&melody()));
    let out = d.path("idea.wav");
    let reg = registry();
    let env = invoke(reg, "hum", &[("to", "piano")], &input, &out);
    assert!(env.ok, "{:?}", env.error);
    let mid = d.path("idea.mid");
    assert!(out.exists() && mid.exists());
    assert!(env.outputs.iter().any(|o| o.role.as_deref() == Some("midi") && o.path.ends_with("idea.mid")));
    assert!(env.outputs.iter().any(|o| o.path.ends_with("idea.wav")));
    let r = &env.result;
    assert_eq!(r["midi"], serde_json::json!(mid.display().to_string()));
    assert_eq!(r["audio"], serde_json::json!(out.display().to_string()));
    assert_eq!(r["notes"], 8, "{r}");
    assert!((r["bpm"].as_f64().unwrap() - 100.0).abs() < 2.0, "{r}");
    assert!(r["key"].is_string(), "{r}");
    assert_eq!(r["quantize_grid"], "1/16");

    // the part is editable MIDI with the expected notes, quantized 80% toward the grid
    let f = MidiFile::read(&mid).unwrap();
    let notes: Vec<u8> = f.tracks[0].notes.iter().map(|n| n.pitch).collect();
    assert_eq!(notes, vec![60, 62, 64, 67, 64, 62, 60, 60]);
    let step = 960.0 / 4.0;
    for n in &f.tracks[0].notes {
        let off = (n.tick as f64 / step - (n.tick as f64 / step).round()).abs() * step;
        // 25 ms of jitter is ~40 ticks at 100 BPM / PPQ 960; 80% strength leaves ≤ 20%
        assert!(off <= 0.2 * 60.0 + 2.0, "tick {} is {off} ticks off the grid", n.tick);
    }
    // the audio covers the MIDI plus the 2 s tail
    let wav = testing::read_wav(&out);
    let want = f.duration_seconds() + 2.0;
    assert!((wav.duration_s() - want).abs() < 0.01, "{} vs {want}", wav.duration_s());
    assert!(wav.peak() > 0.01, "rendered audio is silent");
}

#[test]
fn hum_options_and_rerender() {
    let d = TempDir::new("hum-opts");
    let input = d.path("voice.wav");
    testing::write_wav(&input, &voice(&melody()));
    let reg = registry();
    let out = d.path("strings.wav");
    let env =
        invoke(reg, "hum", &[("to", "strings"), ("grid", "1/8"), ("strength", "100%"), ("bpm", "100")], &input, &out);
    assert!(env.ok, "{:?}", env.error);
    let f = MidiFile::read(&d.path("strings.mid")).unwrap();
    assert!(f.tracks[0].notes.iter().all(|n| n.tick % 480 == 0));
    // --to strings writes the GM strings program into the part
    assert_eq!(f.tracks[0].programs[0].data.program, 48);
    // the intermediate part re-renders with another instrument
    let again = d.path("again.wav");
    let env = invoke(reg, "render", &[("instrument", "organ")], &d.path("strings.mid"), &again);
    assert!(env.ok && again.exists(), "{:?}", env.error);
}

#[test]
fn transcribe_through_the_executor() {
    let d = TempDir::new("transcribe-e2e");
    let input = d.path("voice.wav");
    testing::write_wav(&input, &voice(&melody()));
    let out = d.path("part.mid");
    let env = invoke(registry(), "transcribe", &[("quantize", "1/16")], &input, &out);
    assert!(env.ok, "{:?}", env.error);
    assert!(out.exists() && d.path("part.notes.json").exists());
    assert_eq!(env.result["notes"], 8);
    assert_eq!(env.params["mode"], "mono", "{:?}", env.params);
    assert!(env.params.contains_key("bpm"));
}
