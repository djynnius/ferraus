//! Stereo image metrics: L/R correlation (broadband and low band), mid/side balance,
//! width and correlation per band, and a mono-compatibility score.

use crate::util::{self, pow_db};
use ferraus_core::AudioBuffer;
use serde::Serialize;

/// Stereo metrics for one band.
#[derive(Clone, Debug, Serialize)]
pub struct StereoBand {
    pub low_hz: f64,
    pub high_hz: f64,
    /// L/R correlation −1..1 (1 = mono, 0 = uncorrelated, −1 = out of phase).
    pub correlation: f64,
    /// Side energy / (mid + side) energy: 0 = mono, 0.5 = uncorrelated, 1 = out of phase.
    pub width: f64,
    /// Level change of this band when summed to mono (dB, 0 for mono content).
    pub mono_loss_db: f64,
    /// Band level (mean of L and R energy) in dB relative to full scale.
    pub level_db: f64,
}

/// Result of [`stereo_analysis`].
#[derive(Clone, Debug, Serialize)]
pub struct StereoReport {
    pub channels: usize,
    /// Broadband L/R correlation.
    pub correlation: f64,
    /// L/R correlation below 120 Hz.
    pub correlation_below_120hz: f64,
    /// Side level relative to mid level (dB); `-inf` (null) for mono content.
    pub side_to_mid_db: f64,
    /// Broadband width (side / (mid + side) energy).
    pub width: f64,
    /// Level change when summed to mono (dB).
    pub mono_loss_db: f64,
    /// 0..1: energy-weighted per-band score (1 = folds to mono without loss).
    pub mono_compatibility: f64,
    /// Left level minus right level (dB).
    pub balance_db: f64,
    pub bands: Vec<StereoBand>,
}

/// Band edges used for per-band stereo metrics.
pub const STEREO_BANDS: [(f64, f64); 8] = [
    (20.0, 120.0),
    (120.0, 250.0),
    (250.0, 500.0),
    (500.0, 1000.0),
    (1000.0, 2000.0),
    (2000.0, 4000.0),
    (4000.0, 8000.0),
    (8000.0, 20000.0),
];

/// Zero-lag correlation coefficient `Σab / sqrt(Σa² Σb²)` (1.0 when both are silent).
pub fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x as f64, *y as f64);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa <= 1e-20 || bb <= 1e-20 {
        return if aa <= 1e-20 && bb <= 1e-20 { 1.0 } else { 0.0 };
    }
    ab / (aa * bb).sqrt()
}

/// L/R correlation of the content below `cutoff_hz` (4th-order low-pass). 1.0 for mono.
pub fn low_correlation(buf: &AudioBuffer, cutoff_hz: f64) -> f64 {
    if buf.channels() < 2 {
        return 1.0;
    }
    let sr = buf.sample_rate as f64;
    let l = util::lowpass4(&buf.data[0], sr, cutoff_hz);
    let r = util::lowpass4(&buf.data[1], sr, cutoff_hz);
    correlation(&l, &r)
}

