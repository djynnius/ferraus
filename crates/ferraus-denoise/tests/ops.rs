//! Behaviour of the three denoise commands: profile files, merging, automatic region
//! detection, errors, modes, linking, transients, residuals, and edge cases.

mod common;

use common::*;
use ferraus_core::testing::{self, TempDir};
use ferraus_core::{AudioBuffer, ErrorKind, OpCtx, OpSpec, Runner};
use ferraus_denoise::profile::{self, MergeMethod, NoiseProfile, ProfileSource};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn spec(name: &str) -> OpSpec {
    ferraus_denoise::ops().into_iter().find(|o| o.name == name).unwrap()
}

fn run_custom(
    name: &str,
    pairs: &[(&str, &str)],
    inputs: Vec<PathBuf>,
    output: Option<&Path>,
) -> (ferraus_core::Result<Value>, OpCtx<'static>) {
    let s = spec(name);
    let a = testing::args(&s, pairs);
    let mut c = testing::ctx();
    c.command = s.name.to_string();
    c.inputs = inputs;
    c.globals.output = output.map(|p| p.display().to_string());
    let r = match s.runner {
        Runner::Custom(f) => f(&a, &mut c),
        _ => unreachable!(),
    };
    (r, c)
}

fn run_transform(pairs: &[(&str, &str)], buf: AudioBuffer) -> (ferraus_core::Result<AudioBuffer>, OpCtx<'static>) {
    let s = spec("denoise");
    let a = testing::args(&s, pairs);
    let mut c = testing::ctx();
    c.command = "denoise".into();
    let r = ferraus_core::exec::apply_spec(&s, &a, buf, &mut c);
    (r, c)
}

fn noise_buf(secs: f64, amp: f32, seed: u64) -> AudioBuffer {
    let mut x = make_noise(NoiseKind::Fan, secs, seed);
    x.iter_mut().for_each(|v| *v *= amp);
    AudioBuffer::from_mono(SR, x)
}

fn seg_rms(b: &AudioBuffer, c: usize, a: f64, e: f64) -> f64 {
    rms(&b.data[c][(a * SR as f64) as usize..(e * SR as f64) as usize])
}

/// Every op has an about text, help on every param, and at least two runnable examples.
#[test]
fn specs_are_documented() {
    let ops = ferraus_denoise::ops();
    assert_eq!(ops.len(), 3);
    for op in &ops {
        assert!(!op.about.is_empty(), "{}", op.name);
        assert!(op.examples.len() >= 2, "{}", op.name);
        for e in &op.examples {
            assert!(e.cmd.starts_with(&format!("ferraus {}", op.name)), "{}", e.cmd);
        }
        for p in &op.params {
            assert!(!p.help.is_empty(), "{} --{}", op.name, p.name);
        }
        assert_eq!(op.category, ferraus_core::Category::Clean);
    }
    let d = spec("denoise");
    for (name, default) in [
        ("reduction", "12dB"),
        ("sensitivity", "6"),
        ("smoothing", "3"),
        ("attack", "20ms"),
        ("release", "120ms"),
        ("artifact-control", "50%"),
        ("link", "on"),
        ("preserve-transients", "on"),
        ("noisiness", "30%"),
        ("decay", "65%"),
        ("floor", "0dB"),
        ("model", "dfn3"),
        ("strength", "100%"),
        ("mode", "profile"),
    ] {
        assert_eq!(d.find_param(name).unwrap().default, Some(default), "{name}");
    }
    assert!(d.auto_mix);
}

#[test]
fn profile_json_round_trip() {
    let x = testing::stereo(noise_buf(1.0, 0.1, 1));
    let p = profile::learn(
        &x,
        0,
        x.frames(),
        ProfileSource { method: "range".into(), start_s: Some(0.0), end_s: Some(1.0), ..Default::default() },
    )
    .unwrap();
    assert_eq!(p.fft_size, 2048);
    assert_eq!(p.hop_size, 512);
    assert_eq!(p.channels, 2);
    assert_eq!(p.sample_rate, SR);
    assert_eq!(p.mean_db.len(), 2);
    assert_eq!(p.mean_db[0].len(), 1025);
    assert_eq!(p.linked.std_db.len(), 1025);
    assert_eq!(p.ferraus_version, ferraus_core::FERRAUS_VERSION);
    let bytes = p.to_json();
    let back = NoiseProfile::from_json(&bytes).unwrap();
    assert_eq!(back, p);
    // plain JSON an agent can inspect
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["format"], "ferraus-noiseprof");
    assert_eq!(v["fft_size"], 2048);
    assert_eq!(v["channels"], 2);
    assert!(v["mean_db"][1][100].is_number());
    // broadband noise: std of the dB magnitude is near the Rayleigh value (5.57 dB)
    let s: f32 = p.std_db[0][100..900].iter().sum::<f32>() / 800.0;
    assert!((s - 5.57).abs() < 0.8, "std {s}");
    // corrupt files are rejected with an input error
    let mut bad = v.clone();
    bad["mean_db"][0] = serde_json::json!([1.0, 2.0]);
    let e = NoiseProfile::from_json(&serde_json::to_vec(&bad).unwrap()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::InputMissing);
}

