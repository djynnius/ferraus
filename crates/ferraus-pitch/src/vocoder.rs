//! Phase vocoder: time stretching and (time-varying) pitch shifting with identity phase
//! locking (Laroche & Dolson 1999) and optional cepstral formant-envelope correction.
//!
//! Every synthesis frame `j` sits at output sample `j × hop`; its analysis position comes
//! from a [`TimeMap`], so uniform stretches, transient-anchored stretches and pure pitch
//! shifts (identity map) share one engine. The instantaneous frequency of each bin is
//! measured from a second analysis frame a fixed `n/8` samples earlier, which keeps the
//! estimate valid for any analysis hop. Spectral peaks are shifted to `ratio ×` their
//! frequency together with their region of influence (identity phase locking keeps the
//! phase relations inside each region), and their synthesis phase is accumulated at the
//! shifted frequency. With formant correction each moved bin is multiplied by
//! `E(k') / E(k)`, where `E` is the cepstrally smoothed spectral envelope of the frame, so
//! harmonics move while the vocal-tract resonances stay put.

use crate::timemap::TimeMap;
use crate::util::{hann, princarg, Fft};
use realfft::num_complex::Complex;
use std::f64::consts::PI;

/// Phase-vocoder settings.
#[derive(Clone, Debug)]
pub struct PvParams {
    /// FFT / window size (power of two).
    pub n_fft: usize,
    /// Synthesis hop (samples).
    pub hop: usize,
    /// Apply cepstral formant correction when shifting pitch.
    pub formant_preserve: bool,
    /// Cepstral lifter cutoff (quefrency in samples).
    pub lifter: usize,
    /// Identity phase locking (off = classic bin-by-bin vocoder).
    pub phase_lock: bool,
}

impl PvParams {
    /// High quality: ~90 ms window, 8× overlap.
    pub fn high(sr: u32) -> PvParams {
        let n = ((0.09 * sr as f64) as usize).next_power_of_two().max(256);
        PvParams { n_fft: n, hop: n / 8, formant_preserve: true, lifter: (sr as usize / 700).max(8), phase_lock: true }
    }
    /// Fast: ~45 ms window, 4× overlap.
    pub fn fast(sr: u32) -> PvParams {
        let n = ((0.045 * sr as f64) as usize).next_power_of_two().max(256);
        PvParams { n_fft: n, hop: n / 4, ..PvParams::high(sr) }
    }
    /// Short windows (~23 ms) for speech.
    pub fn speech(sr: u32) -> PvParams {
        let n = ((0.023 * sr as f64) as usize).next_power_of_two().max(128);
        PvParams { n_fft: n, hop: n / 4, ..PvParams::high(sr) }
    }
}

