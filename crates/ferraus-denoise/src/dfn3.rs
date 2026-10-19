//! DeepFilterNet 3 (Schröter et al., 2023) on ONNX Runtime.
//!
//! The registry entry `denoise/dfn3` is the upstream ONNX export: three graphs (`enc.onnx`,
//! `erb_dec.onnx`, `df_dec.onnx`) plus `config.ini`. Everything around the graphs is
//! implemented here, following the reference (`libDF` and `df/enhance.py`, MIT/Apache-2.0):
//!
//! 1. 48 kHz, Vorbis-windowed STFT with a 960-sample window and 480 hop, normalized by
//!    `hop·2 / window²` in the analysis only; the signal is padded with one window of zeros
//!    and the result trimmed by `window − hop` samples (the STFT delay).
//! 2. Features per frame: 32 ERB band energies in dB (rectangular bands of at least two
//!    bins), exponentially mean-normalized (`α = 0.99`, initial state −60…−90 dB, divided by
//!    40); the first 96 complex bins unit-normalized by the square root of an exponential
//!    mean of their magnitude (initial state 0.001…0.0001). Both are shifted two frames
//!    earlier (`conv_lookahead`), zero-padded at the end.
//! 3. Encoder → ERB decoder: 32 gains in 0..1, applied to every bin of their band.
//! 4. Encoder → DF decoder: order-5 complex filters for the first 96 bins, applied to the
//!    *noisy* spectrum over frames `t−2 … t+2` (`df_lookahead = 2`); the gained spectrum is
//!    kept above bin 96.
//! 5. Inverse STFT with overlap-add.
//!
//! The graphs process whole sequences (their GRUs start from zero state on every call).
//! Long inputs run in 30 s segments with 1 s of discarded warm-up context and a 0.5 s
//! crossfade of the gains and filter coefficients, so memory stays bounded; features are
//! always computed over the whole file, so segmenting changes nothing upstream of the graphs.

use ferraus_core::resample::{resample, Quality};
use ferraus_core::{AudioBuffer, Error, OpCtx, Result};
use ferraus_ml::session::{load_for_hinted, ModelSession, TensorF32};
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::path::Path;
use std::sync::Arc;

/// Registry name of the model.
pub const MODEL: &str = "denoise/dfn3";
/// Members of the archive this runtime needs.
pub const MEMBERS: [&str; 4] = ["enc.onnx", "erb_dec.onnx", "df_dec.onnx", "config.ini"];

const MEAN_NORM_INIT: [f32; 2] = [-60.0, -90.0];
const UNIT_NORM_INIT: [f32; 2] = [0.001, 0.0001];

/// Model and DSP parameters (`config.ini`, sections `[df]` and `[deepfilternet]`).
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub sr: usize,
    pub fft_size: usize,
    pub hop_size: usize,
    pub nb_erb: usize,
    pub nb_df: usize,
    pub df_order: usize,
    pub df_lookahead: usize,
    pub conv_lookahead: usize,
    pub min_nb_erb_freqs: usize,
    pub norm_tau: f64,
}

impl Default for Params {
    /// The DeepFilterNet 3 release values.
    fn default() -> Self {
        Params {
            sr: 48_000,
            fft_size: 960,
            hop_size: 480,
            nb_erb: 32,
            nb_df: 96,
            df_order: 5,
            df_lookahead: 2,
            conv_lookahead: 2,
            min_nb_erb_freqs: 2,
            norm_tau: 1.0,
        }
    }
}

