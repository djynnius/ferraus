//! Shared helpers: monotone curves, keyframe parsing, fractional reads, envelope
//! followers, and small spectral utilities.

use ferraus_core::units::Time;
use ferraus_core::{Error, Result};
use std::f64::consts::PI;

/// A curve through points, evaluated with linear or monotone cubic (Fritsch-Carlson)
/// interpolation. Outside the points the end values are held.
#[derive(Clone, Debug)]
pub struct Curve {
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    slopes: Vec<f64>,
    pub spline: bool,
}

impl Curve {
    /// Points must be sorted by x; duplicates keep the last value.
    pub fn new(mut pts: Vec<(f64, f64)>, spline: bool) -> Curve {
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut xs: Vec<f64> = Vec::new();
        let mut ys: Vec<f64> = Vec::new();
        for (x, y) in pts {
            if let Some(last) = xs.last() {
                if (x - *last).abs() < 1e-12 {
                    *ys.last_mut().unwrap() = y;
                    continue;
                }
            }
            xs.push(x);
            ys.push(y);
        }
        let n = xs.len();
        let mut slopes = vec![0.0; n];
        if n >= 2 {
            let d: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i])).collect();
            slopes[0] = d[0];
            slopes[n - 1] = d[n - 2];
            for i in 1..n - 1 {
                if d[i - 1] * d[i] <= 0.0 {
                    slopes[i] = 0.0;
                } else {
                    // weighted harmonic mean (Fritsch-Butland), keeps monotonicity
                    let h0 = xs[i] - xs[i - 1];
                    let h1 = xs[i + 1] - xs[i];
                    let w1 = 2.0 * h1 + h0;
                    let w2 = h1 + 2.0 * h0;
                    slopes[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]);
                }
            }
        }
        Curve { xs, ys, slopes, spline }
    }

    pub fn is_empty(&self) -> bool {
        self.xs.is_empty()
    }

    pub fn eval(&self, x: f64) -> f64 {
        let n = self.xs.len();
        if n == 0 {
            return 0.0;
        }
        if n == 1 || x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        // binary search for segment
        let i = match self.xs.binary_search_by(|v| v.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Less)) {
            Ok(i) => return self.ys[i],
            Err(i) => i - 1,
        };
        let h = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / h;
        if !self.spline {
            return self.ys[i] + (self.ys[i + 1] - self.ys[i]) * t;
        }
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * self.ys[i] + h10 * h * self.slopes[i] + h01 * self.ys[i + 1] + h11 * h * self.slopes[i + 1]
    }

    /// Slope at the first and last point (for extrapolation).
    pub fn end_slopes(&self) -> (f64, f64) {
        let n = self.slopes.len();
        if n == 0 {
            (1.0, 1.0)
        } else {
            (self.slopes[0], self.slopes[n - 1])
        }
    }
}

/// One keyframe: a time and a value.
#[derive(Clone, Debug)]
pub struct Keyframe {
    pub time: Time,
    pub value: f64,
}

/// Parse `0s=0dB,2s=-6dB,end=-60dB` style keyframes (items already split on commas).
pub fn parse_keyframes(items: &[String], value: impl Fn(&str) -> Result<f64>) -> Result<Vec<Keyframe>> {
    let mut out = Vec::new();
    for it in items {
        let (t, v) = it
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("keyframe '{it}' should be time=value, e.g. 2s=-6dB")))?;
        out.push(Keyframe { time: Time::parse(t)?, value: value(v.trim())? });
    }
    if out.is_empty() {
        return Err(Error::usage("at least one keyframe is needed, e.g. 0s=0dB"));
    }
    Ok(out)
}

/// Resolve keyframes to (frame position, value) pairs.
pub fn resolve_keyframes(kf: &[Keyframe], sample_rate: u32, total: usize, bpm: Option<f64>) -> Result<Vec<(f64, f64)>> {
    kf.iter().map(|k| Ok((k.time.frames(sample_rate, total, bpm)? as f64, k.value))).collect()
}

