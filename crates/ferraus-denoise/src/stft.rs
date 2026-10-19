//! STFT framing shared by the profile learner, the gate engine, and the metrics.
//!
//! Frames use a periodic Hann window with 75% overlap. Magnitudes are scaled by
//! `2 / sum(window)` so a full-scale sine reads 0 dB at its bin; profiles store levels on
//! this scale (`magnitude_scale: "dbfs_sine"`).

pub use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Floor for levels in dB (digital silence reads this instead of -inf).
pub const FLOOR_DB: f32 = -160.0;
/// Floor for power values (10^(FLOOR_DB/10)).
pub const FLOOR_POW: f32 = 1e-16;

/// FFT size for a sample rate: 2048 at 44.1/48 kHz, scaled by powers of two with the rate
/// (1024 at 22.05/24 kHz, 4096 at 88.2/96 kHz, 512 at 16 kHz).
pub fn fft_size_for(sample_rate: u32) -> usize {
    let ratio = sample_rate.max(1) as f64 / 46_050.0; // geometric mean of 44.1k and 48k
    let e = ratio.log2().round() as i32;
    (2048f64 * 2f64.powi(e)).round().clamp(256.0, 16384.0) as usize
}

/// Hop for an FFT size (75% overlap).
pub fn hop_for(n_fft: usize) -> usize {
    n_fft / 4
}

/// Periodic Hann window (constant overlap-add at hop n/4 for both w and w²).
pub fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = std::f64::consts::PI * 2.0 * i as f64 / n as f64;
            (0.5 - 0.5 * x.cos()) as f32
        })
        .collect()
}

/// A planned forward/inverse real FFT pair with scratch buffers.
pub struct Fft {
    pub n: usize,
    pub window: Vec<f32>,
    /// Scale that maps |X| to the dBFS-sine magnitude scale.
    pub mag_scale: f32,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    fwd_scratch: Vec<Complex32>,
    inv_scratch: Vec<Complex32>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let window = hann(n);
        let wsum: f32 = window.iter().sum();
        Fft {
            n,
            mag_scale: 2.0 / wsum,
            fwd_scratch: fwd.make_scratch_vec(),
            inv_scratch: inv.make_scratch_vec(),
            fwd,
            inv,
            window,
            time: vec![0.0; n],
        }
    }

    pub fn bins(&self) -> usize {
        self.n / 2 + 1
    }

    /// Window `frame` (length n) and transform it into `spec` (length n/2+1).
    pub fn forward(&mut self, frame: &[f32], spec: &mut [Complex32]) {
        for ((t, x), w) in self.time.iter_mut().zip(frame).zip(&self.window) {
            *t = x * w;
        }
        self.fwd.process_with_scratch(&mut self.time, spec, &mut self.fwd_scratch).expect("fft sizes match");
    }

    /// Forward transform of a frame given as two pieces (ring-buffer order).
    pub fn forward_split(&mut self, a: &[f32], b: &[f32], spec: &mut [Complex32]) {
        for ((t, x), w) in self.time.iter_mut().zip(a.iter().chain(b)).zip(&self.window) {
            *t = x * w;
        }
        self.fwd.process_with_scratch(&mut self.time, spec, &mut self.fwd_scratch).expect("fft sizes match");
    }

    /// Inverse transform `spec` (modified in place as scratch) and return the windowed,
    /// overlap-add normalized time frame. Summing these at hop n/4 reconstructs the input.
    pub fn inverse_windowed(&mut self, spec: &mut [Complex32]) -> &[f32] {
        // DC and Nyquist must be real for the c2r transform
        spec[0].im = 0.0;
        let last = spec.len() - 1;
        spec[last].im = 0.0;
        self.inv.process_with_scratch(spec, &mut self.time, &mut self.inv_scratch).expect("fft sizes match");
        // periodic Hann: sum of w² at hop n/4 is exactly 1.5
        let norm = 1.0 / (self.n as f32 * 1.5);
        for (t, w) in self.time.iter_mut().zip(&self.window) {
            *t *= w * norm;
        }
        &self.time
    }
}

/// Convert a power value on the dBFS-sine scale to dB.
#[inline]
pub fn pow_db(p: f32) -> f32 {
    10.0 * p.max(FLOOR_POW).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_scale_with_rate() {
        assert_eq!(fft_size_for(44_100), 2048);
        assert_eq!(fft_size_for(48_000), 2048);
        assert_eq!(fft_size_for(96_000), 4096);
        assert_eq!(fft_size_for(22_050), 1024);
        assert_eq!(fft_size_for(16_000), 512);
        assert_eq!(fft_size_for(8_000), 256);
    }

    #[test]
    fn hann_squared_is_cola() {
        let n = 2048;
        let w = hann(n);
        let hop = hop_for(n);
        for i in 0..hop {
            let s: f32 = (0..4).map(|m| w[i + m * hop].powi(2)).sum();
            assert!((s - 1.5).abs() < 1e-5, "{s}");
        }
    }

    #[test]
    fn sine_reads_zero_db() {
        let n = 2048;
        let mut f = Fft::new(n);
        let sr = 48_000.0;
        let k = 100.0;
        let freq = k * sr / n as f64;
        let x: Vec<f32> = (0..n).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sr).sin() as f32).collect();
        let mut spec = vec![Complex32::new(0.0, 0.0); f.bins()];
        f.forward(&x, &mut spec);
        let db = 20.0 * (spec[100].norm() * f.mag_scale).log10();
        assert!(db.abs() < 0.01, "{db}");
    }
}
