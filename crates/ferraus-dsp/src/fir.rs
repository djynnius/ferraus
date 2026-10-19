//! Linear-phase FIR design (frequency sampling + Kaiser window) and a uniformly partitioned
//! overlap-save FFT convolver, used by `eq --linear-phase`, `match-eq`, and `reverb --ir`.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Design a linear-phase (symmetric) FIR with `taps` coefficients (forced odd) whose
/// magnitude follows `mag(freq_hz)` (linear gain). The group delay is `(taps-1)/2` samples.
pub fn design_linear_phase(taps: usize, sample_rate: f64, mag: impl Fn(f64) -> f64) -> Vec<f32> {
    let taps = taps.max(3) | 1;
    let m = (taps * 4).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let inv = planner.plan_fft_inverse(m);
    let mut spec: Vec<Complex<f64>> = (0..=m / 2)
        .map(|k| Complex::new(mag(k as f64 * sample_rate / m as f64), 0.0))
        .collect();
    // DC and Nyquist bins must be real
    spec[0].im = 0.0;
    spec[m / 2].im = 0.0;
    let mut h = vec![0.0f64; m];
    inv.process(&mut spec, &mut h).expect("fft sizes match");
    let half = (taps - 1) / 2;
    let beta = 6.0;
    let i0b = bessel_i0(beta);
    (0..taps)
        .map(|i| {
            let k = i as isize - half as isize;
            let v = h[k.rem_euclid(m as isize) as usize] / m as f64;
            let r = k as f64 / (half as f64 + 1.0);
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            (v * w) as f32
        })
        .collect()
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..60 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// Magnitude response (dB) of an FIR at `freq`.
pub fn fir_magnitude_db(h: &[f32], freq: f64, sample_rate: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * freq / sample_rate;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, c) in h.iter().enumerate() {
        let (s, co) = (w * n as f64).sin_cos();
        re += *c as f64 * co;
        im -= *c as f64 * s;
    }
    10.0 * (re * re + im * im).max(1e-300).log10()
}

/// Uniformly partitioned overlap-save convolver (streaming, block size `block`).
pub struct PartitionedConvolver {
    block: usize,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    /// IR partitions in the frequency domain.
    parts: Vec<Vec<Complex<f32>>>,
    /// Frequency-domain delay line of past input blocks (ring).
    fdl: Vec<Vec<Complex<f32>>>,
    fdl_pos: usize,
    /// Last two input blocks (time domain).
    input: Vec<f32>,
    scratch_t: Vec<f32>,
    acc: Vec<Complex<f32>>,
    scratch_f: Vec<Complex<f32>>,
}

impl PartitionedConvolver {
    /// `block` is rounded up to a power of two (≥ 32).
    pub fn new(ir: &[f32], block: usize) -> PartitionedConvolver {
        let block = block.max(32).next_power_of_two();
        let n = 2 * block;
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let nparts = ir.len().div_ceil(block).max(1);
        let mut parts = Vec::with_capacity(nparts);
        let mut t = vec![0.0f32; n];
        for p in 0..nparts {
            t.iter_mut().for_each(|v| *v = 0.0);
            let start = p * block;
            let end = (start + block).min(ir.len());
            if start < end {
                t[..end - start].copy_from_slice(&ir[start..end]);
            }
            let mut f = fwd.make_output_vec();
            fwd.process(&mut t, &mut f).expect("fft sizes match");
            parts.push(f);
        }
        let bins = block + 1;
        PartitionedConvolver {
            block,
            fwd,
            inv,
            fdl: vec![vec![Complex::new(0.0, 0.0); bins]; nparts],
            fdl_pos: 0,
            parts,
            input: vec![0.0; n],
            scratch_t: vec![0.0; n],
            acc: vec![Complex::new(0.0, 0.0); bins],
            scratch_f: vec![Complex::new(0.0, 0.0); bins],
        }
    }

    /// Choose a block size for offline use: about eight partitions, within 256..32768.
    pub fn offline_block(ir_len: usize) -> usize {
        (ir_len / 8).clamp(256, 32768).next_power_of_two()
    }

