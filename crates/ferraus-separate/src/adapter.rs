//! Model adapters: every architecture family is wrapped behind [`SeparationModel`], which
//! turns one stereo window at the model rate into one window per stem.
//!
//! * [`WaveformModel`] — waveform in, stems out: `[1, 2, T] → [1, S, 2, T]`. Demucs ONNX
//!   exports and every ferraus export (STFT inside the graph) use it, as do the generated test
//!   models.
//! * [`MdxModel`] — UVR MDX-Net: complex spectrogram `[1, 4, dim_f, dim_t]` in and out, STFT in
//!   Rust; predicts one stem, the other is the complement.

use crate::stft::{istft, stft, Spec};
use ferraus_core::{Error, ErrorKind, Result};
use ferraus_ml::{split_output, Block, ModelSession, ModelSpec, TensorF32};
use realfft::num_complex::Complex32;
use std::sync::Arc;

pub trait SeparationModel: Send + Sync {
    /// Registry name (or file path) for messages and provenance.
    fn name(&self) -> &str;
    /// Output stem names in output order.
    fn stems(&self) -> &[String];
    fn sample_rate(&self) -> u32;
    /// Trained window length in samples (`--segment auto`); `0` = whole input in one pass.
    fn segment(&self) -> usize;
    /// True when the model only accepts exactly `segment()` samples.
    fn fixed_segment(&self) -> bool {
        false
    }
    /// Receptive-field context discarded at each window edge.
    fn context(&self) -> usize {
        0
    }
    /// Normalize the whole track by its standard deviation before inference (Demucs does this).
    fn normalize_track(&self) -> bool {
        false
    }
    /// One stereo window `[2][T]` → `[S][2][T]`.
    fn process(&self, window: &[Vec<f32>]) -> Result<Block>;
}

pub struct WaveformModel {
    pub name: String,
    pub spec: ModelSpec,
    pub session: Arc<ModelSession>,
    stems: Vec<String>,
    segment: usize,
    fixed: bool,
}

impl WaveformModel {
    pub fn new(name: &str, spec: ModelSpec, session: Arc<ModelSession>) -> Result<WaveformModel> {
        // a fixed time dimension in the ONNX signature pins the window length
        let declared =
            session.input_shapes.first().and_then(|s| s.last().copied()).filter(|d| *d > 0).map(|d| d as usize);
        let segment = declared.or_else(|| spec.chunk_len()).unwrap_or(0);
        if spec.stems.is_empty() {
            return Err(Error::usage(format!("{name}: the model spec lists no stems")));
        }
        Ok(WaveformModel {
            name: name.to_string(),
            stems: spec.stems.clone(),
            segment,
            fixed: declared.is_some(),
            spec,
            session,
        })
    }
}

impl SeparationModel for WaveformModel {
    fn name(&self) -> &str {
        &self.name
    }
    fn stems(&self) -> &[String] {
        &self.stems
    }
    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }
    fn segment(&self) -> usize {
        self.segment
    }
    fn fixed_segment(&self) -> bool {
        self.fixed
    }
    fn context(&self) -> usize {
        self.spec.context_samples
    }
    fn normalize_track(&self) -> bool {
        self.spec.params.get("normalize").and_then(|v| v.as_str()).map(|v| v == "track").unwrap_or(false)
    }
    fn process(&self, window: &[Vec<f32>]) -> Result<Block> {
        let t = window[0].len();
        let mut data = Vec::with_capacity(2 * t);
        for c in window {
            data.extend_from_slice(c);
        }
        let input = self.session.input_names.first().cloned().unwrap_or_else(|| "mix".into());
        let out = self.session.run(vec![TensorF32::new(&input, vec![1, window.len() as i64, t as i64], data)])?;
        let block = split_output(&out[0], t)?;
        if block.len() != self.stems.len() {
            return Err(Error::processing(format!(
                "{} produced {} stems but its spec lists {}",
                self.name,
                block.len(),
                self.stems.len()
            )));
        }
        Ok(block)
    }
}

/// UVR MDX-Net spectrogram model.
pub struct MdxModel {
    pub name: String,
    pub spec: ModelSpec,
    pub session: Arc<ModelSession>,
    stems: Vec<String>,
    n_fft: usize,
    hop: usize,
    dim_f: usize,
    dim_t: usize,
    compensate: f32,
}