impl Params {
    /// Parse `config.ini`; missing keys keep the release defaults.
    pub fn from_ini(text: &str) -> Result<Params> {
        let mut p = Params::default();
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].trim().to_ascii_lowercase();
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let num = || v.parse::<usize>().map_err(|_| Error::processing(format!("DFN3 config.ini: bad {k} = {v}")));
            match (section.as_str(), k) {
                ("df", "sr") => p.sr = num()?,
                ("df", "fft_size") => p.fft_size = num()?,
                ("df", "hop_size") => p.hop_size = num()?,
                ("df", "nb_erb") => p.nb_erb = num()?,
                ("df", "nb_df") => p.nb_df = num()?,
                ("df", "df_order") => p.df_order = num()?,
                ("df", "df_lookahead") => p.df_lookahead = num()?,
                ("df", "min_nb_erb_freqs") => p.min_nb_erb_freqs = num()?,
                ("df", "norm_tau") => {
                    p.norm_tau = v.parse().map_err(|_| Error::processing(format!("DFN3 config.ini: bad norm_tau = {v}")))?
                }
                ("deepfilternet", "conv_lookahead") => p.conv_lookahead = num()?,
                _ => {}
            }
        }
        if p.hop_size * 2 != p.fft_size || p.nb_df > p.fft_size / 2 + 1 || p.df_lookahead >= p.df_order || p.nb_erb < 2
        {
            return Err(Error::unsupported(format!("DFN3 config.ini describes an unsupported layout: {p:?}")));
        }
        Ok(p)
    }

    /// Exponential-mean factor, rounded as the reference does (0.99 for 10 ms hops, τ = 1 s).
    pub fn alpha(&self) -> f32 {
        let a = (-(self.hop_size as f64 / self.sr as f64) / self.norm_tau).exp();
        let mut precision = 3;
        loop {
            let p = 10f64.powi(precision);
            let r = (a * p).round() / p;
            if r < 1.0 {
                return r as f32;
            }
            precision += 1;
        }
    }

    pub fn freq_bins(&self) -> usize {
        self.fft_size / 2 + 1
    }
}

fn freq2erb(f: f32) -> f32 {
    9.265 * (f / (24.7 * 9.265)).ln_1p()
}

fn erb2freq(e: f32) -> f32 {
    24.7 * 9.265 * ((e / 9.265).exp() - 1.0)
}

/// Bins per ERB band (libDF `erb_fb`): equal ERB-scale steps rounded to bins, at least
/// `min_nb_freqs` bins each, summing to `fft_size / 2 + 1`.
pub fn erb_widths(sr: usize, fft_size: usize, nb_bands: usize, min_nb_freqs: usize) -> Vec<usize> {
    let nyq = (sr / 2) as f32;
    let freq_width = sr as f32 / fft_size as f32;
    let erb_low = freq2erb(0.0);
    let erb_high = freq2erb(nyq);
    let step = (erb_high - erb_low) / nb_bands as f32;
    let min = min_nb_freqs as i32;
    let mut erb = vec![0usize; nb_bands];
    let (mut prev, mut over) = (0i32, 0i32);
    for i in 1..=nb_bands {
        let f = erb2freq(erb_low + i as f32 * step);
        let fb = (f / freq_width).round() as i32;
        let mut nb = fb - prev - over;
        if nb < min {
            over = min - nb;
            nb = min;
        } else {
            over = 0;
        }
        erb[i - 1] = nb as usize;
        prev = fb;
    }
    erb[nb_bands - 1] += 1;
    let total: usize = erb.iter().sum();
    let bins = fft_size / 2 + 1;
    if total > bins {
        erb[nb_bands - 1] -= total - bins;
    }
    erb
}

/// The Vorbis window `sin(π/2 · sin²(π(n + ½)/N))`, power complementary at 50% overlap.
pub fn vorbis_window(n: usize) -> Vec<f32> {
    let half = (n / 2) as f64;
    (0..n)
        .map(|i| {
            let s = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / half).sin();
            (0.5 * std::f64::consts::PI * s * s).sin() as f32
        })
        .collect()
}

/// libDF's frame-by-frame STFT (analysis memory of one hop, synthesis overlap-add).
pub struct Stft {
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    window: Vec<f32>,
    wnorm: f32,
    hop: usize,
}

impl Stft {
    pub fn new(fft_size: usize, hop: usize) -> Stft {
        let mut planner = RealFftPlanner::<f32>::new();
        Stft {
            fwd: planner.plan_fft_forward(fft_size),
            inv: planner.plan_fft_inverse(fft_size),
            window: vorbis_window(fft_size),
            wnorm: 1.0 / ((fft_size * fft_size) as f32 / (2 * hop) as f32),
            hop,
        }
    }