/// Stereo analysis of the first two channels (mono input reports a perfect mono image).
pub fn stereo_analysis(buf: &AudioBuffer) -> StereoReport {
    let sr = buf.sample_rate as f64;
    let nyq = sr / 2.0;
    if buf.channels() < 2 {
        let e: f64 = buf.data[0].iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
            / buf.frames().max(1) as f64;
        return StereoReport {
            channels: buf.channels(),
            correlation: 1.0,
            correlation_below_120hz: 1.0,
            side_to_mid_db: f64::NEG_INFINITY,
            width: 0.0,
            mono_loss_db: 0.0,
            mono_compatibility: 1.0,
            balance_db: 0.0,
            bands: STEREO_BANDS
                .iter()
                .filter(|(lo, _)| *lo < nyq)
                .map(|(lo, hi)| StereoBand {
                    low_hz: *lo,
                    high_hz: hi.min(nyq),
                    correlation: 1.0,
                    width: 0.0,
                    mono_loss_db: 0.0,
                    level_db: pow_db(e),
                })
                .collect(),
        };
    }
    let (l, r) = (&buf.data[0], &buf.data[1]);
    let n = buf.frames().max(1) as f64;
    let (mut el, mut er, mut em, mut es) = (0.0f64, 0.0, 0.0, 0.0);
    for (a, b) in l.iter().zip(r) {
        let (a, b) = (*a as f64, *b as f64);
        el += a * a;
        er += b * b;
        em += ((a + b) / 2.0).powi(2);
        es += ((a - b) / 2.0).powi(2);
    }
    let corr = correlation(l, r);
    let corr_low = low_correlation(buf, 120.0);
    let width = if em + es > 1e-20 { es / (em + es) } else { 0.0 };
    let mono_loss = if el + er > 1e-20 {
        pow_db(2.0 * em / (el + er)).max(-120.0)
    } else {
        0.0
    };

    // per band from an STFT
    let n_fft = 4096.min(util::next_pow2(buf.frames().max(512)));
    let hop = n_fft / 2;
    let fl = util::stft_complex(l, n_fft, hop);
    let fr = util::stft_complex(r, n_fft, hop);
    let df = sr / n_fft as f64;
    let mut bands = Vec::new();
    let mut score_acc = 0.0;
    let mut weight_acc = 0.0;
    for (lo, hi) in STEREO_BANDS.iter().filter(|(lo, _)| *lo < nyq) {
        let hi = hi.min(nyq);
        let k0 = ((lo / df).ceil() as usize).max(1);
        let k1 = ((hi / df).floor() as usize).min(n_fft / 2);
        let (mut sl, mut sr_, mut x, mut sm, mut ss) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for (a, b) in fl.iter().zip(&fr) {
            for k in k0..=k1.max(k0) {
                let (p, q) = (a[k], b[k]);
                sl += p.norm_sqr() as f64;
                sr_ += q.norm_sqr() as f64;
                x += (p * q.conj()).re as f64;
                sm += ((p + q) * 0.5).norm_sqr() as f64;
                ss += ((p - q) * 0.5).norm_sqr() as f64;
            }
        }
        let c = if sl > 1e-20 && sr_ > 1e-20 {
            x / (sl * sr_).sqrt()
        } else {
            1.0
        };
        let w = if sm + ss > 1e-20 { ss / (sm + ss) } else { 0.0 };
        let loss = if sl + sr_ > 1e-20 {
            pow_db(2.0 * sm / (sl + sr_)).max(-120.0)
        } else {
            0.0
        };
        let energy = (sl + sr_) / 2.0;
        // normalise band energy to mean square per sample
        let ms = energy * 2.0 / (n_fft as f64 * (3.0 * n_fft as f64 / 8.0)) / fl.len().max(1) as f64;
        score_acc += energy * (1.0 + loss / 6.0).clamp(0.0, 1.0);
        weight_acc += energy;
        bands.push(StereoBand {
            low_hz: *lo,
            high_hz: hi,
            correlation: c,
            width: w,
            mono_loss_db: loss,
            level_db: pow_db(ms),
        });
    }
    let mono_compat = if weight_acc > 0.0 {
        score_acc / weight_acc
    } else {
        1.0
    };
    StereoReport {
        channels: buf.channels(),
        correlation: corr,
        correlation_below_120hz: corr_low,
        side_to_mid_db: if em > 1e-20 && es > 1e-20 {
            10.0 * (es / em).log10()
        } else if es <= 1e-20 {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        },
        width,
        mono_loss_db: mono_loss,
        mono_compatibility: mono_compat,
        balance_db: pow_db(el / n) - pow_db(er / n),
        bands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, sine, stereo};

    #[test]
    fn mono_identical_and_out_of_phase() {
        let s = stereo(sine(48000, 440.0, 1.0, 0.5));
        let r = stereo_analysis(&s);
        assert!((r.correlation - 1.0).abs() < 1e-6);
        assert!(r.width < 1e-6);
        assert!(r.mono_compatibility > 0.99);
        let mut o = s.clone();
        o.data[1].iter_mut().for_each(|v| *v = -*v);
        let r = stereo_analysis(&o);
        assert!(r.correlation < -0.99);
        assert!(r.width > 0.99);
        assert!(r.mono_compatibility < 0.05);
        let m = stereo_analysis(&sine(48000, 440.0, 1.0, 0.5));
        assert_eq!(m.correlation, 1.0);
    }

    #[test]
    fn uncorrelated_and_low_band() {
        let a = noise(48000, 2.0, 0.3, 1);
        let b = noise(48000, 2.0, 0.3, 2);
        let buf = AudioBuffer::from_planar(48000, vec![a.data[0].clone(), b.data[0].clone()]);
        let r = stereo_analysis(&buf);
        assert!(r.correlation.abs() < 0.05);
        assert!((r.width - 0.5).abs() < 0.05);
        assert!((r.mono_loss_db + 3.0).abs() < 0.3);
        assert!((r.mono_compatibility - 0.5).abs() < 0.1);
        // out-of-phase bass under in-phase highs
        let bass = sine(48000, 60.0, 2.0, 0.4).data[0].clone();
        let hi = sine(48000, 2000.0, 2.0, 0.4).data[0].clone();
        let l: Vec<f32> = bass.iter().zip(&hi).map(|(b, h)| b + h).collect();
        let rr: Vec<f32> = bass.iter().zip(&hi).map(|(b, h)| -b + h).collect();
        let buf = AudioBuffer::from_planar(48000, vec![l, rr]);
        let r = stereo_analysis(&buf);
        assert!(r.correlation_below_120hz < -0.9, "{}", r.correlation_below_120hz);
        assert!(r.bands[0].correlation < -0.9);
        assert!(r.bands[5].correlation > 0.9);
    }
}