/// 4-point cubic Hermite (Catmull-Rom) read at fractional position `pos`; zero outside.
#[inline]
pub fn read_cubic(x: &[f32], pos: f64) -> f64 {
    let n = x.len() as isize;
    let i = pos.floor() as isize;
    let t = pos - i as f64;
    let g = |k: isize| -> f64 {
        if k >= 0 && k < n {
            x[k as usize] as f64
        } else {
            0.0
        }
    };
    let (y0, y1, y2, y3) = (g(i - 1), g(i), g(i + 1), g(i + 2));
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// Windowed-sinc fractional read (16 taps per side, Blackman window); zero outside.
pub fn read_sinc(x: &[f32], pos: f64) -> f64 {
    const HALF: isize = 16;
    let i = pos.floor() as isize;
    let frac = pos - i as f64;
    if frac.abs() < 1e-12 {
        return if i >= 0 && (i as usize) < x.len() { x[i as usize] as f64 } else { 0.0 };
    }
    let mut acc = 0.0;
    let mut wsum = 0.0;
    for k in -HALF + 1..=HALF {
        let idx = i + k;
        let d = k as f64 - frac;
        let s = if d.abs() < 1e-12 { 1.0 } else { (PI * d).sin() / (PI * d) };
        let u = (d + HALF as f64) / (2.0 * HALF as f64);
        let w = 0.42 - 0.5 * (2.0 * PI * u).cos() + 0.08 * (4.0 * PI * u).cos();
        let c = s * w;
        wsum += c;
        if idx >= 0 && (idx as usize) < x.len() {
            acc += x[idx as usize] as f64 * c;
        }
    }
    if wsum.abs() > 1e-9 {
        acc / wsum
    } else {
        acc
    }
}

/// Shift a channel by `delay` samples (positive delays, negative advances), keeping its
/// length. Integer shifts are exact.
pub fn shift_channel(x: &[f32], delay: f64) -> Vec<f32> {
    let n = x.len();
    let r = delay.round();
    if (delay - r).abs() < 1e-9 {
        let d = r as isize;
        return (0..n as isize)
            .map(|i| {
                let j = i - d;
                if j >= 0 && (j as usize) < n {
                    x[j as usize]
                } else {
                    0.0
                }
            })
            .collect();
    }
    (0..n).map(|i| read_sinc(x, i as f64 - delay) as f32).collect()
}

/// One-pole coefficient for a time constant in seconds.
#[inline]
pub fn coef(seconds: f64, sample_rate: f64) -> f64 {
    if seconds <= 0.0 {
        0.0
    } else {
        (-1.0 / (seconds * sample_rate)).exp()
    }
}

/// Attack/release envelope follower.
#[derive(Clone, Copy, Debug)]
pub struct Follower {
    pub att: f64,
    pub rel: f64,
    pub y: f64,
}

impl Follower {
    pub fn new(attack_s: f64, release_s: f64, sr: f64) -> Follower {
        Follower { att: coef(attack_s, sr), rel: coef(release_s, sr), y: 0.0 }
    }
    #[inline]
    pub fn tick(&mut self, x: f64) -> f64 {
        let c = if x > self.y { self.att } else { self.rel };
        self.y = x + c * (self.y - x);
        self.y
    }
}

/// Power of frequency `f` in `x` (Hann-windowed Goertzel, normalised so a unit-amplitude
/// sine gives 1.0).
pub fn goertzel_amp(x: &[f64], sr: f64, f: f64) -> f64 {
    let n = x.len();
    if n < 2 {
        return 0.0;
    }
    let w = 2.0 * PI * f / sr;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    let mut wsum = 0.0;
    for (i, v) in x.iter().enumerate() {
        let win = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
        wsum += win;
        let s0 = v * win + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let p = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    2.0 * p.max(0.0).sqrt() / wsum
}

/// Median of a slice (copied).
pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m]
    } else {
        0.5 * (s[m - 1] + s[m])
    }
}

/// Percentile (0..1) of a slice (copied).
pub fn percentile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((s.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    s[idx]
}

/// Smooth 0..1 ramp.
#[inline]
pub fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Parse a channel position name: `L`, `R`, `C`, `LFE`, `Ls`, `Rs`, `M`, or a 1-based
/// number (`1`, `ch2`).
pub fn channel_index(name: &str, channels: usize) -> Option<usize> {
    let n = name.trim();
    let lower = n.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix("ch").or(Some(lower.as_str())) {
        if let Ok(i) = num.parse::<usize>() {
            return if i >= 1 { Some(i - 1) } else { None };
        }
    }
    let idx = match lower.as_str() {
        "l" | "left" => 0,
        "r" | "right" => 1,
        "m" | "mono" => 0,
        "c" | "center" | "centre" => {
            if channels <= 1 {
                0
            } else {
                2
            }
        }
        "lfe" => 3,
        "ls" => 4,
        "rs" => 5,
        _ => return None,
    };
    Some(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_monotone_and_through_points() {
        let c = Curve::new(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 1.0), (3.0, 5.0)], true);
        assert!((c.eval(1.0) - 1.0).abs() < 1e-12);
        for i in 0..100 {
            let x = 1.0 + i as f64 / 100.0;
            assert!(c.eval(x) <= 1.0 + 1e-12, "no overshoot on flat segment");
        }
        let l = Curve::new(vec![(0.0, 0.0), (2.0, 4.0)], false);
        assert!((l.eval(0.5) - 1.0).abs() < 1e-12);
        assert_eq!(l.eval(-1.0), 0.0);
        assert_eq!(l.eval(9.0), 4.0);
    }

    #[test]
    fn shifts() {
        let x: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let y = shift_channel(&x, 3.0);
        assert_eq!(y[3], 0.0);
        assert_eq!(y[10], 7.0);
        let s: Vec<f32> = (0..400).map(|i| (2.0 * PI * 0.01 * i as f64).sin() as f32).collect();
        let y = shift_channel(&s, 0.5);
        let expect = (2.0 * PI * 0.01 * (200.0 - 0.5)).sin();
        assert!((y[200] as f64 - expect).abs() < 1e-3);
    }
}
