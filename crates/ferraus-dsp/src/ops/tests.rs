//! Cross-cutting acceptance tests over every op: null tests, documentation completeness,
//! and (ignored, release-only) speed and denormal benchmarks.

use super::all;
use ferraus_core::prelude::*;
use ferraus_core::testing::{args, ctx, noise, stereo};
use std::time::Instant;

fn run_op(spec: &OpSpec, pairs: &[(&str, &str)], input: AudioBuffer) -> Result<AudioBuffer> {
    let a = args(spec, pairs);
    let mut c = ctx();
    ferraus_core::exec::apply_spec(spec, &a, input, &mut c)
}

/// Extra arguments an op needs to run at all.
fn required_args(name: &str, reference: &str) -> Vec<(&'static str, String)> {
    match name {
        "match-eq" => vec![("reference", reference.to_string())],
        "eq" => vec![("band", "peak:1kHz:+3dB".to_string())],
        _ => vec![],
    }
}

#[test]
fn every_op_is_documented() {
    for op in all() {
        assert!(!op.about.is_empty(), "{} has no about", op.name);
        assert!(op.examples.len() >= 2, "{} needs at least two examples", op.name);
        for p in &op.params {
            assert!(!p.help.is_empty(), "{} --{} has no help", op.name, p.name);
        }
        assert!(op.is_transform(), "{} should be a transform", op.name);
    }
}

#[test]
fn every_op_runs_with_defaults() {
    let dir = ferraus_core::testing::TempDir::new("dsp-defaults");
    let refp = dir.path("ref.wav");
    ferraus_core::testing::write_wav(&refp, &noise(48000, 0.5, 0.2, 77));
    let x = stereo(noise(48000, 0.25, 0.3, 1));
    for op in all() {
        let extra = required_args(op.name, refp.to_str().unwrap());
        let pairs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let y = run_op(&op, &pairs, x.clone()).unwrap_or_else(|e| panic!("{}: {}", op.name, e.message));
        assert!(y.frames() > 0, "{}", op.name);
        for ch in &y.data {
            assert!(ch.iter().all(|v| v.is_finite()), "{} produced non-finite samples", op.name);
        }
        // silence in, silence out (no op may invent signal, except the vocoder's carrier
        // which still follows the silent modulator)
        let s = run_op(&op, &pairs, AudioBuffer::new(48000, 2, 4800)).unwrap();
        assert!(s.peak() < 1e-6, "{} turned silence into {}", op.name, s.peak());
        // very short input
        run_op(&op, &pairs, AudioBuffer::new(48000, 1, 3)).unwrap_or_else(|e| panic!("{} short: {}", op.name, e.message));
    }
}

#[test]
fn null_test_mix_zero_is_bit_exact() {
    let x = stereo(noise(44100, 0.2, 0.5, 3));
    for op in all() {
        let has_mix = op.effective_params().iter().any(|p| p.name == "mix");
        if !has_mix {
            assert!(!op.auto_mix, "{}", op.name);
            continue;
        }
        let extra = required_args(op.name, "unused-reference.wav");
        let mut pairs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
        pairs.push(("mix", "0%"));
        let y = run_op(&op, &pairs, x.clone()).unwrap_or_else(|e| panic!("{}: {}", op.name, e.message));
        assert_eq!(y, x, "{} at mix=0% must return the input bit-exactly", op.name);
    }
}

#[test]
fn compress_tighten_preset_is_listed() {
    let c = all().into_iter().find(|o| o.name == "compress").unwrap();
    let t = c.presets.iter().find(|p| p.name == "tighten").expect("tighten preset");
    for key in ["highpass", "ratio", "attack", "release", "deess"] {
        assert!(t.values.iter().any(|(k, _)| *k == key), "tighten lacks {key}");
        assert!(c.find_param(key).is_some(), "compress lacks --{key}");
    }
    assert!(c.find_param("lowpass").is_some());
}

/// Ops measured by the speed test, with arguments.
fn bench_cases() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let mut v: Vec<(&'static str, Vec<(&'static str, &'static str)>)> = all()
        .iter()
        .filter(|o| o.name != "match-eq" && o.name != "eq")
        .map(|o| (o.name, vec![]))
        .collect();
    v.push(("eq", vec![("band", "highpass:80Hz"), ("band", "peak:3kHz:+2dB"), ("band", "highshelf:10kHz:+1dB")]));
    v.push(("eq", vec![("band", "peak:3kHz:+2dB"), ("linear-phase", "true")]));
    v.push(("reverb", vec![("type", "hall"), ("decay", "3s")]));
    v.push(("vocoder", vec![("mode", "spectral")]));
    v.push(("vocoder", vec![("follow", "true")]));
    v.push(("compress", vec![("preset", "tighten"), ("character", "tube")]));
    v.push(("multiband", vec![("bands", "4"), ("limiter", "true")]));
    v.push(("delay", vec![("analog", "true"), ("pingpong", "true")]));
    v
}

