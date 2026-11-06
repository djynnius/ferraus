//! Short-time Fourier transform helpers: windows, analysis, weighted overlap-add synthesis
//! with exact reconstruction for any hop, Welch long-term spectra, and fractional-octave
//! smoothing.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Periodic Hann window of length `n`.
pub fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect()
}

/// STFT analysis/synthesis engine with a square-root Hann window on both sides.
pub struct Stft {
    pub size: usize,
    pub hop: usize,
    window: Vec<f32>,
    /// Overlap-add normalization for hops that divide `size`: the sum of the squared
    /// windows that overlap any one sample.
    ola_norm: f32,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
}

impl Stft {
    /// `size` must be even; `hop` ≤ `size / 2` for good reconstruction.
    pub fn new(size: usize, hop: usize) -> Stft {
        let size = size.max(16) & !1;
        let hop = hop.clamp(1, size);
        let mut planner = RealFftPlanner::<f32>::new();
        let window: Vec<f32> = hann(size).into_iter().map(|w| w.sqrt()).collect();
        let ola_norm = (window.iter().map(|w| (*w as f64).powi(2)).sum::<f64>() / hop as f64) as f32;
        Stft { size, hop, window, ola_norm, fwd: planner.plan_fft_forward(size), inv: planner.plan_fft_inverse(size) }
    }

    /// Number of bins per frame (`size/2 + 1`).
    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// Centre frequency of bin `k`.
    pub fn bin_freq(&self, k: usize, sample_rate: f64) -> f64 {
        k as f64 * sample_rate / self.size as f64
    }

    /// Analyse `x` into frames. The signal is padded by `size` zeros on both sides so every
    /// input sample is covered by full overlap.
    pub fn analyze(&self, x: &[f32]) -> Vec<Vec<Complex<f32>>> {
        let n = self.size;
        let total = x.len() + 2 * n;
        let mut frames = Vec::new();
        let mut buf = vec![0.0f32; n];
        let mut start = 0usize;
        while start + n <= total {
            for (i, b) in buf.iter_mut().enumerate() {
                let idx = start + i;
                let s = if idx >= n && idx - n < x.len() { x[idx - n] } else { 0.0 };
                *b = s * self.window[i];
            }
            let mut spec = self.fwd.make_output_vec();
            self.fwd.process(&mut buf, &mut spec).expect("fft sizes match");
            frames.push(spec);
            start += self.hop;
        }
        frames
    }

    /// Number of frames [`Stft::analyze`] returns for a signal of `len` samples.
    pub fn frame_count(&self, len: usize) -> usize {
        (len + self.size) / self.hop + 1
    }

    /// Frame `index` of [`Stft::analyze`]`(x)` alone, into `out` (`bins()` long); `buf` is
    /// `size` samples of scratch. Lets long signals stream frame by frame.
    pub fn analyze_frame(&self, x: &[f32], index: usize, buf: &mut [f32], out: &mut [Complex<f32>]) {
        let n = self.size;
        let start = index * self.hop;
        for (i, b) in buf.iter_mut().enumerate() {
            let idx = start + i;
            let s = if idx >= n && idx - n < x.len() { x[idx - n] } else { 0.0 };
            *b = s * self.window[i];
        }
        self.fwd.process(buf, out).expect("fft sizes match");
    }

    /// Inverse of [`Stft::analyze_frame`]: overlap-add `spec` as frame `index` into `out`
    /// (the signal, `len` samples). Summing every frame this way reconstructs the signal
    /// exactly like [`Stft::synthesize`] when `hop` divides `size` (the overlap is then
    /// constant); frames left out contribute nothing, so a signal of differences can be
    /// built from only the frames that differ. `buf` is `size` samples of scratch.
    pub fn overlap_add_frame(&self, spec: &mut [Complex<f32>], index: usize, buf: &mut [f32], out: &mut [f32]) {
        let n = self.size;
        spec[0].im = 0.0;
        if let Some(last) = spec.last_mut() {
            last.im = 0.0;
        }
        self.inv.process(spec, buf).expect("fft sizes match");
        let scale = 1.0 / (n as f32 * self.ola_norm);
        let start = index * self.hop;
        for i in 0..n {
            let j = start + i;
            if j >= n && j - n < out.len() {
                out[j - n] += buf[i] * self.window[i] * scale;
            }
        }
    }

