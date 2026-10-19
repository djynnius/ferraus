//! ferraus-ml: the model runtime.
//!
//! * [`ModelSpec`] — name, version, SHA-256, sample rate, expected shapes, chunk length,
//!   overlap, provider hints, license (defined in `ferraus-models`, re-exported here).
//! * [`session`] — one ONNX Runtime environment, a session cache keyed by model path and
//!   provider settings, execution-provider selection `cuda → coreml → directml → cpu` with a
//!   warning and CPU fallback, and deterministic mode.
//! * [`chunk`] — chunked inference with raised-cosine overlap-add and resampling to the model
//!   rate, padded so chunked results equal whole-file results.
//! * [`accelerate`] — `accelerate install` / `accelerate status`.
//! * [`onnx_proto`] — a tiny ONNX writer for generated test models (no binary fixtures).

pub mod accelerate;
pub mod chunk;
pub mod onnx_proto;
pub mod provider;
pub mod session;

pub use chunk::{overlap_add, Block, ChunkPlan};
pub use ferraus_models::ModelSpec;
pub use provider::Provider;
pub use session::{load, load_for, ModelSession, SessionOptions, TensorF32};

use ferraus_core::{OpSpec, Result};

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    accelerate::ops()
}

/// Which inference devices/execution providers are available, which one automatic
/// selection would use, and whether GPU libraries from `accelerate install` are present.
pub fn device_report() -> serde_json::Value {
    accelerate::report()
}

/// Run a `[1, C, T] → [1, K, C', T]` (or `[1, C', T]`) waveform model over a planar signal
/// in windows, returning `[K][C'][T]`. `input_name` defaults to the model's first input.
pub fn run_waveform_chunked(
    session: &ModelSession,
    input: &[Vec<f32>],
    plan: &ChunkPlan,
    tick: &mut dyn FnMut(usize, usize) -> Result<()>,
) -> Result<Block> {
    let name = session.input_names.first().cloned().unwrap_or_else(|| "input".into());
    let channels = input.len();
    let mut f = |w: &[Vec<f32>], _k: usize| -> Result<Block> {
        let t = w.first().map(|c| c.len()).unwrap_or(0);
        let mut data = Vec::with_capacity(channels * t);
        for c in w {
            data.extend_from_slice(c);
        }
        let out = session.run(vec![TensorF32::new(&name, vec![1, channels as i64, t as i64], data)])?;
        let o = out.into_iter().next().ok_or_else(|| ferraus_core::Error::processing("model has no outputs"))?;
        split_output(&o, t)
    };
    overlap_add(input, plan, &mut f, tick)
}

