//! Small shared helpers: windows, FFT plans, a deterministic PRNG, statistics.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::f64::consts::PI;
use std::sync::Arc;

/// Periodic Hann window (sums to a constant at 50 % overlap).
pub fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32).collect()
}

/// Forward and inverse real FFT of one size with scratch buffers.
pub struct Fft {
    pub n: usize,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    pub time: Vec<f32>,
    pub freq: Vec<Complex<f32>>,
    scratch_f: Vec<Complex<f32>>,
    scratch_i: Vec<Complex<f32>>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let mut p = RealFftPlanner::<f32>::new();
        let fwd = p.plan_fft_forward(n);
        let inv = p.plan_fft_inverse(n);
        let scratch_f = fwd.make_scratch_vec();
        let scratch_i = inv.make_scratch_vec();
        Fft { n, time: vec![0.0; n], freq: vec![Complex::new(0.0, 0.0); n / 2 + 1], fwd, inv, scratch_f, scratch_i }
    }

    /// `self.time` → `self.freq` (time buffer is clobbered).
    pub fn forward(&mut self) {
        let _ = self.fwd.process_with_scratch(&mut self.time, &mut self.freq, &mut self.scratch_f);
    }

    /// `self.freq` → `self.time`, unnormalized (scaled by `n`). Imaginary parts of the DC
    /// and Nyquist bins are zeroed first, as the real inverse requires.
    pub fn inverse(&mut self) {
        self.freq[0].im = 0.0;
        let last = self.freq.len() - 1;
        self.freq[last].im = 0.0;
        let _ = self.inv.process_with_scratch(&mut self.freq, &mut self.time, &mut self.scratch_i);
    }

    /// Forward transform of an external buffer into an external spectrum.
    pub fn forward_into(&mut self, input: &mut [f32], out: &mut [Complex<f32>]) {
        let _ = self.fwd.process_with_scratch(input, out, &mut self.scratch_f);
    }
}

/// Deterministic PRNG (SplitMix64) for seeded stochastic steps.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform in [-1, 1).
    pub fn bipolar(&mut self) -> f64 {
        self.next_f64() * 2.0 - 1.0
    }
}

/// Median of a slice (copied); `None` when empty.
pub fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s: Vec<f64> = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    Some(if n % 2 == 1 { s[n / 2] } else { 0.5 * (s[n / 2 - 1] + s[n / 2]) })
}

/// Wrap a phase to (-π, π].
#[inline]
pub fn princarg(p: f64) -> f64 {
    let two_pi = 2.0 * PI;
    p - two_pi * ((p + PI) / two_pi).floor()
}

/// Linear interpolation of a per-frame curve at fractional frame index `pos`.
pub fn interp(curve: &[f64], pos: f64) -> f64 {
    if curve.is_empty() {
        return 0.0;
    }
    if pos <= 0.0 {
        return curve[0];
    }
    let i = pos.floor() as usize;
    if i + 1 >= curve.len() {
        return curve[curve.len() - 1];
    }
    let f = pos - i as f64;
    curve[i] * (1.0 - f) + curve[i + 1] * f
}

/// Cents between two frequencies (`b` relative to `a`).
#[cfg(test)]
pub fn cents(a_hz: f64, b_hz: f64) -> f64 {
    1200.0 * (b_hz / a_hz).log2()
}

/// Mono mix of planar channels.
pub fn mono(data: &[Vec<f32>]) -> Vec<f32> {
    match data.len() {
        0 => Vec::new(),
        1 => data[0].clone(),
        n => {
            let len = data.iter().map(|c| c.len()).max().unwrap_or(0);
            let mut out = vec![0.0f32; len];
            for c in data {
                for (o, v) in out.iter_mut().zip(c) {
                    *o += *v;
                }
            }
            let g = 1.0 / n as f32;
            out.iter_mut().for_each(|v| *v *= g);
            out
        }
    }
}