    /// Spectra of `x` (`len / hop` frames; frame `t` covers samples `(t−1)·hop … (t+1)·hop`).
    pub fn analysis(&self, x: &[f32]) -> Vec<Vec<Complex32>> {
        let n = self.window.len();
        let frames = x.len() / self.hop;
        let mut buf = self.fwd.make_input_vec();
        let mut scratch = self.fwd.make_scratch_vec();
        let mut out = Vec::with_capacity(frames);
        for t in 0..frames {
            for (i, b) in buf.iter_mut().enumerate() {
                let idx = (t * self.hop + i) as isize - (n - self.hop) as isize;
                let v = if idx >= 0 { x[idx as usize] } else { 0.0 };
                *b = v * self.window[i];
            }
            let mut spec = self.fwd.make_output_vec();
            // lengths always match the plan
            let _ = self.fwd.process_with_scratch(&mut buf, &mut spec, &mut scratch);
            for s in spec.iter_mut() {
                *s *= self.wnorm;
            }
            out.push(spec);
        }
        out
    }

    /// Overlap-add synthesis: `frames × hop` samples.
    pub fn synthesis(&self, spec: &mut [Vec<Complex32>]) -> Vec<f32> {
        let n = self.window.len();
        let mut out = vec![0f32; spec.len() * self.hop];
        let mut mem = vec![0f32; n - self.hop];
        let mut buf = self.inv.make_output_vec();
        let mut scratch = self.inv.make_scratch_vec();
        for (t, s) in spec.iter_mut().enumerate() {
            // the DC and Nyquist bins must be real for the inverse real FFT
            s[0].im = 0.0;
            if let Some(last) = s.last_mut() {
                last.im = 0.0;
            }
            let _ = self.inv.process_with_scratch(s, &mut buf, &mut scratch);
            for (b, w) in buf.iter_mut().zip(&self.window) {
                *b *= w;
            }
            for i in 0..self.hop {
                out[t * self.hop + i] = buf[i] + mem[i];
            }
            mem.copy_from_slice(&buf[self.hop..]);
        }
        out
    }
}

/// Encoder inputs for a whole signal: `erb` is `[T][nb_erb]`, `spec` is `[2][T][nb_df]`
/// (real, imaginary), both already shifted by `conv_lookahead`.
pub struct Features {
    pub frames: usize,
    pub erb: Vec<f32>,
    pub spec: Vec<f32>,
}

/// Compute the normalized ERB and complex features of `spec` (reference `df_features` and
/// `DfNet.pad_feat`).
pub fn features(spec: &[Vec<Complex32>], erb: &[usize], p: &Params) -> Features {
    let t_n = spec.len();
    let (ne, nd) = (p.nb_erb, p.nb_df);
    let alpha = p.alpha();
    let lin = |a: f32, b: f32, n: usize| -> Vec<f32> {
        (0..n).map(|i| if n > 1 { a + (b - a) * i as f32 / (n - 1) as f32 } else { a }).collect()
    };
    let mut mean_state = lin(MEAN_NORM_INIT[0], MEAN_NORM_INIT[1], ne);
    let mut unit_state = lin(UNIT_NORM_INIT[0], UNIT_NORM_INIT[1], nd);
    let mut feat_erb = vec![0f32; t_n * ne];
    let mut feat_spec = vec![0f32; 2 * t_n * nd];
    let shift = p.conv_lookahead;
    for (t, x) in spec.iter().enumerate() {
        // features of frame t go to t − conv_lookahead (dropped when negative)
        let dst = t.checked_sub(shift);
        let mut start = 0;
        for (b, (&w, s)) in erb.iter().zip(mean_state.iter_mut()).enumerate() {
            let pow: f32 = x[start..start + w].iter().map(|c| c.norm_sqr()).sum::<f32>() / w as f32;
            start += w;
            let db = (pow + 1e-10).log10() * 10.0;
            *s = db * (1.0 - alpha) + *s * alpha;
            if let Some(d) = dst {
                feat_erb[d * ne + b] = (db - *s) / 40.0;
            }
        }
        for (f, s) in unit_state.iter_mut().enumerate() {
            *s = x[f].norm() * (1.0 - alpha) + *s * alpha;
            if let Some(d) = dst {
                let v = x[f] / s.sqrt();
                feat_spec[d * nd + f] = v.re;
                feat_spec[t_n * nd + d * nd + f] = v.im;
            }
        }
    }
    Features { frames: t_n, erb: feat_erb, spec: feat_spec }
}