    pub fn block(&self) -> usize {
        self.block
    }

    /// Convolve exactly `block` input samples into `out` (same length).
    pub fn process_block(&mut self, input: &[f32], out: &mut [f32]) {
        let b = self.block;
        debug_assert_eq!(input.len(), b);
        self.input.copy_within(b.., 0);
        self.input[b..].copy_from_slice(input);
        self.scratch_t.copy_from_slice(&self.input);
        let np = self.parts.len();
        self.fdl_pos = (self.fdl_pos + np - 1) % np;
        let slot = &mut self.fdl[self.fdl_pos];
        self.fwd
            .process(&mut self.scratch_t, slot)
            .expect("fft sizes match");
        self.acc.iter_mut().for_each(|c| *c = Complex::new(0.0, 0.0));
        for p in 0..np {
            let x = &self.fdl[(self.fdl_pos + p) % np];
            let h = &self.parts[p];
            for ((a, xv), hv) in self.acc.iter_mut().zip(x).zip(h) {
                *a += xv * hv;
            }
        }
        self.scratch_f.copy_from_slice(&self.acc);
        self.scratch_f[0].im = 0.0;
        self.scratch_f[b].im = 0.0;
        self.inv
            .process(&mut self.scratch_f, &mut self.scratch_t)
            .expect("fft sizes match");
        let scale = 1.0 / (2 * b) as f32;
        for (o, v) in out.iter_mut().zip(&self.scratch_t[b..]) {
            *o = v * scale;
        }
    }

    /// Convolve a whole signal, returning `out_len` samples of `x * ir`.
    pub fn convolve(ir: &[f32], x: &[f32], out_len: usize) -> Vec<f32> {
        let mut c = PartitionedConvolver::new(ir, Self::offline_block(ir.len()));
        let b = c.block;
        let mut out = Vec::with_capacity(out_len + b);
        let mut inb = vec![0.0f32; b];
        let mut ob = vec![0.0f32; b];
        let mut pos = 0;
        while out.len() < out_len {
            for (i, v) in inb.iter_mut().enumerate() {
                *v = x.get(pos + i).copied().unwrap_or(0.0);
            }
            pos += b;
            c.process_block(&inb, &mut ob);
            out.extend_from_slice(&ob);
        }
        out.truncate(out_len);
        out
    }
}

/// Apply a linear-phase FIR to `x` in place with the group delay removed (same length).
pub fn filter_linear_phase(h: &[f32], x: &mut [f32]) {
    let delay = (h.len() - 1) / 2;
    let y = PartitionedConvolver::convolve(h, x, x.len() + delay);
    x.copy_from_slice(&y[delay..delay + x.len()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convolver_matches_direct() {
        let ir: Vec<f32> = (0..300).map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5).collect();
        let x: Vec<f32> = (0..2000).map(|i| ((i * 104729) % 97) as f32 / 97.0 - 0.5).collect();
        let y = PartitionedConvolver::convolve(&ir, &x, x.len() + ir.len() - 1);
        for n in [0usize, 1, 299, 1000, 2298] {
            let mut d = 0.0f64;
            for k in 0..ir.len() {
                if n >= k && n - k < x.len() {
                    d += ir[k] as f64 * x[n - k] as f64;
                }
            }
            assert!((y[n] as f64 - d).abs() < 1e-3, "n={n}: {} vs {d}", y[n]);
        }
    }

    #[test]
    fn designed_fir_follows_target() {
        let sr = 48000.0;
        let h = design_linear_phase(4095, sr, |f| if f < 1000.0 { 2.0 } else { 1.0 });
        assert!((fir_magnitude_db(&h, 200.0, sr) - 6.02).abs() < 0.2);
        assert!(fir_magnitude_db(&h, 5000.0, sr).abs() < 0.2);
        // symmetric
        assert!((h[0] - h[h.len() - 1]).abs() < 1e-9);
    }
}
