//! The polyphonic seam: a fake Basic Pitch transcriber registered the way `ferraus-ml`
//! registers the real one. (Its own test binary: registration is process-wide.)

use ferraus_core::exec::{Invocation, Runtime};
use ferraus_core::testing::{self, TempDir};
use ferraus_core::{AudioBuffer, Globals, Result};
use ferraus_midi::transcribe::poly::{
    self, register_poly_transcriber, Activation, PolyActivations, PolyTranscriber, N_CONTOUR_BINS, N_PITCH_BINS,
};
use ferraus_midi::transcribe::{transcribe, Mode, TranscribeOptions};
use ferraus_midi::MidiFile;
use std::f64::consts::PI;

struct FakeBasicPitch;

impl PolyTranscriber for FakeBasicPitch {
    fn name(&self) -> &str {
        "basic-pitch"
    }
    fn model(&self) -> Option<(String, String, String)> {
        Some(("transcribe/basic-pitch".into(), "test".into(), "0".repeat(64)))
    }
    /// C major triad from 0.5 s to 1.5 s, then G4 from 1.6 s to 2.2 s, whatever the audio.
    fn activations(&self, samples: &[f32], sr: u32) -> Result<PolyActivations> {
        let secs = samples.len() as f64 / sr as f64;
        let fps = poly::frames_per_second();
        let n = (secs * fps).ceil() as usize + 1;
        let mut notes = Activation::zeros(n, N_PITCH_BINS);
        let mut onsets = Activation::zeros(n, N_PITCH_BINS);
        let mut put = |pitch: u8, s: f64, e: f64, level: f32| {
            let b = (pitch - poly::MIDI_OFFSET) as usize;
            let (fs, fe) = ((s * fps).round() as usize, (e * fps).round() as usize);
            for t in fs..fe.min(n) {
                notes.set(t, b, level);
            }
            onsets.set(fs, b, 0.9);
            onsets.set(fs - 1, b, 0.3);
            onsets.set(fs + 1, b, 0.3);
        };
        put(60, 0.5, 1.5, 0.7);
        put(64, 0.5, 1.5, 0.6);
        put(67, 0.5, 1.5, 0.5);
        put(67, 1.6, 2.2, 0.8);
        Ok(PolyActivations { notes, onsets, contours: Some(Activation::zeros(n, N_CONTOUR_BINS)), windowed: false })
    }
}

fn make() -> Result<Box<dyn PolyTranscriber>> {
    Ok(Box::new(FakeBasicPitch))
}

fn chord_audio() -> AudioBuffer {
    let sr = 22050;
    let n = (sr as f64 * 2.5) as usize;
    let x = (0..n)
        .map(|i| {
            let t = i as f64 / sr as f64;
            let mut s = 0.0;
            for m in [60.0, 64.0, 67.0] {
                let f = 440.0 * 2f64.powf((m - 69.0) / 12.0);
                for k in 1..6 {
                    s += (2.0 * PI * f * k as f64 * t).sin() / k as f64;
                }
            }
            (0.1 * s * (t / 0.02).min(1.0)) as f32
        })
        .collect();
    AudioBuffer::from_mono(sr, x)
}

#[test]
fn registered_transcriber_drives_poly_and_auto() {
    register_poly_transcriber(make);
    assert!(poly::poly_transcriber_registered());
    let opts = TranscribeOptions { mode: Mode::Poly, bpm: Some(120.0), ..Default::default() };
    let t = transcribe(&chord_audio(), &opts).unwrap();
    assert_eq!(t.mode, "poly");
    assert_eq!(t.engine, "basic-pitch");
    let got: Vec<(u8, u8)> = t.notes.iter().map(|n| (n.pitch, n.velocity)).collect();
    // velocities follow basic-pitch: round(127 × mean activation)
    assert_eq!(got, vec![(60, 89), (64, 76), (67, 64), (67, 102)], "{:?}", t.notes);
    let first = &t.notes[0];
    assert!((first.start_s - 0.5).abs() < 0.02 && (first.end_s - 1.5).abs() < 0.03, "{first:?}");
    assert!((t.notes[0].confidence - 0.7).abs() < 1e-3);
    assert!(t.model.is_some());

    // auto: the chord fails the monophony test, so the poly transcriber is used
    let t = transcribe(&chord_audio(), &TranscribeOptions::default()).unwrap();
    assert_eq!(t.mode, "poly");
    assert!(!t.polyphony.as_ref().unwrap().monophonic);
    assert!(t.warnings.is_empty(), "{:?}", t.warnings);
}

#[test]
fn poly_through_the_executor_records_the_model() {
    register_poly_transcriber(make);
    let d = TempDir::new("poly-seam");
    let input = d.path("piano.wav");
    testing::write_wav(&input, &chord_audio());
    let out = d.path("piano.mid");
    let reg = testing::leak_registry(ferraus_midi::ops());
    let rt = Runtime {
        registry: reg,
        io: &testing::SIMPLE_IO,
        progress: &testing::NO_PROGRESS,
        presets: &|_, _| None,
        cancel: Default::default(),
    };
    let inv = Invocation {
        command: "transcribe".into(),
        args: vec![("mode".into(), "poly".into())],
        inputs: vec![input.display().to_string()],
        globals: Globals { output: Some(out.display().to_string()), sidecar: false, ..Globals::default() },
        argv: vec![],
        stdin: None,
    };
    let env = rt.invoke(inv).envelope;
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.models.len(), 1);
    assert_eq!(env.models[0].name, "transcribe/basic-pitch");
    let f = MidiFile::read(&out).unwrap();
    assert_eq!(f.note_count(), 4);
}
