//! Loudness and peak measurement (EBU R128 / ITU-R BS.1770 via the `ebur128` crate), a 4x
//! oversampled true-peak meter, and a transparent lookahead true-peak limiter.
//!
//! These are public so other crates (mastering, analysis) can share one implementation:
//!
//! ```
//! use ferraus_core::testing::{sine, stereo};
//! use ferraus_edit::loudness;
//! // EBU Tech 3341 case 1: 1 kHz stereo sine at -23 dBFS peak reads -23.0 LUFS
//! let amp = 10f32.powf(-23.0 / 20.0);
//! let b = stereo(sine(48000, 1000.0, 5.0, amp));
//! let l = loudness::integrated_lufs(&b).unwrap();
//! assert!((l + 23.0).abs() < 0.1);
//! ```

use ebur128::{EbuR128, Mode};
use ferraus_core::prelude::*;
use serde::Serialize;

/// Measured loudness and peak values.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LoudnessStats {
    /// Gated integrated loudness in LUFS (`-inf` reported as -240 for silence).
    pub integrated_lufs: f64,
    /// Loudness range in LU.
    pub loudness_range_lu: f64,
    /// True peak in dBTP (4x oversampled; the larger of this crate's and `ebur128`'s meters).
    pub true_peak_dbtp: f64,
    /// Sample peak in dBFS.
    pub sample_peak_db: f64,
}

fn ebu_err(e: ebur128::Error) -> Error {
    Error::processing(format!("loudness measurement failed: {e:?}"))
}

fn meter(buf: &AudioBuffer, mode: Mode) -> Result<EbuR128> {
    let ch = buf.channels().max(1) as u32;
    let mut m = EbuR128::new(ch, buf.sample_rate.max(1), mode).map_err(ebu_err)?;
    if buf.frames() > 0 {
        let planes: Vec<&[f32]> = buf.data.iter().map(|c| c.as_slice()).collect();
        // feed in chunks so very long inputs do not need one giant call
        let n = buf.frames();
        let chunk = 1 << 16;
        let mut s = 0;
        while s < n {
            let e = (s + chunk).min(n);
            let part: Vec<&[f32]> = planes.iter().map(|p| &p[s..e]).collect();
            m.add_frames_planar_f32(&part).map_err(ebu_err)?;
            s = e;
        }
    }
    Ok(m)
}

/// Repeat a short buffer until it is at least `secs` long (for loudness of clips shorter
/// than the 400 ms gating block).
fn tile(buf: &AudioBuffer, secs: f64) -> AudioBuffer {
    let want = (secs * buf.sample_rate as f64).ceil() as usize;
    let mut out = buf.clone();
    while out.frames() < want && buf.frames() > 0 {
        out.append(buf);
    }
    out
}

/// Gated integrated loudness in LUFS. Returns `f64::NEG_INFINITY` for silence. Clips shorter
/// than 3 s are measured by looping them, so short sounds still get a meaningful value.
pub fn integrated_lufs(buf: &AudioBuffer) -> Result<f64> {
    if buf.is_empty() || buf.peak() == 0.0 {
        return Ok(f64::NEG_INFINITY);
    }
    let src;
    let b = if buf.duration_s() < 3.0 {
        src = tile(buf, 3.0);
        &src
    } else {
        buf
    };
    meter(b, Mode::I)?.loudness_global().map_err(ebu_err)
}

/// Full loudness report: integrated loudness, loudness range, true peak, sample peak.
pub fn analyze(buf: &AudioBuffer) -> Result<LoudnessStats> {
    let integrated = integrated_lufs(buf)?;
    let lra = if buf.duration_s() >= 3.0 {
        meter(buf, Mode::LRA)?.loudness_range().map_err(ebu_err)?
    } else {
        0.0
    };
    Ok(LoudnessStats {
        integrated_lufs: crate::util::round_db(integrated),
        loudness_range_lu: crate::util::round_db(lra),
        true_peak_dbtp: crate::util::round_db(lin_to_db(true_peak_conservative(buf)? as f64)),
        sample_peak_db: crate::util::round_db(lin_to_db(buf.peak() as f64)),
    })
}

/// True peak as measured by the `ebur128` crate's own interpolator (linear).
pub fn true_peak_ebur128(buf: &AudioBuffer) -> Result<f32> {
    if buf.is_empty() {
        return Ok(0.0);
    }
    let m = meter(buf, Mode::TRUE_PEAK)?;
    let mut p = 0.0f64;
    for c in 0..buf.channels() {
        p = p.max(m.true_peak(c as u32).map_err(ebu_err)?);
    }
    Ok(p as f32)
}