#[test]
fn profile_op_writes_file_and_reports_auto_region() {
    let dir = TempDir::new("noiseprof");
    // loud music, planted quiet noise-only region 1.5..2.7 s, loud music again
    let music = music_like(0.0, 4.0, 0.5).buf;
    let quiet = noise_buf(4.0, 0.003, 2);
    let mut x = testing::add(&music, &quiet);
    let (a, b) = ((1.5 * SR as f64) as usize, (2.7 * SR as f64) as usize);
    x.data[0][a..b].copy_from_slice(&quiet.data[0][a..b]);
    let input = dir.path("take.wav");
    testing::write_wav(&input, &x);

    let out = dir.path("room.noiseprof");
    let (r, c) = run_custom("denoise profile", &[("auto", "true")], vec![input.clone()], Some(&out));
    let v = r.unwrap();
    let region = &v["noise_region"];
    let (s, e) = (region["start_s"].as_f64().unwrap(), region["end_s"].as_f64().unwrap());
    assert!(s >= 1.5 - 0.02 && e <= 2.7 + 0.02, "auto picked {s}..{e}");
    assert!(e - s > 0.95, "window {s}..{e}");
    assert!(c.notes.contains_key("noise_region"));
    assert_eq!(c.outputs.len(), 1);
    let p = NoiseProfile::from_json(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(p.source.method, "auto");
    assert_eq!(v["method"], "auto");

    // --range (default 0s..2s) and --from
    let out2 = dir.path("range.noiseprof");
    let (r, _) = run_custom("denoise profile", &[("range", "1.6s..2.6s")], vec![input.clone()], Some(&out2));
    assert_eq!(r.unwrap()["noise_region"]["start_s"], 1.6);
    let noise_file = dir.path("roomtone.wav");
    testing::write_wav(&noise_file, &noise_buf(1.0, 0.01, 3));
    let out3 = dir.path("from.noiseprof");
    let from = noise_file.display().to_string();
    let (r, _) = run_custom("denoise profile", &[("from", from.as_str())], vec![], Some(&out3));
    assert_eq!(r.unwrap()["method"], "from");
    // conflicting sources
    let (r, _) = run_custom(
        "denoise profile",
        &[("auto", "true"), ("range", "0s..1s")],
        vec![input],
        Some(&dir.path("x.noiseprof")),
    );
    assert_eq!(r.unwrap_err().kind, ErrorKind::Usage);
}

#[test]
fn auto_region_prefers_stable_noise_over_a_quiet_irregular_passage() {
    // 0..1 s: a slightly lower noise floor with sparse soft broadband bursts (breaths,
    // rustle), 1.2..2.4 s: steady room tone, the rest loud music
    let music = music_like(0.0, 3.5, 0.5).buf;
    let steady = noise_buf(3.5, 0.004, 4);
    let floor = noise_buf(3.5, 0.0032, 20);
    let bursts = noise_buf(3.5, 0.012, 21);
    let mut x = music.clone();
    let sr = SR as f64;
    for i in 0..(1.0 * sr) as usize {
        let t = i as f64 / sr;
        let on = (t * 7.0).fract() < 0.15;
        x.data[0][i] = floor.data[0][i] + if on { bursts.data[0][i] } else { 0.0 };
    }
    let (a, b) = ((1.2 * sr) as usize, (2.4 * sr) as usize);
    x.data[0][a..b].copy_from_slice(&steady.data[0][a..b]);
    let r = profile::find_quiet_region(&x, 1.0).unwrap();
    assert!(r.start_s >= 1.18 && r.end_s <= 2.42, "picked {:.2}..{:.2}", r.start_s, r.end_s);
}

#[test]
fn merge_math_max_and_mean() {
    let mk = |mean: f32, std: f32, frames: usize| {
        let x = noise_buf(0.2, 0.1, 5);
        let mut p = profile::learn(&x, 0, x.frames(), Default::default()).unwrap();
        p.frames = frames;
        for c in 0..p.channels {
            p.mean_db[c].iter_mut().for_each(|v| *v = mean);
            p.std_db[c].iter_mut().for_each(|v| *v = std);
        }
        p.linked.mean_db.iter_mut().for_each(|v| *v = mean);
        p.linked.std_db.iter_mut().for_each(|v| *v = std);
        p
    };
    let a = mk(-60.0, 2.0, 10);
    let b = mk(-50.0, 4.0, 30);
    let names = vec!["a".to_string(), "b".to_string()];
    let (m, w) = profile::merge(&[a.clone(), b.clone()], &names, MergeMethod::Max).unwrap();
    assert!(w.is_empty());
    assert_eq!(m.mean_db[0][7], -50.0);
    assert_eq!(m.std_db[0][7], 4.0);
    assert_eq!(m.frames, 40);
    assert_eq!(m.source.merged_from, names);
    let (m, _) = profile::merge(&[a, b], &names, MergeMethod::Mean).unwrap();
    // pooled: mean = (10·-60 + 30·-50)/40 = -52.5
    // var = (10·(4 + 3600) + 30·(16 + 2500))/40 − 52.5² = 13 + 18.75 = 31.75
    assert!((m.mean_db[0][3] + 52.5).abs() < 1e-3);
    assert!((m.std_db[0][3] - 31.75f32.sqrt()).abs() < 1e-3, "{}", m.std_db[0][3]);
    assert!((m.linked.mean_db[0] + 52.5).abs() < 1e-3);
}

#[test]
fn merge_mean_matches_profiling_everything_at_once() {
    let x = noise_buf(2.0, 0.05, 6);
    let half = x.frames() / 2;
    let a = profile::learn(&x, 0, half, Default::default()).unwrap();
    let b = profile::learn(&x, half, x.frames(), Default::default()).unwrap();
    // the same frames, accumulated in one learner
    let mut l = profile::ProfileLearner::new(SR, 1);
    l.add_region(&x, 0, half);
    l.add_region(&x, half, x.frames());
    let all = l.finish(Default::default()).unwrap();
    let (m, _) = profile::merge(&[a, b], &["a".into(), "b".into()], MergeMethod::Mean).unwrap();
    let err_mean: f32 = (0..1025).map(|k| (m.mean_db[0][k] - all.mean_db[0][k]).abs()).sum::<f32>() / 1025.0;
    let err_std: f32 = (0..1025).map(|k| (m.std_db[0][k] - all.std_db[0][k]).abs()).sum::<f32>() / 1025.0;
    // pooling is exact up to the n-1 vs n normalization of the sample std and rounding
    assert!(err_mean < 0.002 && err_std < 0.03, "{err_mean} {err_std}");
}

#[test]
fn merge_op_and_mismatch_errors() {
    let dir = TempDir::new("merge");
    let x = noise_buf(1.0, 0.05, 7);
    let p1 = profile::learn(&x, 0, x.frames(), Default::default()).unwrap();
    let mut p2 = p1.clone();
    p2.linked.mean_db.iter_mut().for_each(|v| *v += 6.0);
    p2.mean_db[0].iter_mut().for_each(|v| *v += 6.0);
    let (f1, f2) = (dir.path("a.noiseprof"), dir.path("b.noiseprof"));
    std::fs::write(&f1, p1.to_json()).unwrap();
    std::fs::write(&f2, p2.to_json()).unwrap();
    let out = dir.path("c.noiseprof");
    let (r, c) = run_custom("denoise profile merge", &[], vec![f1.clone(), f2.clone()], Some(&out));
    let v = r.unwrap();
    assert_eq!(v["method"], "merge-max");
    assert_eq!(c.outputs.len(), 1);
    let m = NoiseProfile::from_json(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(m.mean_db[0], p2.mean_db[0]);

    // a profile at another sample rate cannot be merged
    let y = AudioBuffer::from_mono(44_100, noise_buf(1.0, 0.05, 8).data.remove(0));
    let p3 = profile::learn(&y, 0, y.frames(), Default::default()).unwrap();
    let f3 = dir.path("c44.noiseprof");
    std::fs::write(&f3, p3.to_json()).unwrap();
    let (r, _) =
        run_custom("denoise profile merge", &[("method", "mean")], vec![f1, f3], Some(&dir.path("d.noiseprof")));
    let e = r.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
    assert!(e.hint.is_some());
}

#[test]
fn saved_profile_applies_and_checks_sample_rate() {
    let dir = TempDir::new("apply");
    let clean = speech_like(1.0, 1.5, 0.5);
    let nz = make_noise(NoiseKind::Hiss, 2.5, 9);
    let (noisy, _) = mix(&clean.buf, &nz, 10.0, SR as usize);
    let p = profile::learn(&noisy, 0, SR as usize, Default::default()).unwrap();
    let f = dir.path("room.noiseprof");
    std::fs::write(&f, p.to_json()).unwrap();
    let fs = f.display().to_string();
    let (r, c) = run_transform(&[("profile", fs.as_str())], noisy.clone());
    let out = r.unwrap();
    let before = seg_rms(&noisy, 0, 0.1, 0.9);
    let after = seg_rms(&out, 0, 0.1, 0.9);
    assert!(20.0 * (after / before).log10() < -10.0);
    assert!(c.input_records.iter().any(|i| i.path.ends_with("room.noiseprof")));
    // resampled input → explicit error with a hint
    let other = AudioBuffer::from_mono(44_100, noisy.data[0][..44_100].to_vec());
    let (r, _) = run_transform(&[("profile", fs.as_str())], other);
    let e = r.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
    assert!(e.hint.unwrap().contains("--sample-rate 48000"));
}

#[test]
fn default_mode_learns_from_quiet_region_and_warns() {
    let clean = speech_like(1.0, 1.5, 0.5);
    let nz = make_noise(NoiseKind::Hiss, 2.5, 10);
    let (noisy, _) = mix(&clean.buf, &nz, 10.0, SR as usize);
    let (r, c) = run_transform(&[], noisy.clone());
    let out = r.unwrap();
    assert!(c.warnings.iter().any(|w| w.code == "no_noise_profile"));
    let reg = &c.notes["noise_region"];
    assert_eq!(reg["method"], "auto");
    assert!(reg["end_s"].as_f64().unwrap() <= 1.1, "{reg}");
    assert!(seg_rms(&out, 0, 0.1, 0.9) < 0.3 * seg_rms(&noisy, 0, 0.1, 0.9));
    // explicit --auto: same result, no warning
    let (r2, c2) = run_transform(&[("auto", "true")], noisy);
    assert_eq!(r2.unwrap(), out);
    assert!(!c2.warnings.iter().any(|w| w.code == "no_noise_profile"));
    assert!(c2.metrics.contains_key("removed_rms_dbfs"));
}

#[test]
fn residual_is_written_and_sums_back_to_the_input() {
    let dir = TempDir::new("residual");
    let clean = speech_like(0.5, 1.0, 0.5);
    let nz = make_noise(NoiseKind::Fan, 1.5, 11);
    let (noisy, _) = mix(&clean.buf, &nz, 10.0, SR as usize / 2);
    let rp = dir.path("removed.wav");
    let rs = rp.display().to_string();
    let (r, c) = run_transform(&[("noise-range", "0s..0.5s"), ("residual", rs.as_str())], noisy.clone());
    let out = r.unwrap();
    let res = testing::read_wav(&rp);
    assert_eq!(res.frames(), noisy.frames());
    let sum = testing::add(&out, &res);
    assert!(testing::rms_diff(&sum, &noisy) < 1e-5);
    assert!(c.outputs.iter().any(|o| o.role.as_deref() == Some("residual")));
}

#[test]
fn model_slots_fail_explicitly() {
    let home = TempDir::new("models-home");
    std::env::set_var("FERRAUS_HOME", &home.0);
    let x = noise_buf(0.5, 0.05, 12);
    for (pairs, hint) in [
        (vec![("mode", "ml")], "ferraus models pull denoise/dfn3"),
        (vec![("mode", "ml"), ("model", "dfn3")], "ferraus models pull denoise/dfn3"),
        (vec![("mode", "hybrid"), ("auto", "true")], "ferraus models pull denoise/dfn3"),
        (vec![("mode", "ml"), ("model", "music")], "ferraus models pull denoise/music-roformer"),
        (vec![("dereverb", "true")], "ferraus models pull denoise/dereverb-roformer"),
    ] {
        let (r, _) = run_transform(&pairs, x.clone());
        let e = r.unwrap_err();
        assert_eq!(e.kind, ErrorKind::ModelNotCached, "{pairs:?}");
        assert_eq!(e.exit_code, 5);
        // registry models name their version in the hint (denoise/dfn3@0.5.6)
        assert!(e.hint.as_deref().unwrap_or("").starts_with(hint), "{pairs:?}: {:?}", e.hint);
    }
    let slots = ferraus_denoise::ml::model_slots();
    assert!(slots.iter().any(|s| s.pattern == "denoise/music-*"));
    assert!(slots.iter().any(|s| s.pattern == "denoise/dereverb-*"));
}

#[test]
fn usage_errors() {
    let x = noise_buf(0.5, 0.05, 13);
    for pairs in [
        vec![("adaptive", "true"), ("mode", "ml")],
        vec![("auto", "true"), ("noise-range", "0s..0.2s")],
        vec![("noise-range", "0s..10ms")],
    ] {
        let (r, _) = run_transform(&pairs, x.clone());
        assert_eq!(r.unwrap_err().kind, ErrorKind::Usage, "{pairs:?}");
    }
}

#[test]
fn rnnoise_and_hybrid_record_the_model_and_warn() {
    let clean = speech_like(0.5, 1.0, 0.5);
    let nz = make_noise(NoiseKind::Fan, 1.5, 14);
    let (noisy, _) = mix(&clean.buf, &nz, 5.0, SR as usize / 2);
    for mode in ["ml", "hybrid"] {
        let (r, c) = run_transform(&[("mode", mode), ("model", "rnnoise"), ("strength", "80%")], noisy.clone());
        let out = r.unwrap();
        assert_eq!(out.frames(), noisy.frames());
        assert_eq!(c.models.len(), 1);
        assert_eq!(c.models[0].name, "denoise/rnnoise");
        assert_eq!(c.models[0].sha256.len(), 64);
        assert!(c.warnings.iter().any(|w| w.code == "speech_model"));
        assert_eq!(c.notes["model_domain"]["domain"], "speech");
    }
    // 44.1 kHz input is resampled to 48 kHz and back
    let y = AudioBuffer::from_mono(44_100, noisy.data[0][..44_100].to_vec());
    let (r, _) = run_transform(&[("mode", "ml"), ("model", "rnnoise")], y);
    assert_eq!(r.unwrap().frames(), 44_100);
}

#[test]
fn adaptive_mode_follows_a_rising_background() {
    // speech over noise that jumps 12 dB at 1.0 s; the last second is noise only
    let clean = speech_like(0.0, 3.0, 0.4);
    let mut x = clean.buf.clone();
    x.data[0].resize(4 * SR as usize, 0.0);
    let nz = make_noise(NoiseKind::Hiss, 4.0, 15);
    for (i, v) in x.data[0].iter_mut().enumerate() {
        let g = if i < SR as usize { 0.005 } else { 0.02 };
        *v += g * nz[i];
    }
    let (r, c) = run_transform(&[("adaptive", "true")], x.clone());
    let out = r.unwrap();
    assert_eq!(c.notes["mode"], "adaptive");
    let red = 20.0 * (seg_rms(&out, 0, 3.2, 3.95) / seg_rms(&x, 0, 3.2, 3.95)).log10();
    assert!(red < -8.0, "adaptive reduced the louder background by only {red:.1} dB");
    // a profile learned before the jump cannot follow it
    let (r, _) = run_transform(&[("noise-range", "0s..0.04s"), ("sensitivity", "6")], x.clone());
    let _ = r.unwrap_err(); // 40 ms is shorter than one analysis frame
    let (r, _) = run_transform(&[("noise-range", "0.4s..0.95s")], x.clone());
    let out_p = r.unwrap();
    let red_p = 20.0 * (seg_rms(&out_p, 0, 3.2, 3.95) / seg_rms(&x, 0, 3.2, 3.95)).log10();
    eprintln!("rising background: adaptive {red:.1} dB, stale profile {red_p:.1} dB");
    assert!(red < red_p - 3.0, "adaptive {red:.1} dB vs stale profile {red_p:.1} dB");
}

#[test]
fn link_shares_one_mask_across_channels() {
    // L: 1 kHz tone + noise, R: independent noise only
    let tone = testing::sine(SR, 1000.0, 2.0, 0.3);
    let nl = noise_buf(2.0, 0.02, 16);
    let nr = noise_buf(2.0, 0.02, 17);
    let x = AudioBuffer::from_planar(SR, vec![testing::add(&tone, &nl).data.remove(0), nr.data[0].clone()]);
    // learn from a noise-only copy so the tone is not part of the profile
    let noise_only = AudioBuffer::from_planar(SR, vec![nl.data[0].clone(), nr.data[0].clone()]);
    let p = profile::learn(&noise_only, 0, SR as usize, Default::default()).unwrap();
    let dir = TempDir::new("link");
    let f = dir.path("p.noiseprof");
    std::fs::write(&f, p.to_json()).unwrap();
    let fs = f.display().to_string();
    let band = |b: &AudioBuffer, c: usize| -> f64 {
        let s = &b.data[c][SR as usize / 2..];
        (-20..=20).map(|d| testing::tone_level(s, SR, 1000.0 + d as f64).powi(2)).sum::<f64>()
    };
    let (on, _) = run_transform(&[("profile", fs.as_str()), ("link", "on")], x.clone());
    let (off, _) = run_transform(&[("profile", fs.as_str()), ("link", "off")], x.clone());
    let (on, off) = (on.unwrap(), off.unwrap());
    let r_on = 10.0 * (band(&on, 1) / band(&x, 1)).log10();
    let r_off = 10.0 * (band(&off, 1) / band(&x, 1)).log10();
    assert!(r_on > -4.0, "linked: R near the tone should follow L's open mask ({r_on:.1} dB)");
    assert!(r_off < -8.0, "unlinked: R is gated on its own ({r_off:.1} dB)");
    // the tone itself survives either way
    let t_on = 10.0 * (band(&on, 0) / band(&x, 0)).log10();
    assert!(t_on > -0.5, "{t_on}");
}

#[test]
fn transients_survive_better_with_preserve_on() {
    // decaying broadband bursts (drum-like) every 0.4 s in steady noise
    let sr = SR as f64;
    let n = (2.4 * sr) as usize;
    let burst_src = noise_buf(2.4, 1.0, 18);
    let mut clean = vec![0f32; n];
    for k in 0..4 {
        let start = ((0.8 + 0.4 * k as f64) * sr) as usize;
        for i in 0..(0.06 * sr) as usize {
            let env = (-(i as f64) / (0.012 * sr)).exp() as f32;
            clean[start + i] = 0.4 * env * burst_src.data[0][start + i];
        }
    }
    let nz = noise_buf(2.4, 0.02, 19);
    let clean = AudioBuffer::from_mono(SR, clean);
    let x = testing::add(&clean, &nz);
    let mask: Vec<bool> = clean.data[0].iter().map(|v| v.abs() > 0.0).collect();
    let keep = |pt: &str| {
        let (r, c) = run_transform(&[("noise-range", "0s..0.7s"), ("preserve-transients", pt)], x.clone());
        (retained_db(&clean.data[0], &r.unwrap().data[0], &mask), c.metrics.get("onsets").cloned())
    };
    let (on, onsets) = keep("on");
    let (off, _) = keep("off");
    eprintln!("transients retained: preserve on {on:.2} dB, off {off:.2} dB, onsets {onsets:?}");
    assert!(on >= off - 0.05, "on {on:.2} dB, off {off:.2} dB");
    assert!(on > -1.5, "transients lost {on:.2} dB");
    assert!(onsets.unwrap().as_u64().unwrap() >= 3);
}

#[test]
fn edge_cases() {
    // silence stays silence
    let z = AudioBuffer::new(SR, 2, SR as usize);
    let (r, _) = run_transform(&[("auto", "true")], z.clone());
    assert_eq!(r.unwrap(), z);
    // shorter than one frame: unchanged with a warning
    let tiny = testing::noise(SR, 0.01, 0.1, 1);
    let (r, c) = run_transform(&[("auto", "true")], tiny.clone());
    assert_eq!(r.unwrap(), tiny);
    assert!(c.warnings.iter().any(|w| w.code == "input_too_short"));
    // very short input in adaptive mode keeps its length
    let (r, _) = run_transform(&[("adaptive", "true")], tiny.clone());
    assert_eq!(r.unwrap().frames(), tiny.frames());
    // empty input
    let e = AudioBuffer::new(SR, 1, 0);
    let (r, _) = run_transform(&[], e.clone());
    assert_eq!(r.unwrap().frames(), 0);
    // other sample rates use a scaled FFT
    for (sr, fft) in [(96_000u32, 4096u64), (22_050, 1024)] {
        let x = AudioBuffer::from_mono(sr, testing::noise(sr, 1.0, 0.05, 2).data.remove(0));
        let (r, c) = run_transform(&[("auto", "true")], x.clone());
        assert_eq!(r.unwrap().frames(), x.frames());
        assert_eq!(c.metrics["fft_size"].as_u64(), Some(fft));
    }
}