/// Apply the ERB gains (`[T][nb_erb]`) and deep filter (`[T][nb_df][order·2]`) to the noisy
/// spectrum, returning the enhanced spectrum.
pub fn apply(
    spec: &[Vec<Complex32>],
    gains: &[f32],
    coefs: &[f32],
    erb: &[usize],
    p: &Params,
) -> Vec<Vec<Complex32>> {
    let (ne, nd, order) = (p.nb_erb, p.nb_df, p.df_order);
    let past = (order - 1 - p.df_lookahead) as isize;
    let t_n = spec.len();
    let mut out = Vec::with_capacity(t_n);
    for t in 0..t_n {
        let mut y = spec[t].clone();
        let mut start = 0;
        for (b, &w) in erb.iter().enumerate() {
            let g = gains[t * ne + b];
            for v in &mut y[start..start + w] {
                *v *= g;
            }
            start += w;
        }
        let c = &coefs[t * nd * order * 2..(t + 1) * nd * order * 2];
        for f in 0..nd {
            let mut acc = Complex32::new(0.0, 0.0);
            for n in 0..order {
                let tt = t as isize - past + n as isize;
                if tt < 0 || tt as usize >= t_n {
                    continue;
                }
                let k = (f * order + n) * 2;
                acc += spec[tt as usize][f] * Complex32::new(c[k], c[k + 1]);
            }
            y[f] = acc;
        }
        out.push(y);
    }
    out
}

/// A loaded DeepFilterNet 3.
pub struct Dfn3 {
    pub params: Params,
    erb: Vec<usize>,
    stft: Stft,
    enc: Arc<ModelSession>,
    erb_dec: Arc<ModelSession>,
    df_dec: Arc<ModelSession>,
    /// Frames per inference segment (whole sequence when shorter).
    pub segment: usize,
    warmup: usize,
    crossfade: usize,
}

/// Segmenting of long inputs (frames of 10 ms).
const SEGMENT: usize = 3000;
const WARMUP: usize = 100;
const CROSSFADE: usize = 50;
/// Frames discarded after each segment's end (convolution context).
const TAIL: usize = 8;

impl Dfn3 {
    /// Resolve `denoise/dfn3` (download policy of `ctx`, provenance), unpack it once next to
    /// the cached archive, and load the three graphs on the context's device.
    pub fn load(ctx: &mut OpCtx) -> Result<Dfn3> {
        let m = ferraus_models::ensure_model(ctx, MODEL)?;
        let dir = m.path.with_file_name("extracted");
        let paths = ferraus_ml::archive::extract_members(&m.path, &dir, &MEMBERS)?;
        let hints = m.spec.provider_hints.clone();
        let params = Params::from_ini(&std::fs::read_to_string(&paths[3])?)?;
        let enc = load_for_hinted(ctx, &paths[0], &hints)?;
        let erb_dec = load_for_hinted(ctx, &paths[1], &hints)?;
        let df_dec = load_for_hinted(ctx, &paths[2], &hints)?;
        Ok(Dfn3::new(params, enc, erb_dec, df_dec))
    }

    /// Load from an unpacked directory (tests, tools).
    pub fn load_dir(dir: &Path, opts: &ferraus_ml::SessionOptions) -> Result<Dfn3> {
        let params = Params::from_ini(&std::fs::read_to_string(dir.join("config.ini"))?)?;
        let enc = ferraus_ml::load(&dir.join("enc.onnx"), opts)?.0;
        let erb_dec = ferraus_ml::load(&dir.join("erb_dec.onnx"), opts)?.0;
        let df_dec = ferraus_ml::load(&dir.join("df_dec.onnx"), opts)?.0;
        Ok(Dfn3::new(params, enc, erb_dec, df_dec))
    }