/// The larger of this crate's 4x true-peak meter and `ebur128`'s (linear). Used wherever a
/// ceiling must never be exceeded, so the result holds whichever meter checks it.
pub fn true_peak_conservative(buf: &AudioBuffer) -> Result<f32> {
    Ok(true_peak(buf).max(true_peak_ebur128(buf)?))
}

// ---------------------------------------------------------------------------------------
// 4x oversampled true-peak meter
// ---------------------------------------------------------------------------------------

/// Taps per phase of the interpolator.
const TP_TAPS: usize = 16;
/// Offset of the first tap relative to the current sample.
const TP_FIRST: isize = -7;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// Kaiser-windowed sinc kernels for fractional positions 1/4, 2/4, 3/4.
fn tp_kernels() -> [[f32; TP_TAPS]; 3] {
    let beta = 7.0;
    let half = 8.5;
    let i0b = bessel_i0(beta);
    let mut k = [[0f32; TP_TAPS]; 3];
    for (p, row) in k.iter_mut().enumerate() {
        let frac = (p + 1) as f64 / 4.0;
        let mut taps = [0f64; TP_TAPS];
        for (j, t) in taps.iter_mut().enumerate() {
            let x = frac - (j as isize + TP_FIRST) as f64;
            let px = std::f64::consts::PI * x;
            let sinc = if x.abs() < 1e-12 { 1.0 } else { px.sin() / px };
            let r = x / half;
            let w = if r.abs() >= 1.0 {
                0.0
            } else {
                bessel_i0(beta * (1.0 - r * r).sqrt()) / i0b
            };
            *t = sinc * w;
        }
        let sum: f64 = taps.iter().sum();
        for (o, t) in row.iter_mut().zip(taps) {
            *o = (t / sum) as f32;
        }
    }
    k
}

/// Per-frame true-peak envelope of one channel: `env[n]` is the largest magnitude of the
/// band-limited signal on `[n, n+1)` (the sample itself and three interpolated points).
fn tp_envelope_channel(x: &[f32], kernels: &[[f32; TP_TAPS]; 3], env: &mut [f32]) {
    let n = x.len();
    let get = |i: isize| -> f32 {
        if i < 0 || i as usize >= n {
            0.0
        } else {
            x[i as usize]
        }
    };
    let interior_lo = (-TP_FIRST) as usize;
    let interior_hi = n.saturating_sub(TP_TAPS);
    for i in 0..n {
        let mut m = x[i].abs();
        if i >= interior_lo && i < interior_hi {
            let base = (i as isize + TP_FIRST) as usize;
            let w = &x[base..base + TP_TAPS];
            for k in kernels {
                let mut acc = 0f32;
                for j in 0..TP_TAPS {
                    acc += w[j] * k[j];
                }
                m = m.max(acc.abs());
            }
        } else {
            for k in kernels {
                let mut acc = 0f32;
                for (j, kj) in k.iter().enumerate() {
                    acc += get(i as isize + TP_FIRST + j as isize) * kj;
                }
                m = m.max(acc.abs());
            }
        }
        if m > env[i] {
            env[i] = m;
        }
    }
}

/// Per-frame true-peak envelope across all channels.
pub fn true_peak_envelope(buf: &AudioBuffer) -> Vec<f32> {
    let kernels = tp_kernels();
    let mut env = vec![0f32; buf.frames()];
    for c in &buf.data {
        tp_envelope_channel(c, &kernels, &mut env);
    }
    env
}

/// True peak (linear) with 4x oversampling.
pub fn true_peak(buf: &AudioBuffer) -> f32 {
    true_peak_envelope(buf).into_iter().fold(0.0, f32::max)
}

/// True peak in dBTP.
pub fn true_peak_dbtp(buf: &AudioBuffer) -> f64 {
    lin_to_db(true_peak(buf) as f64)
}

/// Sample peak in dBFS.
pub fn sample_peak_db(buf: &AudioBuffer) -> f64 {
    lin_to_db(buf.peak() as f64)
}

/// RMS over all channels in dBFS.
pub fn rms_db(buf: &AudioBuffer) -> f64 {
    lin_to_db(buf.rms() as f64)
}

// ---------------------------------------------------------------------------------------
// Lookahead true-peak limiter
// ---------------------------------------------------------------------------------------

