//! ITU-R BS.1770-4 integrated loudness (K-weighting, 400 ms blocks, absolute and
//! relative gating). Used by `enhance` to set its loudness target without a run-time
//! dependency.

use super::biquad::{Coefs, Sos};
use std::f64::consts::PI;

fn k_weighting(sr: f64) -> Sos {
    // stage 1: high shelf
    let (g, f0, q) = (3.999_843_853_973_347, 1_681.974_450_955_533, 0.707_175_236_955_419_6);
    let k = (PI * f0 / sr).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Coefs {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    };
    // stage 2: RLB high-pass
    let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (PI * f0 / sr).tan();
    let d = 1.0 + k / q + k * k;
    let hp = Coefs {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k * k - 1.0) / d,
        a2: (1.0 - k / q + k * k) / d,
    };
    Sos::new(vec![shelf, hp])
}

/// Integrated loudness in LUFS (`None` for silence or empty input).
pub fn integrated(chans: &[Vec<f32>], sr: u32) -> Option<f64> {
    let n = chans.first().map(|c| c.len()).unwrap_or(0);
    if n == 0 {
        return None;
    }
    let srf = sr as f64;
    let kw = k_weighting(srf);
    let weighted: Vec<Vec<f32>> = chans.iter().map(|c| kw.apply(c)).collect();
    let weight = |c: usize| if c == 3 { 0.0 } else if c >= 4 { 1.41 } else { 1.0 };
    let block = ((0.4 * srf) as usize).min(n).max(1);
    let hop = ((0.1 * srf) as usize).max(1);
    let mut powers = Vec::new();
    let mut start = 0;
    loop {
        let end = start + block;
        if end > n {
            break;
        }
        let mut z = 0.0;
        for (c, ch) in weighted.iter().enumerate() {
            let ms: f64 = ch[start..end].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / block as f64;
            z += weight(c) * ms;
        }
        powers.push(z);
        start += hop;
    }
    let lufs = |p: f64| -0.691 + 10.0 * p.max(1e-30).log10();
    let abs: Vec<f64> = powers.iter().copied().filter(|p| lufs(*p) > -70.0).collect();
    if abs.is_empty() {
        return None;
    }
    let rel_gate = lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
    let gated: Vec<f64> = abs.into_iter().filter(|p| lufs(*p) > rel_gate).collect();
    if gated.is_empty() {
        return None;
    }
    Some(lufs(gated.iter().sum::<f64>() / gated.len() as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing;

    #[test]
    fn sine_reference_levels() {
        // a full-scale 1 kHz sine in one channel reads -3.01 LUFS; in two channels 0 LUFS
        let s = testing::sine(48000, 1000.0, 2.0, 1.0);
        let mono = integrated(&s.data, 48000).unwrap();
        assert!((mono + 3.01).abs() < 0.1, "{mono}");
        let st = testing::stereo(s);
        let l = integrated(&st.data, 48000).unwrap();
        assert!(l.abs() < 0.1, "{l}");
        let q = testing::sine(44100, 1000.0, 1.0, 0.1);
        assert!((integrated(&q.data, 44100).unwrap() + 23.01).abs() < 0.15);
        assert!(integrated(&[vec![0.0; 48000]], 48000).is_none());
    }
}