    fn new(params: Params, enc: Arc<ModelSession>, erb_dec: Arc<ModelSession>, df_dec: Arc<ModelSession>) -> Dfn3 {
        let erb = erb_widths(params.sr, params.fft_size, params.nb_erb, params.min_nb_erb_freqs);
        let stft = Stft::new(params.fft_size, params.hop_size);
        Dfn3 { params, erb, stft, enc, erb_dec, df_dec, segment: SEGMENT, warmup: WARMUP, crossfade: CROSSFADE }
    }

    /// Run the three graphs on frames `a..b` of the features. Returns gains `[b−a][nb_erb]`
    /// and coefficients `[b−a][nb_df][order·2]`.
    fn infer(&self, feat: &Features, a: usize, b: usize) -> Result<(Vec<f32>, Vec<f32>)> {
        let (ne, nd) = (self.params.nb_erb, self.params.nb_df);
        let s = b - a;
        let erb = feat.erb[a * ne..b * ne].to_vec();
        let mut spec = Vec::with_capacity(2 * s * nd);
        for ch in 0..2 {
            let off = ch * feat.frames * nd;
            spec.extend_from_slice(&feat.spec[off + a * nd..off + b * nd]);
        }
        let enc = self.enc.run(vec![
            TensorF32::new("feat_erb", vec![1, 1, s as i64, ne as i64], erb),
            TensorF32::new("feat_spec", vec![1, 2, s as i64, nd as i64], spec),
        ])?;
        let get = |name: &str| -> Result<TensorF32> {
            enc.iter()
                .find(|t| t.name == name)
                .cloned()
                .ok_or_else(|| Error::processing(format!("DFN3 encoder produced no '{name}' output")))
        };
        let m = self.erb_dec.run(vec![get("emb")?, get("e3")?, get("e2")?, get("e1")?, get("e0")?])?;
        let c = self.df_dec.run(vec![get("emb")?, get("c0")?])?;
        let gains = m.into_iter().next().ok_or_else(|| Error::processing("DFN3 ERB decoder has no output"))?;
        let coefs = c
            .into_iter()
            .find(|t| t.name == "coefs")
            .ok_or_else(|| Error::processing("DFN3 DF decoder produced no 'coefs' output"))?;
        let want_c = s * nd * self.params.df_order * 2;
        if gains.data.len() != s * ne || coefs.data.len() != want_c {
            return Err(Error::processing(format!(
                "DFN3 decoders returned shapes {:?} and {:?} for {s} frames",
                gains.shape, coefs.shape
            )));
        }
        Ok((gains.data, coefs.data))
    }

    /// Gains and coefficients for every frame, segmenting long inputs.
    fn infer_all(&self, feat: &Features, tick: &mut dyn FnMut(f64) -> Result<()>) -> Result<(Vec<f32>, Vec<f32>)> {
        let t_n = feat.frames;
        let (ne, nc) = (self.params.nb_erb, self.params.nb_df * self.params.df_order * 2);
        if t_n <= self.segment {
            let r = self.infer(feat, 0, t_n)?;
            tick(1.0)?;
            return Ok(r);
        }
        let mut gains = vec![0f32; t_n * ne];
        let mut coefs = vec![0f32; t_n * nc];
        let mut wsum = vec![0f32; t_n];
        let step = self.segment - self.crossfade;
        let mut start = 0;
        while start < t_n {
            let end = (start + self.segment).min(t_n);
            let a = start.saturating_sub(self.warmup);
            let b = (end + TAIL).min(t_n);
            let (g, c) = self.infer(feat, a, b)?;
            for t in start..end {
                // linear crossfade over the overlap with the neighbouring segments
                let mut w = 1.0f32;
                if start > 0 && t < start + self.crossfade {
                    w = (t - start) as f32 / self.crossfade as f32 + 0.5 / self.crossfade as f32;
                }
                if end < t_n && t >= end - self.crossfade {
                    w = w.min((end - t) as f32 / self.crossfade as f32 - 0.5 / self.crossfade as f32);
                }
                let lt = t - a;
                wsum[t] += w;
                for (o, v) in gains[t * ne..(t + 1) * ne].iter_mut().zip(&g[lt * ne..(lt + 1) * ne]) {
                    *o += w * v;
                }
                for (o, v) in coefs[t * nc..(t + 1) * nc].iter_mut().zip(&c[lt * nc..(lt + 1) * nc]) {
                    *o += w * v;
                }
            }
            tick(end as f64 / t_n as f64)?;
            if end == t_n {
                break;
            }
            start += step;
        }
        for t in 0..t_n {
            let w = wsum[t].max(1e-6);
            gains[t * ne..(t + 1) * ne].iter_mut().for_each(|v| *v /= w);
            coefs[t * nc..(t + 1) * nc].iter_mut().for_each(|v| *v /= w);
        }
        Ok((gains, coefs))
    }