/// `out[n] = min(x[n - before ..= n + after])` (indices clamped), in O(n).
fn window_min(x: &[f32], before: usize, after: usize) -> Vec<f32> {
    let n = x.len();
    let mut out = vec![0f32; n];
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut next = 0usize; // next index to push
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (i + after).min(n.saturating_sub(1));
        while next <= hi && next < n {
            while let Some(&b) = dq.back() {
                if x[b] >= x[next] {
                    dq.pop_back();
                } else {
                    break;
                }
            }
            dq.push_back(next);
            next += 1;
        }
        let lo = i.saturating_sub(before);
        while let Some(&f) = dq.front() {
            if f < lo {
                dq.pop_front();
            } else {
                break;
            }
        }
        *o = dq.front().map(|&f| x[f]).unwrap_or(1.0);
    }
    out
}

/// Limiter settings.
#[derive(Clone, Copy, Debug)]
pub struct LimiterOptions {
    /// Ceiling in dBTP.
    pub ceiling_db: f64,
    /// Lookahead (attack) time in seconds.
    pub lookahead_s: f64,
    /// Release time constant in seconds.
    pub release_s: f64,
}

impl Default for LimiterOptions {
    fn default() -> Self {
        LimiterOptions {
            ceiling_db: -1.0,
            lookahead_s: 0.003,
            release_s: 0.08,
        }
    }
}

/// What the limiter did.
#[derive(Clone, Debug, Serialize, Default)]
pub struct LimiterReport {
    /// Largest gain reduction applied, in dB (positive).
    pub max_gain_reduction_db: f64,
    /// Limiter passes needed to bring every inter-sample peak under the ceiling.
    pub passes: usize,
    /// True peak after limiting, in dBTP.
    pub true_peak_dbtp: f64,
}

/// One limiter pass with a smooth gain curve: the required gain at every true peak is
/// reached before the peak (lookahead), held across the interpolator's span, and released
/// exponentially. Returns the smallest gain applied.
fn limiter_pass(buf: &mut AudioBuffer, target: f32, lookahead: usize, release: usize) -> f32 {
    let n = buf.frames();
    if n == 0 {
        return 1.0;
    }
    let env = true_peak_envelope(buf);
    let greq: Vec<f32> = env
        .iter()
        .map(|&e| if e > target { target / e } else { 1.0 })
        .collect();
    if greq.iter().all(|&g| g >= 1.0) {
        return 1.0;
    }
    // every sample feeding an over-ceiling interpolated point gets that point's gain
    let spread = window_min(&greq, TP_TAPS / 2 + 1, TP_TAPS / 2 + 1);
    let w = lookahead.max(1);
    // h[n] = min over [n, n + w - 1]
    let h = window_min(&spread, 0, w - 1);
    // instant attack, exponential release
    let a = if release == 0 {
        1.0
    } else {
        1.0 - (-1.0 / release as f64).exp() as f32
    };
    let mut r = vec![0f32; n];
    let mut prev = 1.0f32;
    for i in 0..n {
        let rel = prev + (1.0 - prev) * a;
        let v = h[i].min(rel);
        r[i] = v;
        prev = v;
    }
    // box average over [n - w + 1, n]: g[p] <= h[k] <= spread[p] for every k in the box
    let mut g = vec![0f32; n];
    let mut acc = 0f64;
    for i in 0..n {
        acc += r[i] as f64;
        if i >= w {
            acc -= r[i - w] as f64;
        }
        // frames before the start count as r[0], which is <= every requirement in [0, w)
        let missing = w.saturating_sub(i + 1);
        g[i] = ((acc + missing as f64 * r[0] as f64) / w as f64) as f32;
    }
    let mut min_g = 1.0f32;
    for c in &mut buf.data {
        for (s, gi) in c.iter_mut().zip(&g) {
            *s *= *gi;
        }
    }
    for gi in &g {
        min_g = min_g.min(*gi);
    }
    min_g
}

