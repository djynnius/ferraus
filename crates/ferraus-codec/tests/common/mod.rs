#![allow(dead_code)]

use ferraus_codec::CodecIo;
use ferraus_core::ctx::{Globals, NoProgress};
use ferraus_core::exec::{Invocation, Runtime};
use ferraus_core::{AudioBuffer, Envelope, Registry};
use std::path::{Path, PathBuf};
use std::process::Command;

pub static IO: CodecIo = CodecIo;
pub static NP: NoProgress = NoProgress;

pub fn registry() -> &'static Registry {
    let mut r = Registry::new();
    r.extend(ferraus_codec::ops());
    Box::leak(Box::new(r))
}

pub fn globals() -> Globals {
    Globals { sidecar: false, ..Globals::default() }
}

/// Run a command through the shared executor with `CodecIo`.
pub fn invoke(cmd: &str, args: &[(&str, &str)], inputs: &[&Path], g: Globals) -> Envelope {
    let reg = registry();
    let presets = |_: &str, _: &str| None;
    let rt = Runtime {
        registry: reg,
        io: &IO,
        progress: &NP,
        presets: &presets,
        cancel: Default::default(),
    };
    let inv = Invocation {
        command: cmd.into(),
        args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        inputs: inputs.iter().map(|p| p.display().to_string()).collect(),
        globals: g,
        argv: vec![],
        stdin: None,
    };
    rt.invoke(inv).envelope
}

/// Deterministic integer-valued test signal at a bit depth: sine + noise, exactly
/// representable at `bits`.
pub fn int_signal(sr: u32, ch: usize, secs: f64, bits: u16) -> AudioBuffer {
    let n = (sr as f64 * secs) as usize;
    let s = (1u64 << (bits - 1)) as f64;
    let mut x: u64 = 0x1234_5678_9abc_def1;
    let data = (0..ch)
        .map(|c| {
            (0..n)
                .map(|i| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let noise = ((x >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.1;
                    let v = 0.7 * (2.0 * std::f64::consts::PI * (220.0 * (c + 1) as f64) * i as f64 / sr as f64).sin() + noise;
                    let q = (v * s).round().clamp(-s, s - 1.0);
                    (q / s) as f32
                })
                .collect()
        })
        .collect();
    AudioBuffer { sample_rate: sr, data }
}

/// The bytes of a RIFF `data` chunk.
pub fn wav_data(bytes: &[u8]) -> Vec<u8> {
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            return bytes[pos + 8..(pos + 8 + len).min(bytes.len())].to_vec();
        }
        pos += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

pub fn ffmpeg() -> Option<PathBuf> {
    ferraus_codec::ffmpeg_bridge::locate()
}

/// Run the system ffmpeg to synthesize a test file (tests skip when it is absent).
pub fn ffmpeg_gen(args: &[&str]) -> bool {
    let Some(bin) = ffmpeg() else { return false };
    let mut full = vec!["-hide_banner", "-loglevel", "error", "-y"];
    full.extend_from_slice(args);
    Command::new(bin).args(&full).status().map(|s| s.success()).unwrap_or(false)
}

pub fn max_abs_diff(a: &AudioBuffer, b: &AudioBuffer) -> f32 {
    a.data
        .iter()
        .zip(&b.data)
        .flat_map(|(x, y)| x.iter().zip(y).map(|(p, q)| (p - q).abs()))
        .fold(0.0, f32::max)
}

/// Best alignment lag (0..max_lag) of `b` against `a` by correlation, then SNR in dB.
pub fn aligned_snr(a: &[f32], b: &[f32], max_lag: usize) -> (usize, f64) {
    let n = a.len().min(b.len()).saturating_sub(max_lag);
    let mut best = (0, f64::MIN);
    for lag in 0..max_lag {
        let c: f64 = (0..n).step_by(7).map(|i| a[i] as f64 * b[i + lag] as f64).sum();
        if c > best.1 {
            best = (lag, c);
        }
    }
    let lag = best.0;
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for i in 0..n {
        let d = a[i] as f64 - b[i + lag] as f64;
        sig += (a[i] as f64).powi(2);
        err += d * d;
    }
    (lag, 10.0 * (sig / err.max(1e-20)).log10())
}