    /// Enhance one 48 kHz channel; the output has the input's length and timing.
    pub fn enhance_channel(&self, x: &[f32], tick: &mut dyn FnMut(f64) -> Result<()>) -> Result<Vec<f32>> {
        if x.is_empty() {
            return Ok(Vec::new());
        }
        let p = &self.params;
        let mut padded = x.to_vec();
        padded.resize(x.len() + p.fft_size, 0.0);
        let spec = self.stft.analysis(&padded);
        let feat = features(&spec, &self.erb, p);
        let (gains, coefs) = self.infer_all(&feat, tick)?;
        let mut enhanced = apply(&spec, &gains, &coefs, &self.erb, p);
        let y = self.stft.synthesis(&mut enhanced);
        let d = p.fft_size - p.hop_size;
        Ok((0..x.len()).map(|i| y.get(i + d).copied().unwrap_or(0.0)).collect())
    }

    /// Enhance every channel independently (resampled to 48 kHz and back).
    pub fn enhance(&self, buf: &AudioBuffer, tick: &mut dyn FnMut(f64) -> Result<()>) -> Result<AudioBuffer> {
        let rate = self.params.sr as u32;
        let x = resample(buf, rate, Quality::High);
        let channels = x.channels().max(1);
        let mut data = Vec::with_capacity(x.channels());
        for (c, ch) in x.data.iter().enumerate() {
            let mut sub = |f: f64| tick((c as f64 + f) / channels as f64);
            data.push(self.enhance_channel(ch, &mut sub)?);
        }
        let y = AudioBuffer::from_planar(rate, data);
        let mut y = resample(&y, buf.sample_rate, Quality::High);
        for c in &mut y.data {
            c.resize(buf.frames(), 0.0);
        }
        Ok(y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erb_bands_match_libdf() {
        let p = Params::default();
        let e = erb_widths(p.sr, p.fft_size, p.nb_erb, p.min_nb_erb_freqs);
        assert_eq!(e.len(), 32);
        assert_eq!(e.iter().sum::<usize>(), 481);
        assert!(e.iter().all(|w| *w >= 2));
        // low bands are the 2-bin minimum, high bands are wide
        assert_eq!(&e[..4], &[2, 2, 2, 2]);
        assert!(e[31] > 30, "{e:?}");
        assert!(e.windows(2).all(|w| w[1] >= w[0]), "{e:?}");
    }

    #[test]
    fn alpha_is_rounded_like_the_reference() {
        assert_eq!(Params::default().alpha(), 0.99);
    }

    #[test]
    fn config_ini_parses() {
        let ini = "[train]\nseed = 43\n[df]\nsr = 48000\nfft_size = 960\nhop_size = 480\nnb_erb = 32\nnb_df = 96\nnorm_tau = 1\ndf_order = 5\ndf_lookahead = 2\n[deepfilternet]\nconv_lookahead = 2\n";
        assert_eq!(Params::from_ini(ini).unwrap(), Params::default());
        assert!(Params::from_ini("[df]\nhop_size = 240\n").is_err());
        assert!(Params::from_ini("[df]\nnb_df = x\n").is_err());
    }

    #[test]
    fn stft_reconstructs_perfectly() {
        let p = Params::default();
        let stft = Stft::new(p.fft_size, p.hop_size);
        let x: Vec<f32> = (0..48_000).map(|i| ((i as f32) * 0.031).sin() * 0.5 + ((i * 7 % 11) as f32 - 5.0) * 0.01).collect();
        let mut padded = x.clone();
        padded.resize(x.len() + p.fft_size, 0.0);
        let mut spec = stft.analysis(&padded);
        let y = stft.synthesis(&mut spec);
        let d = p.fft_size - p.hop_size;
        let err = x.iter().enumerate().map(|(i, v)| (v - y[i + d]).abs()).fold(0f32, f32::max);
        assert!(err < 1e-5, "max reconstruction error {err}");
        // analysis normalization: a full-scale sine at a bin centre reads about 0.5·Σw/N
        let f = 100.0 * 48_000.0 / 960.0;
        let s: Vec<f32> = (0..9600).map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / 48_000.0).sin() as f32).collect();
        let sp = stft.analysis(&s);
        let mag = sp[5][100].norm();
        let wsum: f32 = vorbis_window(960).iter().sum();
        assert!((mag - 0.5 * wsum / 960.0).abs() < 1e-3, "{mag}");
    }

