//! Real-model tests: CREPE, SwiftF0 and Basic Pitch on synthetic signals.
//!
//! Each test runs when its model is already in the model cache, or downloads it (allowed
//! explicitly) when `FERRAUS_TEST_MODELS=1`; otherwise it prints a note and passes.

use ferraus_core::testing;
use ferraus_core::{AudioBuffer, OpCtx};
use ferraus_pitch::pyin::PitchFrame;
use ferraus_pitch::tracker::{tracker_for, TrackOptions, TrackerKind};
use std::f64::consts::PI;

fn want_models() -> bool {
    std::env::var("FERRAUS_TEST_MODELS").map(|v| v.trim() == "1").unwrap_or(false)
}

/// A context that may use `name`: `None` (skip) when it is not cached and model tests are
/// not enabled.
fn model_ctx(name: &str) -> Option<OpCtx<'static>> {
    let cache = ferraus_models::Cache::default_root();
    let manifest = ferraus_models::active_registry(&cache).manifest;
    let entry = manifest.resolve(name).expect("registry entry").clone();
    let cached = cache.cached(&entry).is_some();
    if !cached && !want_models() {
        eprintln!("skipping: {name} is not cached (FERRAUS_TEST_MODELS=1 downloads it)");
        return None;
    }
    let mut ctx = testing::ctx();
    ctx.globals.yes = want_models();
    ctx.globals.device = "cpu".into();
    Some(ctx)
}

/// A tone of `harmonics` partials with 1/k amplitudes (1 = a pure sine).
fn tone(sr: u32, f: f64, secs: f64, harmonics: usize) -> Vec<f32> {
    let n = (sr as f64 * secs) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / sr as f64;
            (1..=harmonics).map(|k| 0.5 / k as f64 * (2.0 * PI * f * k as f64 * t).sin()).sum::<f64>() as f32
        })
        .collect()
}

/// A vocal-like signal: a glottal-ish harmonic source (−12 dB/octave) with vibrato, shaped
/// by three vowel formants, plus a little breath noise. Returns the signal and the true f0
/// per sample.
fn vocal(sr: u32, notes: &[(f64, f64)], vib_cents: f64, noise: f32) -> (Vec<f32>, Vec<f64>) {
    let formants = [(730.0, 90.0), (1090.0, 110.0), (2440.0, 160.0)];
    let srf = sr as f64;
    let mut f0 = Vec::new();
    for (f, secs) in notes {
        let n = (secs * srf) as usize;
        for i in 0..n {
            let t = (f0.len() + i) as f64 / srf;
            f0.push(f * 2f64.powf(vib_cents / 1200.0 * (2.0 * PI * 5.5 * t).sin()));
        }
    }
    let mut phase = 0.0f64;
    let mut seed = 12345u64;
    let mut out = Vec::with_capacity(f0.len());
    for &f in &f0 {
        phase += 2.0 * PI * f / srf;
        let mut v = 0.0;
        let mut k = 1.0;
        while k * f < srf * 0.45 && k < 40.0 {
            let h = k * f;
            let env: f64 = formants.iter().map(|(fc, bw)| 1.0 / (1.0 + ((h - fc) / bw).powi(2))).sum::<f64>() + 0.05;
            v += env / (k * k).sqrt() * (k * phase).sin();
            k += 1.0;
        }
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let r = ((seed >> 33) as f64 / (1u64 << 31) as f64) - 1.0;
        out.push((0.15 * v) as f32 + noise * r as f32);
    }
    (out, f0)
}

/// Raw pitch accuracy: fraction of frames (away from the edges) estimated voiced and within
/// 50 cents of the truth.
fn rpa(frames: &[PitchFrame], truth: &dyn Fn(f64) -> Option<f64>, dur: f64) -> (f64, usize) {
    let mut hit = 0usize;
    let mut total = 0usize;
    for fr in frames {
        if fr.time_s < 0.06 || fr.time_s > dur - 0.06 {
            continue;
        }
        let Some(f) = truth(fr.time_s) else { continue };
        total += 1;
        if fr.voiced && fr.freq_hz > 0.0 && (1200.0 * (fr.freq_hz / f).log2()).abs() < 50.0 {
            hit += 1;
        }
    }
    (hit as f64 / total.max(1) as f64, total)
}

