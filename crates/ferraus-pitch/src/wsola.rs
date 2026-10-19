//! WSOLA (waveform-similarity overlap-add, Verhelst & Roelands 1993) time stretching.
//!
//! Output frames are placed every `n/2` samples with a Hann window. Each frame's input
//! segment is taken near the position the [`TimeMap`] asks for, shifted by up to
//! `tolerance` samples to best match (normalised cross-correlation, computed with an FFT)
//! the natural continuation of the previous segment. Inside protected (slope-1) windows
//! the search is disabled, so transients are copied verbatim, exactly once, at their
//! mapped position.

use crate::timemap::TimeMap;
use crate::util::{hann, Fft};

/// WSOLA settings.
#[derive(Clone, Debug)]
pub struct WsolaParams {
    /// Frame length (samples, even).
    pub n: usize,
    /// Maximum shift searched around the nominal position (samples).
    pub tolerance: usize,
}

impl WsolaParams {
    /// ~23 ms frames, ±6 ms search (music).
    pub fn music(sr: u32) -> WsolaParams {
        WsolaParams {
            n: ((0.023 * sr as f64) as usize / 2 * 2).max(64),
            tolerance: (0.006 * sr as f64) as usize,
        }
    }
    /// ~15 ms frames, ±4 ms search (speech).
    pub fn speech(sr: u32) -> WsolaParams {
        WsolaParams {
            n: ((0.015 * sr as f64) as usize / 2 * 2).max(64),
            tolerance: (0.004 * sr as f64) as usize,
        }
    }
    /// Half-width of the slope-1 window needed around a transient.
    pub fn protect_radius(&self) -> usize {
        self.n / 2 + self.tolerance + 16
    }
}

/// Stretch `x` along `map`.
pub fn render(x: &[f32], map: &TimeMap, p: &WsolaParams) -> Vec<f32> {
    let n = p.n.max(4) / 2 * 2;
    let hs = n / 2;
    let tol = p.tolerance;
    let out_len = map.out_len;
    let win = hann(n);
    let m = (n + 2 * tol).next_power_of_two();
    let mut fr = Fft::new(m);
    let mut reg_spec = vec![realfft::num_complex::Complex::new(0.0f32, 0.0); m / 2 + 1];
    let mut reg = vec![0.0f32; m];
    let get = |i: isize| -> f32 {
        if i >= 0 && (i as usize) < x.len() {
            x[i as usize]
        } else {
            0.0
        }
    };
    let mut acc = vec![0.0f32; out_len];
    let mut wsum = vec![0.0f32; out_len];
    let mut prev: Option<isize> = None;
    let mut j = 0usize;
    loop {
        let oc = j * hs;
        if oc >= out_len + hs {
            break;
        }
        let os = oc as isize - hs as isize; // output start of this frame
        let nominal = map.in_at(oc as f64).round() as isize - hs as isize;
        let chosen = match prev {
            Some(pv) if tol > 0 && !map.is_protected(oc as f64) => {
                let natural = pv + hs as isize;
                // reference: natural continuation
                let mut ref_e = 0.0f64;
                for i in 0..m {
                    let v = if i < n { get(natural + i as isize) } else { 0.0 };
                    fr.time[i] = v;
                    ref_e += (v as f64) * (v as f64);
                }
                if ref_e < 1e-12 {
                    nominal
                } else {
                    fr.forward();
                    let base = nominal - tol as isize;
                    let span = n + 2 * tol;
                    for (i, r) in reg.iter_mut().enumerate() {
                        *r = if i < span { get(base + i as isize) } else { 0.0 };
                    }
                    fr.forward_into(&mut reg, &mut reg_spec);
                    for (a, b) in fr.freq.iter_mut().zip(&reg_spec) {
                        *a = a.conj() * b;
                    }
                    fr.inverse();
                    // candidate energies via a running sum
                    let mut e = 0.0f64;
                    for i in 0..n {
                        let v = get(base + i as isize) as f64;
                        e += v * v;
                    }
                    let mut best = (f64::NEG_INFINITY, tol as isize);
                    for d in 0..=2 * tol {
                        if d > 0 {
                            let out_v = get(base + d as isize - 1) as f64;
                            let in_v = get(base + d as isize + n as isize - 1) as f64;
                            e += in_v * in_v - out_v * out_v;
                        }
                        let score = fr.time[d] as f64 / (e.max(1e-12)).sqrt();
                        if score > best.0 {
                            best = (score, d as isize);
                        }
                    }
                    base + best.1
                }
            }
            _ => nominal,
        };
        for i in 0..n {
            let o = os + i as isize;
            if o < 0 || o as usize >= out_len {
                continue;
            }
            acc[o as usize] += win[i] * get(chosen + i as isize);
            wsum[o as usize] += win[i];
        }
        prev = Some(chosen);
        j += 1;
    }
    for (a, w) in acc.iter_mut().zip(&wsum) {
        *a /= w.max(1e-3);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_map_reconstructs_exactly() {
        let x: Vec<f32> = ferraus_core::testing::noise(8000, 0.3, 0.5, 3).data.remove(0);
        let map = TimeMap::uniform(x.len(), 1.0);
        let y = render(&x, &map, &WsolaParams::music(8000));
        assert_eq!(y.len(), x.len());
        for (a, b) in x.iter().zip(&y) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn stretch_keeps_pitch() {
        let sr = 16000;
        let x = crate::fixtures::sine_wave(sr, 220.0, 0.5, 0.5);
        let map = TimeMap::uniform(x.len(), 1.7);
        let y = render(&x, &map, &WsolaParams::music(sr));
        assert_eq!(y.len(), (x.len() as f64 * 1.7).round() as usize);
        let fr = crate::pyin::track(&y, sr, &crate::pyin::PyinOptions::default());
        let v: Vec<f64> = fr[5..fr.len() - 5].iter().map(|f| f.freq_hz).collect();
        let m = crate::util::median(&v).unwrap();
        assert!(crate::util::cents(220.0, m).abs() < 5.0, "{m}");
    }
}