/// Render `x` through the vocoder. `ratio(input_pos)` gives the pitch ratio at an input
/// sample position; `reset(output_pos)` forces a phase reset (output phase = input phase)
/// for frames centred there.
pub fn render(
    x: &[f32],
    map: &TimeMap,
    ratio: &dyn Fn(f64) -> f64,
    reset: &dyn Fn(f64) -> bool,
    p: &PvParams,
) -> Vec<f32> {
    let n = p.n_fft;
    let half = n / 2;
    let nb = half + 1;
    let hs = p.hop;
    let href = (n / 8).max(1);
    let out_len = map.out_len;
    let win = hann(n);
    let mut fa = Fft::new(n);
    let mut fb = Fft::new(n);
    let mut fenv = if p.formant_preserve { Some(Fft::new(n)) } else { None };
    let mut acc = vec![0.0f32; out_len + n];
    let mut wsum = vec![0.0f32; out_len + n];
    let mut mag = vec![0.0f64; nb];
    let mut pha = vec![0.0f64; nb];
    let mut omega = vec![0.0f64; nb];
    let mut theta_prev = vec![0.0f64; nb];
    let mut theta_cur = vec![0.0f64; nb];
    let mut logenv = vec![0.0f64; nb];
    let mut y = vec![Complex::new(0.0f64, 0.0); nb];
    let mut peaks: Vec<usize> = Vec::new();
    let mut first = true;
    let get = |i: isize| -> f32 {
        if i >= 0 && (i as usize) < x.len() {
            x[i as usize]
        } else {
            0.0
        }
    };
    let mut j = 0usize;
    loop {
        let oc = j * hs; // output centre
        if oc >= out_len + half {
            break;
        }
        let ic = map.in_at(oc as f64);
        let c = ic.round() as isize;
        for i in 0..n {
            let off = i as isize - half as isize;
            fa.time[i] = get(c + off) * win[i];
            fb.time[i] = get(c - href as isize + off) * win[i];
        }
        fa.forward();
        fb.forward();
        let mut mx = 0.0f64;
        for k in 0..nb {
            let a = fa.freq[k];
            let b = fb.freq[k];
            let m = (a.re as f64).hypot(a.im as f64);
            mag[k] = m;
            mx = mx.max(m);
            let pa = (a.im as f64).atan2(a.re as f64);
            let pb = (b.im as f64).atan2(b.re as f64);
            pha[k] = pa;
            let bin_w = 2.0 * PI * k as f64 / n as f64;
            omega[k] = bin_w + princarg(pa - pb - bin_w * href as f64) / href as f64;
        }
        let r = ratio(ic).clamp(0.25, 4.0);
        let do_reset = first || reset(oc as f64);
        let shifting = (r - 1.0).abs() > 1e-9;
        if shifting {
            if let Some(fe) = fenv.as_mut() {
                envelope(fe, &mag, p.lifter, &mut logenv);
            }
        }
        // peaks and regions of influence
        peaks.clear();
        let floor = mx * 1e-6;
        if p.phase_lock {
            for k in 1..nb - 1 {
                if mag[k] > floor && mag[k] > mag[k - 1] && mag[k] >= mag[k + 1] {
                    peaks.push(k);
                }
            }
        } else {
            peaks.extend((0..nb).filter(|&k| mag[k] > floor));
        }
        y.iter_mut().for_each(|v| *v = Complex::new(0.0, 0.0));
        let mut lo = 0usize;
        for (pi, &pk) in peaks.iter().enumerate() {
            let hi = if !p.phase_lock {
                pk + 1
            } else if pi + 1 < peaks.len() {
                // boundary at the lowest bin between this peak and the next
                let nx = peaks[pi + 1];
                let mut b = pk + 1;
                for k in pk + 1..nx {
                    if mag[k] < mag[b] {
                        b = k;
                    }
                }
                b
            } else {
                nb
            };
            let start = if p.phase_lock { lo } else { pk };
            let s = if shifting { (pk as f64 * (r - 1.0)).round() as isize } else { 0 };
            let tgt = pk as isize + s;
            let theta = if do_reset {
                pha[pk]
            } else if tgt >= 0 && (tgt as usize) < nb {
                theta_prev[tgt as usize] + omega[pk] * r * hs as f64
            } else {
                pha[pk]
            };
            for k in start..hi {
                let k2 = k as isize + s;
                if k2 < 0 || k2 as usize >= nb {
                    continue;
                }
                let k2 = k2 as usize;
                let mut m = mag[k];
                if shifting && fenv.is_some() && s != 0 {
                    m *= (logenv[k2] - logenv[k]).clamp(-4.0, 4.0).exp();
                }
                let ph = theta + (pha[k] - pha[pk]);
                y[k2] += Complex::new(m * ph.cos(), m * ph.sin());
            }
            lo = hi;
        }
        for k in 0..nb {
            let v = y[k];
            theta_cur[k] = if v.re != 0.0 || v.im != 0.0 {
                v.im.atan2(v.re)
            } else {
                theta_prev[k] + 2.0 * PI * k as f64 * hs as f64 / n as f64
            };
            fa.freq[k] = Complex::new(v.re as f32, v.im as f32);
        }
        std::mem::swap(&mut theta_prev, &mut theta_cur);
        fa.inverse();
        let scale = 1.0 / n as f32;
        // OLA: output index of frame sample i is oc - half + i; acc is offset by half
        for i in 0..n {
            let o = oc + i; // = (oc - half + i) + half
            if o < half || o - half >= out_len {
                continue;
            }
            acc[o - half] += fa.time[i] * scale * win[i];
            wsum[o - half] += win[i] * win[i];
        }
        first = false;
        j += 1;
    }
    let steady = 3.0 * n as f32 / (8.0 * hs as f32);
    let fl = 0.1 * steady;
    acc.truncate(out_len);
    for (a, w) in acc.iter_mut().zip(&wsum) {
        *a /= w.max(fl);
    }
    acc
}