/// Limit `buf` so its true peak never exceeds `opts.ceiling_db`. Runs further passes when
/// the gain curve's own modulation leaves a residual overshoot, and finally a tiny static
/// trim as a guarantee. Checked with both true-peak meters.
pub fn limit_true_peak(buf: &mut AudioBuffer, opts: &LimiterOptions) -> Result<LimiterReport> {
    let ceiling = db_to_lin(opts.ceiling_db) as f32;
    let sr = buf.sample_rate as f64;
    let lookahead = (opts.lookahead_s * sr).round().max(1.0) as usize;
    let release = (opts.release_s * sr).round().max(0.0) as usize;
    let mut total_min_gain = 1.0f64;
    let mut passes = 0;
    // aim a hair under the ceiling so the meters' interpolation differences never matter
    let target = ceiling * db_to_lin(-0.02) as f32;
    let mut tp = true_peak_conservative(buf)?;
    while tp > ceiling && passes < 6 {
        let g = limiter_pass(buf, target, lookahead, release);
        total_min_gain *= g as f64;
        passes += 1;
        tp = true_peak_conservative(buf)?;
    }
    if tp > ceiling {
        let g = target / tp;
        buf.scale(g);
        total_min_gain *= g as f64;
        tp = true_peak_conservative(buf)?;
    }
    Ok(LimiterReport {
        max_gain_reduction_db: crate::util::round_db(-lin_to_db(total_min_gain)),
        passes,
        true_peak_dbtp: crate::util::round_db(lin_to_db(tp as f64)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, sine, stereo};

    fn amp(db: f64) -> f32 {
        db_to_lin(db) as f32
    }

    #[test]
    fn tech3341_case1_and_2() {
        let b = stereo(sine(48000, 1000.0, 20.0, amp(-23.0)));
        let l = integrated_lufs(&b).unwrap();
        assert!((l + 23.0).abs() <= 0.1, "case 1: {l}");
        let b = stereo(sine(48000, 1000.0, 20.0, amp(-33.0)));
        let l = integrated_lufs(&b).unwrap();
        assert!((l + 33.0).abs() <= 0.1, "case 2: {l}");
    }

    fn segments(parts: &[(f64, f64)]) -> AudioBuffer {
        let mut out = AudioBuffer::new(48000, 2, 0);
        for &(db, secs) in parts {
            out.append(&stereo(sine(48000, 1000.0, secs, amp(db))));
        }
        out
    }

    #[test]
    fn tech3341_case3_4_5_gating() {
        // case 3: -36 / -23 / -36 dBFS for 10 / 60 / 10 s → -23.0 LUFS
        let l = integrated_lufs(&segments(&[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)])).unwrap();
        assert!((l + 23.0).abs() <= 0.1, "case 3: {l}");
        // case 4: -72 / -36 / -23 / -36 / -72 dBFS for 10 / 10 / 60 / 10 / 10 s → -23.0
        let l = integrated_lufs(&segments(&[
            (-72.0, 10.0),
            (-36.0, 10.0),
            (-23.0, 60.0),
            (-36.0, 10.0),
            (-72.0, 10.0),
        ]))
        .unwrap();
        assert!((l + 23.0).abs() <= 0.1, "case 4: {l}");
        // case 5: -26 / -20 / -26 dBFS for 20 / 20.1 / 20 s → -23.0
        let l = integrated_lufs(&segments(&[(-26.0, 20.0), (-20.0, 20.1), (-26.0, 20.0)])).unwrap();
        assert!((l + 23.0).abs() <= 0.1, "case 5: {l}");
    }

    #[test]
    fn true_peak_of_sines() {
        // a sine sampled exactly at its peaks reads the same sample and true peak
        let b = sine(48000, 12000.0, 0.5, 0.5);
        let tp = true_peak(&b);
        assert!((tp - 0.5).abs() < 0.01, "{tp}");
        // fs/4 sine at 45 degrees phase: samples at 0.707·A, true peak A (+3 dB)
        let n = 24000;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                (0.5 * (std::f64::consts::FRAC_PI_2 * i as f64 + std::f64::consts::FRAC_PI_4).sin())
                    as f32
            })
            .collect();
        let b = AudioBuffer::from_mono(48000, x);
        assert!((b.peak() - 0.3536).abs() < 1e-3);
        let tp = true_peak(&b);
        assert!((tp - 0.5).abs() < 0.01, "inter-sample peak {tp}");
        let tpe = true_peak_ebur128(&b).unwrap();
        assert!((tpe - 0.5).abs() < 0.02, "ebur128 inter-sample peak {tpe}");
    }

    #[test]
    fn window_min_works() {
        let x = [5.0, 3.0, 4.0, 1.0, 6.0, 7.0, 2.0];
        assert_eq!(window_min(&x, 0, 1), vec![3.0, 3.0, 1.0, 1.0, 6.0, 2.0, 2.0]);
        assert_eq!(window_min(&x, 1, 1), vec![3.0, 3.0, 1.0, 1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn limiter_holds_ceiling() {
        let mut b = stereo(noise(48000, 3.0, 1.0, 7));
        b.scale(1.8);
        let r = limit_true_peak(&mut b, &LimiterOptions::default()).unwrap();
        assert!(true_peak(&b) <= amp(-1.0));
        assert!(true_peak_ebur128(&b).unwrap() <= amp(-1.0));
        assert!(r.max_gain_reduction_db > 3.0);
    }

    #[test]
    fn limiter_leaves_quiet_audio_alone() {
        let b0 = sine(48000, 440.0, 1.0, 0.25);
        let mut b = b0.clone();
        let r = limit_true_peak(&mut b, &LimiterOptions::default()).unwrap();
        assert_eq!(b, b0);
        assert_eq!(r.passes, 0);
    }
}
