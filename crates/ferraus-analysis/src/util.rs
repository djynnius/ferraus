//! Shared DSP helpers: windows, STFT, Welch spectra, biquads, mel filterbanks, FFT
//! cross-correlation, and small statistics. Public so sibling crates can reuse them.

use realfft::RealFftPlanner;
use rustfft::num_complex::Complex;
use std::f64::consts::PI;

/// Decibels of a power value, floored at -240 dB (never `-inf`).
#[inline]
pub fn pow_db(p: f64) -> f64 {
    if p <= 1e-24 {
        -240.0
    } else {
        10.0 * p.log10()
    }
}

/// Decibels of an amplitude, floored at -240 dB.
#[inline]
pub fn amp_db(a: f64) -> f64 {
    ferraus_core::lin_to_db(a)
}

/// Periodic Hann window of length `n`.
pub fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32)
        .collect()
}

/// Smallest power of two `>= n`.
pub fn next_pow2(n: usize) -> usize {
    n.max(1).next_power_of_two()
}

/// Complex STFT frames (one-sided, `n_fft/2+1` bins). Frames are centered: frame `i` is
/// centered on sample `i * hop` (the signal is zero-padded by `n_fft/2` on both sides).
pub fn stft_complex(x: &[f32], n_fft: usize, hop: usize) -> Vec<Vec<Complex<f32>>> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let win = hann(n_fft);
    let half = n_fft / 2;
    let hop = hop.max(1);
    let n_frames = if x.is_empty() { 0 } else { x.len() / hop + 1 };
    let mut input = fft.make_input_vec();
    let mut out = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut frames = Vec::with_capacity(n_frames);
    for f in 0..n_frames {
        let start = (f * hop) as isize - half as isize;
        for (k, slot) in input.iter_mut().enumerate() {
            let idx = start + k as isize;
            *slot = if idx >= 0 && (idx as usize) < x.len() {
                x[idx as usize] * win[k]
            } else {
                0.0
            };
        }
        fft.process_with_scratch(&mut input, &mut out, &mut scratch)
            .expect("fft sizes match");
        frames.push(out.clone());
    }
    frames
}

/// Magnitude STFT (see [`stft_complex`] for framing).
pub fn stft_mag(x: &[f32], n_fft: usize, hop: usize) -> Vec<Vec<f32>> {
    stft_complex(x, n_fft, hop)
        .into_iter()
        .map(|fr| fr.iter().map(|c| c.norm()).collect())
        .collect()
}

/// Welch power spectrum: mean of `|X|^2` over Hann-windowed frames with 50% overlap,
/// each frame mean-removed (so DC offset does not leak into low bins), summed over the
/// given channels and averaged. Returns `(power, scale)` where `power[k] * scale` is the
/// mean-square contribution of bin `k` (so summing over bins gives the signal's mean
/// square, excluding DC).
pub fn welch_power(channels: &[&[f32]], n_fft: usize) -> (Vec<f64>, f64) {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let win = hann(n_fft);
    let wss: f64 = win.iter().map(|w| (*w as f64).powi(2)).sum();
    let hop = n_fft / 2;
    let nb = n_fft / 2 + 1;
    let mut acc = vec![0.0f64; nb];
    let mut count = 0usize;
    let mut input = fft.make_input_vec();
    let mut out = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    for ch in channels {
        let n = ch.len();
        if n == 0 {
            continue;
        }
        let starts: Vec<usize> = if n < n_fft {
            vec![0]
        } else {
            (0..=(n - n_fft) / hop).map(|i| i * hop).collect()
        };
        for start in starts {
            let end = (start + n_fft).min(n);
            let len = end - start;
            let mean: f64 = ch[start..end].iter().map(|v| *v as f64).sum::<f64>() / len as f64;
            for k in 0..n_fft {
                input[k] = if k < len {
                    ((ch[start + k] as f64 - mean) as f32) * win[k]
                } else {
                    0.0
                };
            }
            fft.process_with_scratch(&mut input, &mut out, &mut scratch)
                .expect("fft sizes match");
            for (a, c) in acc.iter_mut().zip(out.iter()) {
                *a += c.norm_sqr() as f64;
            }
            count += 1;
        }
    }
    if count > 0 {
        for a in acc.iter_mut() {
            *a /= count as f64;
        }
    }
    // one-sided: double all but DC and Nyquist; normalise by N*sum(w^2)
    let scale = 2.0 / (n_fft as f64 * wss);
    (acc, scale)
}

