//! Quality metrics for denoising and a naive reference gate.
//!
//! * [`birdie_score`] — the "musical noise" (birdie) metric: the fraction of
//!   time-frequency cells that jump up by more than 10 dB from the previous frame (positive
//!   spectral flux) to more than 10 dB above their bin's median level. Steady noise,
//!   attenuated or not, almost never does this; noise that flickers between gated and open
//!   bins, the chirping artifact of naive spectral gates, does it constantly. The score is
//!   scale invariant, so a uniformly attenuated noise floor scores like the original.
//! * [`hard_gate`] — a naive binary spectral gate with the same threshold as the real
//!   engine but no soft knee, smoothing, median, or floor. It exists to show what the
//!   artifact controls buy.

use crate::engine::{RAYLEIGH_LOG_BIAS_DB, RAYLEIGH_STD_DB};
use crate::profile::NoiseProfile;
use crate::stft::{fft_size_for, hop_for, Fft};
use ferraus_core::AudioBuffer;
use realfft::num_complex::Complex32;

/// Birdie score below which a signal counts as free of musical noise (fixed; steady
/// Gaussian noise scores about 0.00006, a naive hard gate about 0.0013).
pub const BIRDIE_THRESHOLD: f64 = 0.0003;
/// Jump (dB) from the previous frame, and height (dB) above the bin's median level, that
/// make a cell a birdie.
const BIRDIE_JUMP_DB: f32 = 10.0;

/// Fraction of time-frequency cells (100 Hz to 16 kHz, half-size STFT independent of the
/// engine's framing) that pop up by more than 10 dB to more than 10 dB above their bin's
/// median level.
pub fn birdie_score(x: &[f32], sample_rate: u32) -> f64 {
    let n = (fft_size_for(sample_rate) / 2).max(256);
    let hop = n / 4;
    if x.len() < n + hop {
        return 0.0;
    }
    let mut fft = Fft::new(n);
    let bins = fft.bins();
    let bin_hz = sample_rate as f64 / n as f64;
    let lo = ((100.0 / bin_hz).ceil() as usize).max(1);
    let hi = ((16_000.0f64.min(sample_rate as f64 / 2.0) / bin_hz) as usize).min(bins - 1);
    let mut spec = vec![Complex32::new(0.0, 0.0); bins];
    let mut levels: Vec<Vec<f32>> = Vec::new();
    let mut s = 0;
    while s + n <= x.len() {
        fft.forward(&x[s..s + n], &mut spec);
        levels.push(spec[lo..=hi].iter().map(|z| 10.0 * z.norm_sqr().max(1e-24).log10()).collect());
        s += hop;
    }
    let width = hi - lo + 1;
    let mut medians = vec![0f32; width];
    let mut col = Vec::with_capacity(levels.len());
    for (k, m) in medians.iter_mut().enumerate() {
        col.clear();
        col.extend(levels.iter().map(|f| f[k]));
        col.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        *m = col[col.len() / 2];
    }
    let mut count = 0usize;
    for t in 1..levels.len() {
        for k in 0..width {
            let l = levels[t][k];
            if l - levels[t - 1][k] > BIRDIE_JUMP_DB && l - medians[k] > BIRDIE_JUMP_DB {
                count += 1;
            }
        }
    }
    count as f64 / ((levels.len() - 1) * width) as f64
}

/// Naive hard spectral gate: bins whose power is above `mean power + sensitivity × std /
/// 5.57 dB` pass unchanged, every other bin is multiplied by `-reduction`. No smoothing.
pub fn hard_gate(
    buf: &AudioBuffer,
    profile: &NoiseProfile,
    reduction_db: f32,
    sensitivity: f32,
) -> AudioBuffer {
    let n = fft_size_for(buf.sample_rate);
    let hop = hop_for(n);
    let mut fft = Fft::new(n);
    let bins = fft.bins();
    let gmin = 10f32.powf(-reduction_db.abs() / 20.0);
    let (mean, std) = (&profile.linked.mean_db, &profile.linked.std_db);
    let thr: Vec<f32> = (0..bins.min(mean.len()))
        .map(|k| mean[k] + RAYLEIGH_LOG_BIAS_DB + sensitivity * std[k] / RAYLEIGH_STD_DB)
        .collect();
    let scale2 = fft.mag_scale * fft.mag_scale;
    let total = buf.frames();
    let mut spec = vec![Complex32::new(0.0, 0.0); bins];
    let data = buf
        .data
        .iter()
        .map(|ch| {
            // pad n - hop zeros in front so every sample gets full overlap
            let pad = n - hop;
            let mut x = vec![0f32; pad];
            x.extend_from_slice(ch);
            x.resize(pad + total + n, 0.0);
            let mut y = vec![0f32; x.len() + n];
            let mut s = 0;
            while s + n <= x.len() {
                fft.forward(&x[s..s + n], &mut spec);
                for (k, z) in spec.iter_mut().enumerate() {
                    let p = z.norm_sqr() * scale2;
                    let db = 10.0 * p.max(1e-16).log10();
                    if k >= thr.len() || db <= thr[k] {
                        *z *= gmin;
                    }
                }
                let t = fft.inverse_windowed(&mut spec);
                for (o, v) in y[s..s + n].iter_mut().zip(t) {
                    *o += v;
                }
                s += hop;
            }
            y[pad..pad + total].to_vec()
        })
        .collect();
    AudioBuffer::from_planar(buf.sample_rate, data)
}