/// Cepstrally smoothed log-magnitude envelope of `mag` (natural log), into `out`.
fn envelope(f: &mut Fft, mag: &[f64], lifter: usize, out: &mut [f64]) {
    let n = f.n;
    for (k, m) in mag.iter().enumerate() {
        f.freq[k] = Complex::new((m + 1e-9).ln() as f32, 0.0);
    }
    f.inverse(); // real cepstrum × n
    let lc = lifter.min(n / 2 - 1);
    for i in 0..n {
        let keep = i < lc || i > n - lc;
        if !keep {
            f.time[i] = 0.0;
        } else if i == lc - 1 || i == n - lc + 1 {
            f.time[i] *= 0.5;
        }
    }
    f.forward();
    let scale = 1.0 / n as f64;
    for (o, c) in out.iter_mut().zip(&f.freq) {
        *o = c.re as f64 * scale;
    }
}

/// Pitch-shift by a constant ratio, keeping duration.
pub fn pitch_shift(x: &[f32], ratio: f64, p: &PvParams) -> Vec<f32> {
    let map = TimeMap::uniform(x.len(), 1.0);
    render(x, &map, &|_| ratio, &|_| false, p)
}

/// Pitch-shift with a per-frame ratio curve (`ratio[k]` applies at sample `k × hop`).
pub fn pitch_shift_curve(x: &[f32], ratio: &[f64], hop: usize, p: &PvParams) -> Vec<f32> {
    let map = TimeMap::uniform(x.len(), 1.0);
    let h = hop.max(1) as f64;
    render(x, &map, &|pos| crate::util::interp(ratio, pos / h), &|_| false, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use crate::pyin::{track, PyinOptions};

    fn median_pitch(x: &[f32], sr: u32) -> f64 {
        let fr = track(x, sr, &PyinOptions::default());
        let v: Vec<f64> = fr[10..fr.len() - 10].iter().filter(|f| f.voiced).map(|f| f.freq_hz).collect();
        crate::util::median(&v).unwrap_or(0.0)
    }

    #[test]
    fn identity_ratio_reconstructs() {
        let sr = 16000;
        let x = vocal(sr, &[(150.0, 0.5)], Vowel::A, 0.0, 0.0);
        let y = pitch_shift(&x, 1.0, &PvParams::high(sr));
        assert_eq!(y.len(), x.len());
        let err: f64 =
            x.iter().zip(&y).skip(2000).take(4000).map(|(a, b)| ((a - b) as f64).powi(2)).sum::<f64>() / 4000.0;
        assert!(err.sqrt() < 0.01, "rms err {}", err.sqrt());
    }

    #[test]
    fn shifts_pitch_by_ratio() {
        let sr = 16000;
        let x = vocal(sr, &[(150.0, 0.8)], Vowel::A, 0.0, 0.0);
        for st in [4.0, -3.0, 0.5] {
            let r = 2f64.powf(st / 12.0);
            let y = pitch_shift(&x, r, &PvParams::high(sr));
            let f = median_pitch(&y, sr);
            assert!(crate::util::cents(150.0 * r, f).abs() < 5.0, "{st} st: got {f}, want {}", 150.0 * r);
        }
    }

    #[test]
    fn stretch_keeps_pitch_and_scales_length() {
        let sr = 16000;
        let x = vocal(sr, &[(200.0, 0.5)], Vowel::A, 0.0, 0.0);
        let map = TimeMap::uniform(x.len(), 1.5);
        let y = render(&x, &map, &|_| 1.0, &|_| false, &PvParams::high(sr));
        assert_eq!(y.len(), (x.len() as f64 * 1.5).round() as usize);
        assert!(crate::util::cents(200.0, median_pitch(&y, sr)).abs() < 5.0);
    }
}