/// A biquad section (direct form I, f64 state).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Biquad {
    /// RBJ cookbook low-pass.
    pub fn lowpass(sr: f64, f0: f64, q: f64) -> Biquad {
        let w = 2.0 * PI * (f0 / sr).min(0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        Biquad {
            b0: (1.0 - c) / 2.0 / a0,
            b1: (1.0 - c) / a0,
            b2: (1.0 - c) / 2.0 / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// RBJ cookbook high-pass.
    pub fn highpass(sr: f64, f0: f64, q: f64) -> Biquad {
        let w = 2.0 * PI * (f0 / sr).min(0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        Biquad {
            b0: (1.0 + c) / 2.0 / a0,
            b1: -(1.0 + c) / a0,
            b2: (1.0 + c) / 2.0 / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// Filter a signal (fresh state), returning f32.
    pub fn run(&self, x: &[f32]) -> Vec<f32> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0, 0.0, 0.0);
        x.iter()
            .map(|&v| {
                let v = v as f64;
                let y = self.b0 * v + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
                x2 = x1;
                x1 = v;
                y2 = y1;
                y1 = y;
                y as f32
            })
            .collect()
    }

    /// Filter a signal (fresh state), returning f64.
    pub fn run_f64(&self, x: &[f64]) -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0, 0.0, 0.0);
        x.iter()
            .map(|&v| {
                let y = self.b0 * v + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
                x2 = x1;
                x1 = v;
                y2 = y1;
                y1 = y;
                y
            })
            .collect()
    }
}

/// 4th-order Butterworth low-pass (two cascaded biquads).
pub fn lowpass4(x: &[f32], sr: f64, f0: f64) -> Vec<f32> {
    let a = Biquad::lowpass(sr, f0, 0.541_196_100_146_197);
    let b = Biquad::lowpass(sr, f0, 1.306_562_964_876_376_4);
    b.run(&a.run(x))
}

/// 4th-order Butterworth high-pass (two cascaded biquads).
pub fn highpass4(x: &[f32], sr: f64, f0: f64) -> Vec<f32> {
    let a = Biquad::highpass(sr, f0, 0.541_196_100_146_197);
    let b = Biquad::highpass(sr, f0, 1.306_562_964_876_376_4);
    b.run(&a.run(x))
}

/// Hz → mel (HTK formula).
pub fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}

/// Mel → Hz (HTK formula).
pub fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

/// Triangular mel filterbank: `n_mels` filters over `[fmin, fmax]` for an `n_fft` FFT.
/// Each filter is a sparse list of `(bin, weight)`; every filter covers at least one bin.
pub fn mel_filterbank(
    sr: f64,
    n_fft: usize,
    n_mels: usize,
    fmin: f64,
    fmax: f64,
) -> Vec<Vec<(usize, f32)>> {
    let nb = n_fft / 2 + 1;
    let df = sr / n_fft as f64;
    let fmax = fmax.min(sr / 2.0);
    let (m0, m1) = (hz_to_mel(fmin), hz_to_mel(fmax));
    let pts: Vec<f64> = (0..n_mels + 2)
        .map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (n_mels + 1) as f64))
        .collect();
    (0..n_mels)
        .map(|m| {
            let (lo, c, hi) = (pts[m], pts[m + 1], pts[m + 2]);
            let mut f = Vec::new();
            for k in 0..nb {
                let fk = k as f64 * df;
                let w = if fk > lo && fk <= c {
                    (fk - lo) / (c - lo)
                } else if fk > c && fk < hi {
                    (hi - fk) / (hi - c)
                } else {
                    0.0
                };
                if w > 0.0 {
                    f.push((k, w as f32));
                }
            }
            if f.is_empty() {
                let k = ((c / df).round() as usize).min(nb - 1);
                f.push((k, 1.0));
            }
            f
        })
        .collect()
}

/// Apply a sparse filterbank to one magnitude/power frame.
pub fn apply_bank(bank: &[Vec<(usize, f32)>], frame: &[f32]) -> Vec<f32> {
    bank.iter()
        .map(|f| f.iter().map(|(k, w)| frame[*k] * w).sum())
        .collect()
}

/// Type-II DCT (orthonormal) of `x`, first `n_out` coefficients.
pub fn dct2(x: &[f32], n_out: usize) -> Vec<f32> {
    let n = x.len();
    (0..n_out)
        .map(|k| {
            let s: f64 = x
                .iter()
                .enumerate()
                .map(|(i, v)| *v as f64 * (PI * k as f64 * (i as f64 + 0.5) / n as f64).cos())
                .sum();
            let norm = if k == 0 {
                (1.0 / n as f64).sqrt()
            } else {
                (2.0 / n as f64).sqrt()
            };
            (s * norm) as f32
        })
        .collect()
}