/// `cargo test -p ferraus-dsp --release -- --ignored --nocapture speed`
#[test]
#[ignore]
fn speed_every_effect_at_least_50x_realtime() {
    let secs = 10.0;
    let x = stereo(noise(48000, secs, 0.3, 11));
    let mut slow = Vec::new();
    println!("{:<44} {:>10}", "op", "x realtime");
    for (name, pairs) in bench_cases() {
        let op = all().into_iter().find(|o| o.name == name).unwrap();
        let t = Instant::now();
        run_op(&op, &pairs, x.clone()).unwrap();
        let rt = secs / t.elapsed().as_secs_f64();
        let label = format!("{name} {pairs:?}");
        println!("{label:<44} {rt:>10.0}");
        if rt < 50.0 {
            slow.push(format!("{label}: {rt:.1}x"));
        }
    }
    // match-eq with a reference file
    let dir = ferraus_core::testing::TempDir::new("dsp-bench");
    let refp = dir.path("ref.wav");
    ferraus_core::testing::write_wav(&refp, &stereo(noise(48000, secs, 0.2, 5)));
    let op = all().into_iter().find(|o| o.name == "match-eq").unwrap();
    let t = Instant::now();
    run_op(&op, &[("reference", refp.to_str().unwrap())], x.clone()).unwrap();
    let rt = secs / t.elapsed().as_secs_f64();
    println!("{:<44} {rt:>10.0}", "match-eq");
    if rt < 50.0 {
        slow.push(format!("match-eq: {rt:.1}x"));
    }
    assert!(slow.is_empty(), "slower than 50x realtime: {slow:?}");
}

/// Long silence after a loud burst must not be much slower than noise (denormals).
#[test]
#[ignore]
fn denormal_silence_is_not_slower() {
    let rate = 48000;
    let secs = 10.0;
    let n = (rate as f64 * secs) as usize;
    let loud = noise(rate, secs, 0.9, 3);
    let mut burst = loud.clone();
    for v in burst.data[0].iter_mut().skip(rate as usize / 2) {
        *v = 0.0;
    }
    let loud = stereo(loud);
    let burst = stereo(burst);
    assert_eq!(burst.frames(), n);
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("reverb", vec![("mix", "100%")]),
        ("reverb", vec![("type", "hall"), ("mix", "100%"), ("decay", "6s")]),
        ("delay", vec![("feedback", "90%"), ("mix", "100%")]),
        ("chorus", vec![]),
        ("flanger", vec![]),
        ("phaser", vec![]),
        ("compress", vec![]),
        ("gate", vec![]),
        ("expand", vec![]),
        ("lowpass", vec![("order", "8"), ("freq", "40Hz")]),
        ("eq", vec![("band", "peak:100Hz:+6dB:q=8")]),
        ("deess", vec![]),
        ("transient", vec![("attack", "+3dB")]),
        ("multiband", vec![]),
        ("saturate", vec![]),
        ("vocoder", vec![]),
    ];
    let mut bad = Vec::new();
    for (name, pairs) in cases {
        let op = all().into_iter().find(|o| o.name == name).unwrap();
        // warm up and take the best of three to reduce noise
        let time = |b: &AudioBuffer| {
            (0..3)
                .map(|_| {
                    let t = Instant::now();
                    run_op(&op, &pairs, b.clone()).unwrap();
                    t.elapsed().as_secs_f64()
                })
                .fold(f64::MAX, f64::min)
        };
        let tn = time(&loud);
        let ts = time(&burst);
        let ratio = ts / tn;
        println!("{name:<10} {:<40} noise {tn:.3}s  burst+silence {ts:.3}s  ratio {ratio:.2}", format!("{pairs:?}"));
        if ratio > 1.5 {
            bad.push(format!("{name} {pairs:?}: {ratio:.2}"));
        }
    }
    assert!(bad.is_empty(), "silence is slower than noise: {bad:?}");
}
