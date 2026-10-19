//! Release check (ignored by default): separate a generated 60-minute stereo 44.1 kHz input
//! under the default `--max-memory` (2 GB) and report the peak heap.
//!
//! ```text
//! cargo test --release -p ferraus-separate --test long_input -- --ignored --nocapture
//! # peak RSS of the whole process (macOS):
//! /usr/bin/time -l target/release/deps/long_input-<hash> --ignored --nocapture
//! ```
//!
//! The model is the tiny generated 4-stem separator (a real model's ONNX Runtime peak comes on
//! top, see docs/dev/PERFORMANCE.md); `FERRAUS_LONG_MINUTES` shortens the input. The input WAV
//! is written in chunks so the test itself never holds the whole song.

use ferraus_core::op::Runner;
use ferraus_core::testing::{self, TempDir};
use ferraus_models::ModelSpec;
use serde_json::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Counts live heap bytes and their peak.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    let now = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(now, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                grew(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// A 16-bit stereo WAV of `frames` frames: a 110 Hz tone, a chirp-ish tone, and noise.
fn write_long_wav(path: &std::path::Path, frames: usize) {
    let sr = 44_100u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let data_len = (frames * 4) as u32;
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap();
    f.write_all(&sr.to_le_bytes()).unwrap();
    f.write_all(&(sr * 4).to_le_bytes()).unwrap();
    f.write_all(&4u16.to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    let mut seed = 0x1234_5678u32;
    for i in 0..frames {
        let t = i as f64 / sr as f64;
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (seed >> 9) as f64 / (1u32 << 23) as f64 - 0.5;
        let a = 0.3 * (2.0 * std::f64::consts::PI * 110.0 * t).sin();
        let b = 0.15 * (2.0 * std::f64::consts::PI * (500.0 + 200.0 * (0.1 * t).sin()) * t).sin();
        let l = a + b + 0.05 * noise;
        let r = -0.8 * l;
        for v in [l, r] {
            f.write_all(&((v * 32767.0).round() as i16).to_le_bytes()).unwrap();
        }
    }
    f.flush().unwrap();
}

fn kernels() -> Vec<Vec<f32>> {
    const K: usize = 9;
    let boxcar = vec![1.0 / K as f32; K];
    let mut delta = [0.0f32; K];
    delta[K / 2] = 1.0;
    let hp: Vec<f32> = delta.iter().zip(&boxcar).map(|(d, b)| d - b).collect();
    vec![
        hp.iter().map(|v| v * 0.3).collect(),
        boxcar.iter().map(|v| v * 0.6).collect(),
        delta.iter().map(|v| v * 0.1).collect(),
        hp.iter().map(|v| v * 0.5).collect(),
    ]
}

#[test]
#[ignore]
fn sixty_minute_input_stays_within_the_memory_budget() {
    let minutes: f64 = std::env::var("FERRAUS_LONG_MINUTES").ok().and_then(|v| v.parse().ok()).unwrap_or(60.0);
    let frames = (minutes * 60.0 * 44_100.0) as usize;
    let home = TempDir::new("sep-long-home");
    std::env::set_var("FERRAUS_HOME", home.path("h"));
    let dir = TempDir::new("sep-long");
    let input = dir.path("long.wav");
    write_long_wav(&input, frames);
    let model = dir.path("toy.onnx");
    std::fs::write(&model, ferraus_ml::onnx_proto::separator_model(&kernels())).unwrap();
    let spec = ModelSpec {
        name: "toy".into(),
        version: "test".into(),
        task: "separate".into(),
        sha256: None,
        sample_rate: 44_100,
        stems: vec!["drums".into(), "bass".into(), "other".into(), "vocals".into()],
        arch: "waveform".into(),
        inputs: vec![],
        outputs: vec![],
        chunk_seconds: Some(7.8),
        chunk_samples: None,
        overlap: 0.25,
        context_samples: 16,
        provider_hints: vec![],
        license: Default::default(),
        params: Value::Null,
    };
    spec.save(&model.with_extension("spec.json")).unwrap();

    let ops = ferraus_separate::ops();
    let op = &ops[0];
    let a = testing::args(op, &[("model", model.to_str().unwrap())]);
    let mut ctx = testing::ctx();
    ctx.command = "separate".into();
    ctx.inputs = vec![input.clone()];
    ctx.globals.device = "cpu".into();
    let budget = ctx.globals.max_memory;
    let before = PEAK.load(Relaxed);
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let start = std::time::Instant::now();
    let Runner::Custom(run) = op.runner else { panic!("separate is a custom op") };
    let v = run(&a, &mut ctx).unwrap();
    let secs = start.elapsed().as_secs_f64();
    let peak = PEAK.load(Relaxed);
    let report = serde_json::json!({
        "minutes": minutes,
        "seconds": secs,
        "peak_heap_mb": peak as f64 / (1 << 20) as f64,
        "peak_heap_before_run_mb": before as f64 / (1 << 20) as f64,
        "max_memory_mb": budget / (1 << 20),
        "memory": v["memory"],
        "outputs": ctx.outputs.len(),
    });
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(ctx.outputs.len(), 4);
    assert_eq!(v["memory"]["spilled"], minutes >= 30.0, "{report}");
    // The input streams into a spill file and every stem streams out of one, so the heap holds
    // a few blocks, not the song (one stem alone would be 1.27 GB at 60 minutes).
    if minutes >= 30.0 {
        assert!(peak < 400 << 20, "{report}");
    }
    assert!(peak < 4 << 30, "PRD: a 60-minute input under 4 GB; {report}");
}