/// Turn `[1, K, C, T]` or `[1, C, T]` into `[K][C][T]`.
pub fn split_output(o: &TensorF32, t: usize) -> Result<Block> {
    let dims: Vec<usize> = o.shape.iter().map(|d| *d as usize).collect();
    let (k, c) = match dims.as_slice() {
        [1, k, c, tt] if *tt == t => (*k, *c),
        [1, c, tt] if *tt == t => (1, *c),
        [k, c, tt] if *tt == t => (*k, *c),
        _ => {
            return Err(ferraus_core::Error::processing(format!(
                "unexpected model output shape {:?} for a window of {t} samples",
                o.shape
            )))
        }
    };
    let mut out = vec![vec![Vec::new(); c]; k];
    for (ki, stem) in out.iter_mut().enumerate() {
        for (ci, ch) in stem.iter_mut().enumerate() {
            let off = (ki * c + ci) * t;
            *ch = o.data[off..off + t].to_vec();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::TempDir;

    fn cpu() -> SessionOptions {
        SessionOptions { device: "cpu".into(), deterministic: true, threads: 1, provider_hints: vec![] }
    }

    #[test]
    fn gain_model_runs_on_cpu() {
        let dir = TempDir::new("ml-gain");
        let p = dir.path("gain.onnx");
        std::fs::write(&p, onnx_proto::gain_model(2, 0.5)).unwrap();
        let (s, warnings) = load(&p, &cpu()).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(s.provider, Provider::Cpu);
        let out = s.run(vec![TensorF32::new("x", vec![1, 2, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0])]).unwrap();
        assert_eq!(out[0].shape, vec![1, 2, 3]);
        assert_eq!(out[0].data, vec![0.5, 1.0, 1.5, 2.0, 2.5, 3.0]);
        // session cache: same path + options → same session
        let (s2, _) = load(&p, &cpu()).unwrap();
        assert!(std::sync::Arc::ptr_eq(&s, &s2));
    }

    fn signal(n: usize) -> Vec<Vec<f32>> {
        (0..2)
            .map(|c| {
                (0..n)
                    .map(|i| {
                        let t = i as f32 / 44100.0;
                        (2.0 * std::f32::consts::PI * (220.0 + 110.0 * c as f32) * t).sin() * 0.5
                            + (2.0 * std::f32::consts::PI * 3000.0 * t).sin() * 0.1
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn chunked_onnx_equals_whole_file_without_seams() {
        let dir = TempDir::new("ml-chunk");
        let p = dir.path("conv.onnx");
        let kernel: Vec<f32> = (0..15).map(|i| ((i as f32 - 7.0) * 0.4).cos() / 8.0).collect();
        std::fs::write(&p, onnx_proto::conv_model(2, &kernel)).unwrap();
        let (s, _) = load(&p, &cpu()).unwrap();
        let x = signal(44_100);
        let whole = run_waveform_chunked(&s, &x, &ChunkPlan::whole(), &mut |_, _| Ok(())).unwrap();
        let plan = ChunkPlan::new(8192, 0.25, 16).unwrap();
        let mut calls = 0;
        let chunked = run_waveform_chunked(&s, &x, &plan, &mut |_, _| {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert!(calls > 4, "{calls} windows");
        let mut max_err = 0f32;
        for c in 0..2 {
            for (a, b) in chunked[0][c].iter().zip(&whole[0][c]) {
                max_err = max_err.max((a - b).abs());
            }
        }
        assert!(max_err < 1e-5, "chunked differs from whole by {max_err}");

        // spectral discontinuity check: high-frequency energy (second difference) around each
        // window boundary must match the rest of the signal
        let y = &chunked[0][0];
        let d2: Vec<f32> = (1..y.len() - 1).map(|i| y[i + 1] - 2.0 * y[i] + y[i - 1]).collect();
        let energy = |a: usize, b: usize| d2[a..b].iter().map(|v| v * v).sum::<f32>() / (b - a) as f32;
        let global = energy(64, d2.len() - 64);
        for b in plan.boundaries(y.len()) {
            if b < 64 || b + 64 > d2.len() {
                continue;
            }
            let local = energy(b - 32, b + 32);
            assert!((local / global - 1.0).abs() < 0.25, "boundary {b}: local HF energy {local} vs global {global}");
        }
    }

    #[test]
    fn deterministic_runs_are_bit_exact() {
        let dir = TempDir::new("ml-det");
        let p = dir.path("conv.onnx");
        std::fs::write(&p, onnx_proto::conv_model(2, &[0.25, 0.5, 0.25])).unwrap();
        let (s, _) = load(&p, &cpu()).unwrap();
        let x = signal(10_000);
        let plan = ChunkPlan::new(2048, 0.25, 4).unwrap();
        let a = run_waveform_chunked(&s, &x, &plan, &mut |_, _| Ok(())).unwrap();
        session::clear_cache();
        let (s, _) = load(&p, &cpu()).unwrap();
        let b = run_waveform_chunked(&s, &x, &plan, &mut |_, _| Ok(())).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn separator_model_layout() {
        let dir = TempDir::new("ml-sep");
        let p = dir.path("sep.onnx");
        let kernels = vec![vec![0.0, 1.0, 0.0], vec![0.0, 0.5, 0.0], vec![0.25, 0.0, 0.25]];
        std::fs::write(&p, onnx_proto::separator_model(&kernels)).unwrap();
        let (s, _) = load(&p, &cpu()).unwrap();
        let x = vec![vec![1.0, 2.0, 3.0, 4.0], vec![-1.0, -2.0, -3.0, -4.0]];
        let out = run_waveform_chunked(&s, &x, &ChunkPlan::whole(), &mut |_, _| Ok(())).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0][0], x[0]);
        assert_eq!(out[0][1], x[1]);
        assert_eq!(out[1][1], vec![-0.5, -1.0, -1.5, -2.0]);
        assert_eq!(out[2][0], vec![0.5, 1.0, 1.5, 0.75]);
    }

    #[test]
    fn missing_model_and_bad_device() {
        assert!(load(std::path::Path::new("/nonexistent/x.onnx"), &cpu()).is_err());
        let dir = TempDir::new("ml-dev");
        let p = dir.path("gain.onnx");
        std::fs::write(&p, onnx_proto::gain_model(1, 1.0)).unwrap();
        let bad = SessionOptions { device: "tpu".into(), ..cpu() };
        assert!(load(&p, &bad).is_err());
    }

    #[test]
    fn pinned_unavailable_provider_falls_back_to_cpu() {
        let dir = TempDir::new("ml-fallback");
        let p = dir.path("gain.onnx");
        std::fs::write(&p, onnx_proto::gain_model(1, 2.0)).unwrap();
        // this build has no CUDA: pinning it must warn and fall back to CPU
        if Provider::Cuda.compiled() {
            return;
        }
        let opts = SessionOptions { device: "cuda".into(), deterministic: false, threads: 1, provider_hints: vec![] };
        let (s, warnings) = load(&p, &opts).unwrap();
        assert_eq!(s.provider, Provider::Cpu);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("cuda execution provider failed"));
    }

    #[test]
    fn device_report_shape() {
        let r = device_report();
        assert!(r["providers"].is_array());
        assert!(r["active"].is_string());
        assert!(r["gpu_libs"]["installed"].is_boolean());
    }
}