/// Accuracy on tones 100–1000 Hz (`harmonics` partials) and vocal-like phrases. Frames
/// count as correct only when they are voiced and within 50 cents (stricter than raw pitch
/// accuracy, which ignores the voicing decision).
fn tones_accuracy(kind: TrackerKind, model: &str, harmonics: usize) {
    ferraus_ml::register_all();
    let Some(mut ctx) = model_ctx(model) else { return };
    let t = tracker_for(&mut ctx, kind).expect("tracker");
    assert!(ctx.models.iter().any(|m| m.name.starts_with("pitch/")), "model recorded in provenance");
    let opts = TrackOptions { fmin: 60.0, fmax: 1200.0, hop_s: 0.010 };
    let mut hits = 0.0;
    let mut total = 0usize;
    // sines 100–1000 Hz (log spaced, off the bin grid) at 16 kHz and 44.1 kHz
    for sr in [16_000u32, 44_100] {
        for i in 0..13 {
            let f = 100.0 * 10f64.powf(i as f64 / 12.0);
            let x = tone(sr, f, 0.6, harmonics);
            let frames = t.track(&x, sr, &opts).unwrap();
            assert_eq!(frames.len(), x.len() / ferraus_pitch::pyin::hop_samples(sr, 0.01) + 1);
            let (acc, n) = rpa(&frames, &|_| Some(f), 0.6);
            eprintln!("{} tone ({harmonics} partials) {f:7.1} Hz @ {sr}: accuracy {:.3}", kind.name(), acc);
            hits += acc * n as f64;
            total += n;
        }
    }
    let sine_rpa = hits / total as f64;
    // vocal-like phrases with vibrato and breath noise, at 44.1 kHz
    let sr = 44_100;
    let mut vh = 0.0;
    let mut vt = 0usize;
    for (notes, vib) in [
        (vec![(110.0, 0.5), (146.83, 0.5), (196.0, 0.5)], 30.0),
        (vec![(220.0, 0.5), (261.63, 0.5), (329.63, 0.5)], 50.0),
        (vec![(392.0, 0.5), (523.25, 0.5), (659.26, 0.5), (880.0, 0.5)], 40.0),
    ] {
        let (x, f0) = vocal(sr, &notes, vib, 0.01);
        let dur = x.len() as f64 / sr as f64;
        let bounds: Vec<f64> = notes.iter().scan(0.0, |acc, n| {
            *acc += n.1;
            Some(*acc)
        }).collect();
        // skip 40 ms around note changes (the truth is ambiguous inside a jump)
        let truth = |t: f64| {
            if bounds.iter().any(|b| (t - b).abs() < 0.04) {
                return None;
            }
            f0.get((t * sr as f64) as usize).copied()
        };
        let frames = t.track(&x, sr, &opts).unwrap();
        let (acc, n) = rpa(&frames, &truth, dur);
        eprintln!("{} vocal {:?}: RPA {:.3}", kind.name(), notes.iter().map(|n| n.0).collect::<Vec<_>>(), acc);
        vh += acc * n as f64;
        vt += n;
    }
    let vocal_rpa = vh / vt as f64;
    eprintln!("{}: tone accuracy {sine_rpa:.4}, vocal accuracy {vocal_rpa:.4}", kind.name());
    assert!(sine_rpa >= 0.95, "{} tone accuracy {sine_rpa}", kind.name());
    assert!(vocal_rpa >= 0.95, "{} vocal accuracy {vocal_rpa}", kind.name());

    // silence and noise are not voiced
    let silence = vec![0f32; 16_000];
    assert!(t.track(&silence, 16_000, &opts).unwrap().iter().all(|f| !f.voiced));
    let mut seed = 7u64;
    let noise: Vec<f32> = (0..32_000)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            0.1 * (((seed >> 33) as f64 / (1u64 << 31) as f64) - 1.0) as f32
        })
        .collect();
    let fr = t.track(&noise, 16_000, &opts).unwrap();
    let voiced = fr.iter().filter(|f| f.voiced).count() as f64 / fr.len() as f64;
    eprintln!("{}: white noise voiced fraction {voiced:.3}", kind.name());
    assert!(voiced < 0.05, "{} voices {voiced} of white noise", kind.name());
}