/// Full cross-correlation of `a` and `b` via FFT, searched over lags `-max_lag..=max_lag`.
/// Returns `(lag, value, norm_value)` of the peak absolute correlation where a positive
/// lag means `b` is delayed relative to `a` (`b[n] ≈ a[n - lag]`). `norm_value` is
/// normalised by the signal energies (−1..1).
pub fn xcorr_peak(a: &[f32], b: &[f32], max_lag: usize) -> (isize, f64, f64) {
    if a.is_empty() || b.is_empty() {
        return (0, 0.0, 0.0);
    }
    let n = next_pow2(a.len() + b.len());
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut ia = fwd.make_input_vec();
    let mut ib = fwd.make_input_vec();
    for (s, v) in ia.iter_mut().zip(a) {
        *s = *v as f64;
    }
    for (s, v) in ib.iter_mut().zip(b) {
        *s = *v as f64;
    }
    let mut fa = fwd.make_output_vec();
    let mut fb = fwd.make_output_vec();
    fwd.process(&mut ia, &mut fa).expect("fft");
    fwd.process(&mut ib, &mut fb).expect("fft");
    let mut prod: Vec<Complex<f64>> = fa.iter().zip(&fb).map(|(x, y)| x.conj() * y).collect();
    // DC and Nyquist bins must be real for the inverse real FFT
    if let Some(f) = prod.first_mut() {
        f.im = 0.0;
    }
    if let Some(l) = prod.last_mut() {
        l.im = 0.0;
    }
    let mut c = inv.make_output_vec();
    inv.process(&mut prod, &mut c).expect("ifft");
    let scale = 1.0 / n as f64;
    let max_pos = max_lag.min(b.len().saturating_sub(1));
    let max_neg = max_lag.min(a.len().saturating_sub(1));
    let mut best = (0isize, 0.0f64);
    for lag in 0..=max_pos {
        let v = c[lag] * scale;
        if v.abs() > best.1.abs() {
            best = (lag as isize, v);
        }
    }
    for lag in 1..=max_neg {
        let v = c[n - lag] * scale;
        if v.abs() > best.1.abs() {
            best = (-(lag as isize), v);
        }
    }
    let ea: f64 = a.iter().map(|v| (*v as f64).powi(2)).sum();
    let eb: f64 = b.iter().map(|v| (*v as f64).powi(2)).sum();
    let norm = (ea * eb).sqrt();
    (
        best.0,
        best.1,
        if norm > 0.0 { best.1 / norm } else { 0.0 },
    )
}

/// Percentile (0..100) of a slice by linear interpolation (copies and sorts).
pub fn percentile(x: &[f64], p: f64) -> f64 {
    if x.is_empty() {
        return f64::NAN;
    }
    let mut v: Vec<f64> = x.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = (p / 100.0).clamp(0.0, 1.0) * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
}

/// Median of a slice.
pub fn median(x: &[f64]) -> f64 {
    percentile(x, 50.0)
}

/// Mean of a slice (0 when empty).
pub fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f64>() / x.len() as f64
    }
}

/// Population standard deviation.
pub fn std_dev(x: &[f64]) -> f64 {
    if x.len() < 2 {
        return 0.0;
    }
    let m = mean(x);
    (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Pearson correlation of two equal-length vectors.
pub fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (ma, mb) = (mean(&a[..n]), mean(&b[..n]));
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (a[i] - ma, b[i] - mb);
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    if saa <= 0.0 || sbb <= 0.0 {
        return if saa <= 0.0 && sbb <= 0.0 { 1.0 } else { 0.0 };
    }
    sab / (saa * sbb).sqrt()
}

/// Round to a tidy number of digits for JSON output: 4 decimals, or 4 significant digits
/// for small magnitudes. Non-finite values pass through (they serialise as `null`).
pub fn tidy(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let mag = x.abs().log10().floor() as i32;
    let decimals = (3 - mag).clamp(4, 12);
    let f = 10f64.powi(decimals);
    (x * f).round() / f
}

/// Recursively round every float in a JSON value with [`tidy`].
pub fn tidy_json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Number(n) => {
            if n.is_f64() {
                if let Some(f) = n.as_f64() {
                    if let Some(t) = serde_json::Number::from_f64(tidy(f)) {
                        *n = t;
                    }
                }
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(tidy_json),
        serde_json::Value::Object(o) => o.values_mut().for_each(tidy_json),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xcorr_finds_delay() {
        let a: Vec<f32> = (0..2000).map(|i| ((i * 7919) % 101) as f32 / 50.0 - 1.0).collect();
        let mut b = vec![0.0f32; 37];
        b.extend_from_slice(&a[..2000 - 37]);
        let (lag, _, norm) = xcorr_peak(&a, &b, 200);
        assert_eq!(lag, 37);
        assert!(norm > 0.9);
        let (lag, _, _) = xcorr_peak(&b, &a, 200);
        assert_eq!(lag, -37);
    }

    #[test]
    fn welch_parseval_sine() {
        let sr = 48000.0;
        let x: Vec<f32> = (0..48000)
            .map(|i| (0.5 * (2.0 * PI * 1000.0 * i as f64 / sr).sin()) as f32)
            .collect();
        let (p, scale) = welch_power(&[&x], 4096);
        let ms: f64 = p.iter().map(|v| v * scale).sum();
        assert!((ms - 0.125).abs() < 0.005, "{ms}");
    }

    #[test]
    fn tidy_rounds() {
        assert_eq!(tidy(-23.012345678), -23.0123);
        assert_eq!(tidy(0.000123456789), 0.0001235);
    }
}
