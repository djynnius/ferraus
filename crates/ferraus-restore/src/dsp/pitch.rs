//! Time-varying pitch change: tape-style varispeed and a WSOLA-aligned granular
//! shifter that preserves duration.
//!
//! The shifter overlap-adds Hann grains (about 46 ms, 50% overlap). Each grain is the
//! input around its own output time, resampled by the current pitch ratio; its source
//! position is nudged within +-12 ms to the offset whose waveform best continues the
//! previous grain (waveform-similarity overlap-add), which avoids phase cancellation.
//! Quality is good for speech, monophonic lines, and moderate shifts; dense polyphonic
//! material and large shifts show the usual granular roughness.

use super::biquad::butterworth;
use super::util::read_cubic;

/// Low-pass the input before reading it faster than real time (ratio > 1).
fn antialias(x: &[f32], sr: f64, max_ratio: f64) -> Vec<f32> {
    if max_ratio <= 1.0001 {
        return x.to_vec();
    }
    let cutoff = (0.45 * sr / max_ratio).min(0.45 * sr);
    let lp = butterworth(8, cutoff, sr, false);
    lp.apply(x)
}

/// Tape-style varispeed: the read position advances by `ratio(input_position)` per output
/// sample, so pitch and duration change together. Returns the new signal.
pub fn varispeed(x: &[f32], sr: f64, ratio: impl Fn(f64) -> f64, max_ratio: f64) -> Vec<f32> {
    let src = antialias(x, sr, max_ratio);
    let n = x.len() as f64;
    let mut out = Vec::with_capacity(x.len());
    let mut pos = 0.0f64;
    while pos < n {
        out.push(read_cubic(&src, pos) as f32);
        let r = ratio(pos).clamp(1.0 / 16.0, 16.0);
        pos += r;
        if out.len() > x.len() * 17 {
            break;
        }
    }
    out
}

/// Duration-preserving pitch shift with a time-varying ratio (`ratio(output_position)`).
pub fn shift_preserving(x: &[f32], sr: f64, ratio: impl Fn(f64) -> f64, max_ratio: f64) -> Vec<f32> {
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    let src = antialias(x, sr, max_ratio);
    let grain = ((0.046 * sr) as usize).max(64) & !1;
    let hop = grain / 2;
    let search = (0.012 * sr) as isize;
    let win: Vec<f64> = (0..grain)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / grain as f64).cos())
        .collect();
    let mut out = vec![0.0f64; n + grain];
    let half = (grain / 2) as f64;
    let mut prev: Option<(f64, f64)> = None; // (anchor, ratio)
    let frames = n / hop + 2;
    let corr_len = hop as isize;
    let step = 2isize;
    for k in 0..frames {
        let c = (k * hop) as f64; // output centre
        let r = ratio(c).clamp(0.25, 4.0);
        let mut anchor = c;
        if let Some((pa, pr)) = prev {
            // continuation point of the previous grain, in input samples
            let cont = pa + hop as f64 * pr;
            let mut best = (0isize, f64::MIN);
            for d in (-search..=search).step_by(2) {
                let cand = c + d as f64;
                let mut s = 0.0;
                let mut e1 = 0.0;
                let mut e2 = 0.0;
                let mut m = -corr_len / 2;
                while m < corr_len / 2 {
                    let a = sample(&src, cont + m as f64 * pr);
                    let b = sample(&src, cand + m as f64 * r);
                    s += a * b;
                    e1 += a * a;
                    e2 += b * b;
                    m += step;
                }
                let score = if e1 > 1e-20 && e2 > 1e-20 { s / (e1 * e2).sqrt() } else { 0.0 };
                // prefer staying close to the nominal position when scores tie
                let score = score - 1e-6 * (d.abs() as f64);
                if score > best.1 {
                    best = (d, score);
                }
            }
            anchor = c + best.0 as f64;
        }
        let start = c - half;
        for (i, w) in win.iter().enumerate() {
            let t = start + i as f64;
            if t < 0.0 {
                continue;
            }
            let ti = t as usize;
            if ti >= out.len() {
                break;
            }
            let p = anchor + (i as f64 - half) * r;
            out[ti] += w * read_cubic(&src, p);
        }
        prev = Some((anchor, r));
    }
    out.truncate(n);
    out.into_iter().map(|v| v as f32).collect()
}

#[inline]
fn sample(x: &[f32], pos: f64) -> f64 {
    let i = pos.round() as isize;
    if i >= 0 && (i as usize) < x.len() {
        x[i as usize] as f64
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing;

    #[test]
    fn varispeed_octave_halves_length() {
        let x = testing::sine(48000, 440.0, 1.0, 0.5).data[0].clone();
        let y = varispeed(&x, 48000.0, |_| 2.0, 2.0);
        assert_eq!(y.len(), 24000);
        let f = crate::testutil::dominant_freq(&y[2000..22000], 48000, 800.0, 960.0);
        assert!((f - 880.0).abs() < 1.0, "{f}");
    }

    #[test]
    fn shifter_keeps_length_and_moves_pitch() {
        let x = testing::sine(48000, 300.0, 1.0, 0.5).data[0].clone();
        for (r, expect) in [(1.5, 450.0), (0.75, 225.0)] {
            let y = shift_preserving(&x, 48000.0, |_| r, r);
            assert_eq!(y.len(), x.len());
            let f = crate::testutil::dominant_freq(&y[6000..42000], 48000, 150.0, 600.0);
            assert!((f - expect).abs() < 2.0, "{r}: {f}");
            let lvl = testing::rms_db(&y[6000..42000]) - testing::rms_db(&x[6000..42000]);
            assert!(lvl.abs() < 1.5, "level change {lvl}");
        }
    }
}