#[test]
fn crepe_tracks_tones_and_voices() {
    tones_accuracy(TrackerKind::Crepe, "pitch/crepe-tiny", 1);
}

#[test]
fn swiftf0_tracks_tones_and_voices() {
    // SwiftF0 is unreliable on pure sinusoids (confidence 0.4-0.5 below 300 Hz, and wrong on
    // tones that fall exactly on an STFT bin); real voices and instruments are harmonic
    tones_accuracy(TrackerKind::SwiftF0, "pitch/swiftf0", 3);
}

#[test]
fn model_trackers_work_in_tune_and_analyze_pitch() {
    ferraus_ml::register_all();
    let ops = ferraus_pitch::ops();
    let spec = |name: &str| ops.iter().find(|o| o.name == name).unwrap().clone();
    for (tracker, model) in [("crepe", "pitch/crepe-tiny"), ("swiftf0", "pitch/swiftf0")] {
        if model_ctx(model).is_none() {
            continue;
        }
        // 40 cents flat of A3: chromatic correction pulls it to 220 Hz
        let sr = 44_100;
        let f = 220.0 * 2f64.powf(-40.0 / 1200.0);
        let (x, _) = vocal(sr, &[(f, 1.0)], 0.0, 0.0);
        let buf = AudioBuffer::from_mono(sr, x);
        let (y, c) = testing::run_ctx(&spec("tune"), &[("tracker", tracker), ("key", "C"), ("scale", "chromatic"), ("retune-speed", "0ms")], buf).unwrap();
        assert!(c.models.iter().any(|m| m.name == model), "{tracker}: {:?}", c.models);
        let mut ctx = model_ctx(model).unwrap();
        let t = tracker_for(&mut ctx, TrackerKind::parse(tracker).unwrap()).unwrap();
        let frames = t.track(&y.data[0], sr, &TrackOptions::default()).unwrap();
        let mut cents: Vec<f64> = frames
            .iter()
            .filter(|fr| fr.voiced && fr.time_s > 0.3 && fr.time_s < 0.8)
            .map(|fr| 1200.0 * (fr.freq_hz / 220.0).log2())
            .collect();
        cents.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = cents[cents.len() / 2];
        eprintln!("{tracker}: tuned median {med:.1} cents from A3");
        assert!(med.abs() < 15.0, "{tracker}: tuned median {med} cents");
    }
}