impl MdxModel {
    pub fn new(name: &str, spec: ModelSpec, session: Arc<ModelSession>) -> Result<MdxModel> {
        let need = |k: &str| {
            spec.param_usize(k).ok_or_else(|| {
                Error::new(
                    ErrorKind::Unsupported,
                    "model_spec_incomplete",
                    format!("{name}: MDX spec is missing '{k}'"),
                )
            })
        };
        let n_fft = need("n_fft")?;
        let hop = need("hop")?;
        let dim_f = need("dim_f")?;
        let dim_t = need("dim_t")?;
        if dim_f > n_fft / 2 + 1 {
            return Err(Error::usage(format!("{name}: dim_f exceeds the FFT size")));
        }
        let stems =
            if spec.stems.len() == 2 { spec.stems.clone() } else { vec!["primary".into(), "complement".into()] };
        Ok(MdxModel {
            name: name.to_string(),
            compensate: spec.param_f64("compensate").unwrap_or(1.0) as f32,
            stems,
            n_fft,
            hop,
            dim_f,
            dim_t,
            spec,
            session,
        })
    }

    fn chunk(&self) -> usize {
        self.hop * (self.dim_t - 1)
    }
}

impl SeparationModel for MdxModel {
    fn name(&self) -> &str {
        &self.name
    }
    fn stems(&self) -> &[String] {
        &self.stems
    }
    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }
    fn segment(&self) -> usize {
        self.chunk()
    }
    fn fixed_segment(&self) -> bool {
        true
    }
    fn context(&self) -> usize {
        // UVR trims n_fft/2 at each chunk edge
        self.spec.context_samples.max(self.n_fft / 2)
    }
    fn process(&self, window: &[Vec<f32>]) -> Result<Block> {
        let t = window[0].len();
        if t != self.chunk() {
            return Err(Error::processing(format!("{} expects windows of {} samples", self.name, self.chunk())));
        }
        let specs: Vec<Spec> = window.iter().map(|c| stft(c, self.n_fft, self.hop)).collect();
        let (df, dt) = (self.dim_f, self.dim_t);
        // [1, 4, dim_f, dim_t]: ch0 re, ch0 im, ch1 re, ch1 im
        let mut data = vec![0f32; 4 * df * dt];
        for (c, s) in specs.iter().enumerate() {
            for (ti, frame) in s.frames.iter().enumerate().take(dt) {
                for (f, v) in frame.iter().enumerate().take(df) {
                    data[((2 * c) * df + f) * dt + ti] = v.re;
                    data[((2 * c + 1) * df + f) * dt + ti] = v.im;
                }
            }
        }
        let input = self.session.input_names.first().cloned().unwrap_or_else(|| "input".into());
        let out = self.session.run(vec![TensorF32::new(&input, vec![1, 4, df as i64, dt as i64], data)])?;
        let o = &out[0].data;
        if o.len() != 4 * df * dt {
            return Err(Error::processing(format!("{}: unexpected output shape {:?}", self.name, out[0].shape)));
        }
        let bins = self.n_fft / 2 + 1;
        let mut primary = Vec::with_capacity(2);
        for c in 0..2 {
            let mut s = specs[c].clone();
            for (ti, frame) in s.frames.iter_mut().enumerate() {
                for (f, v) in frame.iter_mut().enumerate().take(bins) {
                    *v = if f < df && ti < dt {
                        Complex32::new(o[((2 * c) * df + f) * dt + ti], o[((2 * c + 1) * df + f) * dt + ti])
                    } else {
                        Complex32::new(0.0, 0.0)
                    };
                }
            }
            let y: Vec<f32> = istft(&s, t).into_iter().map(|v| v * self.compensate).collect();
            primary.push(y);
        }
        let complement: Vec<Vec<f32>> =
            window.iter().zip(&primary).map(|(m, p)| m.iter().zip(p).map(|(a, b)| a - b).collect()).collect();
        Ok(vec![primary, complement])
    }
}

/// The stems an adapter for `spec` will produce, in output order, without loading the model.
pub fn stems_of(spec: &ModelSpec) -> Vec<String> {
    match spec.arch.as_str() {
        "mdx" if spec.stems.len() != 2 => vec!["primary".into(), "complement".into()],
        _ => spec.stems.clone(),
    }
}

/// Build the adapter for a spec's architecture family.
pub fn open(name: &str, spec: ModelSpec, session: Arc<ModelSession>) -> Result<Box<dyn SeparationModel>> {
    match spec.arch.as_str() {
        "waveform" | "demucs" => Ok(Box::new(WaveformModel::new(name, spec, session)?)),
        "mdx" => Ok(Box::new(MdxModel::new(name, spec, session)?)),
        other => Err(Error::new(
            ErrorKind::Unsupported,
            "model_arch_unsupported",
            format!("{name}: no separation adapter for architecture '{other}' (supported: waveform, mdx)"),
        )),
    }
}