    /// Overlap-add frames back to a signal of length `len` (inverse of [`Stft::analyze`]).
    pub fn synthesize(&self, frames: &mut [Vec<Complex<f32>>], len: usize) -> Vec<f32> {
        let n = self.size;
        let total = len + 2 * n;
        let mut out = vec![0.0f32; total + n];
        let mut norm = vec![0.0f32; total + n];
        let mut buf = vec![0.0f32; n];
        for (k, spec) in frames.iter_mut().enumerate() {
            spec[0].im = 0.0;
            if let Some(last) = spec.last_mut() {
                last.im = 0.0;
            }
            self.inv.process(spec, &mut buf).expect("fft sizes match");
            let start = k * self.hop;
            for i in 0..n {
                let w = self.window[i];
                out[start + i] += buf[i] * w / n as f32;
                norm[start + i] += w * w;
            }
        }
        (0..len)
            .map(|i| {
                let j = i + n;
                if norm[j] > 1e-6 {
                    out[j] / norm[j]
                } else {
                    0.0
                }
            })
            .collect()
    }
}

/// Long-term average power spectrum (Welch, Hann, 50% overlap). Returns `size/2+1` powers.
pub fn welch_psd(x: &[f32], size: usize) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(size);
    let w = hann(size);
    let hop = size / 2;
    let mut acc = vec![0.0f64; size / 2 + 1];
    let mut count = 0usize;
    let mut buf = vec![0.0f32; size];
    let mut spec = fwd.make_output_vec();
    let mut start = 0usize;
    loop {
        if start >= x.len() && count > 0 {
            break;
        }
        for (i, b) in buf.iter_mut().enumerate() {
            *b = x.get(start + i).copied().unwrap_or(0.0) * w[i];
        }
        fwd.process(&mut buf, &mut spec).expect("fft sizes match");
        for (a, c) in acc.iter_mut().zip(&spec) {
            *a += c.norm_sqr() as f64;
        }
        count += 1;
        start += hop;
        if start + size / 2 > x.len() {
            break;
        }
    }
    for a in &mut acc {
        *a /= count.max(1) as f64;
    }
    acc
}

/// Fractional-octave smoothing of a per-bin spectrum (power or dB values) with a
/// rectangular window `fraction` octaves wide (e.g. 1/3). Bin `k` is at `k·df` Hz.
pub fn smooth_octave(values: &[f64], df: f64, fraction: f64) -> Vec<f64> {
    let n = values.len();
    let mut prefix = vec![0.0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + values[i];
    }
    let half = 2f64.powf(fraction / 2.0);
    (0..n)
        .map(|k| {
            if k == 0 {
                return values[0];
            }
            let f = k as f64 * df;
            let lo = ((f / half) / df).floor().max(1.0) as usize;
            let hi = (((f * half) / df).ceil() as usize).min(n - 1).max(lo);
            (prefix[hi + 1] - prefix[lo]) / (hi + 1 - lo) as f64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stft_round_trip() {
        let x: Vec<f32> = (0..5000).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
        let s = Stft::new(512, 128);
        let mut f = s.analyze(&x);
        let y = s.synthesize(&mut f, x.len());
        let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-4, "{err}");
    }

    #[test]
    fn streaming_frames_match_the_batch_transform() {
        let x: Vec<f32> =
            (0..3001).map(|i| ((i as f32) * 0.031).sin() * 0.4 + ((i * 7 % 13) as f32 - 6.0) * 0.01).collect();
        let s = Stft::new(256, 64);
        let frames = s.analyze(&x);
        assert_eq!(s.frame_count(x.len()), frames.len());
        let mut buf = vec![0.0f32; s.size];
        let mut spec = vec![Complex::new(0.0f32, 0.0); s.bins()];
        let mut y = vec![0.0f32; x.len()];
        for (t, f) in frames.iter().enumerate() {
            s.analyze_frame(&x, t, &mut buf, &mut spec);
            assert_eq!(&spec, f, "frame {t}");
            s.overlap_add_frame(&mut spec, t, &mut buf, &mut y);
        }
        let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-5, "{err}");
    }
}