/// Basic Pitch on a synthetic C major triad (C4 E4 G4, piano-like decaying partials) that
/// starts at 0.5 s and lasts 1.5 s, inside a 6 s file (several windows).
#[test]
fn basic_pitch_finds_the_chord() {
    use ferraus_ml::basic_pitch::*;
    let Some(mut ctx) = model_ctx(BASIC_PITCH_MODEL) else { return };
    let sr = BASIC_PITCH_RATE as f64;
    let n = (6.0 * sr) as usize;
    let mut x = vec![0f32; n];
    let notes = [60u8, 64, 67];
    for (start, dur) in [(0.5, 1.5), (3.9, 1.2)] {
        let s0 = (start * sr) as usize;
        for &m in &notes {
            let f = 440.0 * 2f64.powf((m as f64 - 69.0) / 12.0);
            for i in 0..(dur * sr) as usize {
                let t = i as f64 / sr;
                let env = (-2.0 * t).exp() * (1.0 - (-t * 400.0).exp());
                let mut v = 0.0;
                for k in 1..6 {
                    v += (2.0 * PI * f * k as f64 * t).sin() / (k * k) as f64;
                }
                x[s0 + i] += (0.15 * env * v) as f32;
            }
        }
    }
    let (frames, onsets, contours) = basic_pitch_activations(&mut ctx, &x).unwrap();
    assert!(ctx.models.iter().any(|m| m.name == BASIC_PITCH_MODEL));
    let t_out = basic_pitch_frames(n);
    assert_eq!(frames.len(), t_out);
    assert_eq!(onsets.len(), t_out);
    assert_eq!(contours.len(), t_out);
    assert!(frames.iter().all(|f| f.len() == BASIC_PITCH_KEYS));
    assert!(contours.iter().all(|f| f.len() == BASIC_PITCH_CONTOUR_BINS));
    let fps = BASIC_PITCH_FPS as f64;
    let keys: Vec<usize> = notes.iter().map(|m| (*m - BASIC_PITCH_MIDI_OFFSET) as usize).collect();
    for (start, dur) in [(0.5f64, 1.5f64), (3.9, 1.2)] {
        // sustained part: the three chord notes are the three strongest frame activations
        let a = ((start + 0.2) * fps) as usize;
        let b = ((start + dur - 0.3) * fps) as usize;
        let mut mean = vec![0f32; BASIC_PITCH_KEYS];
        for f in &frames[a..b] {
            for (m, v) in mean.iter_mut().zip(f) {
                *m += v / (b - a) as f32;
            }
        }
        let mut order: Vec<usize> = (0..BASIC_PITCH_KEYS).collect();
        order.sort_by(|i, j| mean[*j].partial_cmp(&mean[*i]).unwrap());
        eprintln!(
            "chord at {start}s: top frame keys {:?} ({:?})",
            order[..5].iter().map(|k| k + 21).collect::<Vec<_>>(),
            order[..5].iter().map(|k| mean[*k]).collect::<Vec<_>>()
        );
        let mut top: Vec<usize> = order[..3].to_vec();
        top.sort();
        assert_eq!(top, keys, "frame activations at {start}s");
        assert!(keys.iter().all(|k| mean[*k] > 0.3), "{:?}", keys.iter().map(|k| mean[*k]).collect::<Vec<_>>());
        // onsets: each chord note peaks within 50 ms of the attack, and much less mid-note
        let oa = ((start - 0.05) * fps) as usize;
        let ob = ((start + 0.08) * fps) as usize;
        for &k in &keys {
            let (peak_t, peak) = (oa..ob).map(|t| (t, onsets[t][k])).fold((0, 0f32), |m, v| if v.1 > m.1 { v } else { m });
            let mid = onsets[a..b].iter().map(|o| o[k]).fold(0f32, f32::max);
            eprintln!("  onset key {} peak {peak:.2} at {:.3}s, mid-note max {mid:.2}", k + 21, peak_t as f64 / fps);
            assert!(peak > 0.3 && peak > 2.0 * mid, "onset of key {} at {start}s", k + 21);
            assert!(((peak_t as f64 / fps) - start).abs() < 0.06);
        }
        // contour: the bin of each fundamental (3 per semitone, centre bin 3k+1) is active
        for &k in &keys {
            let c = (a..b).map(|t| contours[t][3 * k + 1].max(contours[t][3 * k]).max(contours[t][3 * k + 2])).sum::<f32>()
                / (b - a) as f32;
            assert!(c > 0.2, "contour of key {} at {start}s: {c}", k + 21);
        }
    }
    // silence between the chords: no note is active
    let quiet = (2.6 * fps) as usize..(3.6 * fps) as usize;
    let loud = frames[quiet].iter().flatten().fold(0f32, |m, v| m.max(*v));
    assert!(loud < 0.3, "activation in silence {loud}");
}