    #[test]
    fn unity_gains_and_identity_filter_return_the_input() {
        let p = Params::default();
        let erb = erb_widths(p.sr, p.fft_size, p.nb_erb, p.min_nb_erb_freqs);
        let stft = Stft::new(p.fft_size, p.hop_size);
        let x: Vec<f32> = (0..9600).map(|i| ((i as f32) * 0.05).sin()).collect();
        let spec = stft.analysis(&x);
        let t = spec.len();
        let gains = vec![1f32; t * p.nb_erb];
        // identity filter: coefficient 1 on the current frame (n = order − 1 − lookahead)
        let mut coefs = vec![0f32; t * p.nb_df * p.df_order * 2];
        let cur = p.df_order - 1 - p.df_lookahead;
        for tt in 0..t {
            for f in 0..p.nb_df {
                coefs[((tt * p.nb_df + f) * p.df_order + cur) * 2] = 1.0;
            }
        }
        let y = apply(&spec, &gains, &coefs, &erb, &p);
        for (a, b) in y.iter().zip(&spec) {
            for (u, v) in a.iter().zip(b) {
                assert!((u - v).norm() < 1e-6);
            }
        }
        // a one-frame delay filter picks the previous frame for the DF bins only
        let mut delay = vec![0f32; coefs.len()];
        for tt in 0..t {
            for f in 0..p.nb_df {
                delay[((tt * p.nb_df + f) * p.df_order + cur - 1) * 2] = 1.0;
            }
        }
        let y = apply(&spec, &vec![0.5; t * p.nb_erb], &delay, &erb, &p);
        assert!((y[10][5] - spec[9][5]).norm() < 1e-6);
        assert!((y[10][200] - spec[10][200] * 0.5).norm() < 1e-6);
    }

    #[test]
    fn features_follow_the_reference_normalization() {
        let p = Params::default();
        let erb = erb_widths(p.sr, p.fft_size, p.nb_erb, p.min_nb_erb_freqs);
        // a constant spectrum: the mean state converges to its level, so features → 0
        let spec = vec![vec![Complex32::new(0.01, 0.0); 481]; 2000];
        let f = features(&spec, &erb, &p);
        assert_eq!(f.erb.len(), 2000 * 32);
        let late = &f.erb[1900 * 32..1901 * 32];
        assert!(late.iter().all(|v| v.abs() < 1e-3), "{late:?}");
        // first frame (shifted to index −2 and dropped); index 0 holds frame 2
        let db = (1e-4f32 + 1e-10).log10() * 10.0;
        let s0 = -60.0f32;
        let s1 = db * 0.01 + s0 * 0.99;
        let s2 = db * 0.01 + s1 * 0.99;
        let s3 = db * 0.01 + s2 * 0.99;
        assert!((f.erb[0] - (db - s3) / 40.0).abs() < 1e-4, "{} vs {}", f.erb[0], (db - s3) / 40.0);
        // the last conv_lookahead frames are zero padding
        assert!(f.erb[1998 * 32..].iter().all(|v| *v == 0.0));
        // unit norm: |x| / sqrt(state) with state → |x|
        let re = f.spec[1900 * 96];
        assert!((re - 0.01 / 0.01f32.sqrt()).abs() < 1e-3, "{re}");
        assert_eq!(f.spec[2000 * 96 + 1900 * 96], 0.0);
    }
}
