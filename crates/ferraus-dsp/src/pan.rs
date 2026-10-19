//! Pan laws, balance, position parsing (`L30`, `right`, `-100..100`), and sample-accurate
//! keyframe automation (`0s=-100,4s=0,8s=100`) with linear or monotone-spline curves.

use ferraus_core::units::Time;
use ferraus_core::{Error, Result};
use std::f64::consts::FRAC_PI_4;

/// How much a centred mono source is attenuated in each channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanLaw {
    /// Constant power (sin/cos): centre −3 dB, `L² + R²` constant.
    Minus3,
    /// Compromise between constant power and constant voltage: centre −4.5 dB.
    Minus4_5,
    /// Constant voltage (`L + R` constant): centre −6 dB.
    Minus6,
    /// Balance-style linear: centre 0 dB, the far side fades linearly.
    Linear,
}

impl PanLaw {
    pub fn parse(s: &str) -> Result<PanLaw> {
        match s.trim().to_ascii_lowercase().replace(' ', "").as_str() {
            "-3db" | "-3" | "3db" | "constant-power" => Ok(PanLaw::Minus3),
            "-4.5db" | "-4.5" | "4.5db" => Ok(PanLaw::Minus4_5),
            "-6db" | "-6" | "6db" | "constant-voltage" => Ok(PanLaw::Minus6),
            "linear" | "0db" | "0" => Ok(PanLaw::Linear),
            other => Err(Error::usage(format!(
                "unknown pan law '{other}': expected -3dB, -4.5dB, -6dB, or linear"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PanLaw::Minus3 => "-3dB",
            PanLaw::Minus4_5 => "-4.5dB",
            PanLaw::Minus6 => "-6dB",
            PanLaw::Linear => "linear",
        }
    }
}

/// Left/right gains placing a mono source at `pos` (−1 hard left … +1 hard right).
#[inline]
pub fn pan_gains(pos: f64, law: PanLaw) -> (f64, f64) {
    let p = pos.clamp(-1.0, 1.0);
    let theta = (p + 1.0) * FRAC_PI_4; // 0 .. π/2
    // exact zeros at the extremes (cos(π/2) is not exactly 0 in floating point)
    let (pl, pr) = if p >= 1.0 {
        (0.0, 1.0)
    } else if p <= -1.0 {
        (1.0, 0.0)
    } else {
        (theta.cos(), theta.sin())
    };
    let (vl, vr) = ((1.0 - p) / 2.0, (1.0 + p) / 2.0);
    match law {
        PanLaw::Minus3 => (pl, pr),
        PanLaw::Minus6 => (vl, vr),
        PanLaw::Minus4_5 => ((pl * vl).sqrt(), (pr * vr).sqrt()),
        PanLaw::Linear => ((1.0 - p).min(1.0), (1.0 + p).min(1.0)),
    }
}

/// Balance gains for a stereo source: the centre is unity; moving right attenuates the left
/// channel along the chosen law (normalised to unity at centre).
#[inline]
pub fn balance_gains(pos: f64, law: PanLaw) -> (f64, f64) {
    let (cl, cr) = pan_gains(0.0, law);
    let (l, r) = pan_gains(pos, law);
    ((l / cl).min(1.0), (r / cr).min(1.0))
}

/// Parse a position: `-100..100` (optionally with `%`), `left`, `center`, `right`, `L30`,
/// `R60`, `C`. Returns −100..100.
pub fn parse_position(s: &str) -> Result<f64> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let v = match lower.as_str() {
        "left" | "l" => -100.0,
        "right" | "r" => 100.0,
        "center" | "centre" | "c" | "middle" | "mid" => 0.0,
        _ => {
            if let Some(n) = lower.strip_prefix('l').filter(|n| !n.is_empty()) {
                -parse_num(n, s)?
            } else if let Some(n) = lower.strip_prefix('r').filter(|n| !n.is_empty()) {
                parse_num(n, s)?
            } else {
                parse_num(lower.trim_end_matches('%'), s)?
            }
        }
    };
    if !(-100.0..=100.0).contains(&v) {
        return Err(Error::usage(format!(
            "pan position '{s}' is outside -100 (left) .. 100 (right)"
        )));
    }
    Ok(v)
}

fn parse_num(n: &str, orig: &str) -> Result<f64> {
    n.trim().parse::<f64>().map_err(|_| {
        Error::usage(format!(
            "cannot parse pan position '{orig}': expected -100..100, left, center, right, L30, or R60"
        ))
    })
}

/// Interpolation between keyframes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Linear,
    /// Monotone cubic (Fritsch–Carlson): smooth, never overshoots between keyframes.
    Spline,
}

impl Curve {
    pub fn parse(s: &str) -> Curve {
        if s.eq_ignore_ascii_case("spline") {
            Curve::Spline
        } else {
            Curve::Linear
        }
    }
}

/// Keyframed automation resolved to frame positions. Values hold before the first and after
/// the last keyframe; at each keyframe frame the value is exactly the keyframe value.
#[derive(Clone, Debug)]
pub struct Keyframes {
    pub frames: Vec<usize>,
    pub values: Vec<f64>,
    pub curve: Curve,
    tangents: Vec<f64>,
}

impl Keyframes {
    /// Build from (frame, value) points (sorted by frame; duplicates keep the last value).
    pub fn new(mut points: Vec<(usize, f64)>, curve: Curve) -> Result<Keyframes> {
        if points.is_empty() {
            return Err(Error::usage("keyframes need at least one point, e.g. 0s=-100"));
        }
        points.sort_by_key(|p| p.0);
        let mut frames: Vec<usize> = Vec::new();
        let mut values: Vec<f64> = Vec::new();
        for (f, v) in points {
            if frames.last() == Some(&f) {
                *values.last_mut().unwrap() = v;
            } else {
                frames.push(f);
                values.push(v);
            }
        }
        let tangents = monotone_tangents(&frames, &values);
        Ok(Keyframes {
            frames,
            values,
            curve,
            tangents,
        })
    }

    /// Parse `0s=-100,4s=0,end=100` against a buffer. `value` parses each value.
    pub fn parse(
        s: &str,
        sample_rate: u32,
        total: usize,
        bpm: Option<f64>,
        curve: Curve,
        value: impl Fn(&str) -> Result<f64>,
    ) -> Result<Keyframes> {
        let mut pts = Vec::new();
        for item in s.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()) {
            let (t, v) = item.split_once('=').ok_or_else(|| {
                Error::usage(format!("keyframe '{item}' should look like 4s=0"))
            })?;
            let frame = Time::parse(t)?.frames(sample_rate, total, bpm)?;
            pts.push((frame, value(v)?));
        }
        Keyframes::new(pts, curve)
    }

    /// Value at a frame.
    pub fn value_at(&self, frame: usize) -> f64 {
        let n = self.frames.len();
        if frame <= self.frames[0] {
            return self.values[0];
        }
        if frame >= self.frames[n - 1] {
            return self.values[n - 1];
        }
        let k = match self.frames.binary_search(&frame) {
            Ok(i) => return self.values[i],
            Err(i) => i - 1,
        };
        self.segment(k, frame as f64)
    }

    fn segment(&self, k: usize, x: f64) -> f64 {
        let (x0, x1) = (self.frames[k] as f64, self.frames[k + 1] as f64);
        let (y0, y1) = (self.values[k], self.values[k + 1]);
        let h = x1 - x0;
        let t = (x - x0) / h;
        match self.curve {
            Curve::Linear => y0 + (y1 - y0) * t,
            Curve::Spline => {
                let (m0, m1) = (self.tangents[k] * h, self.tangents[k + 1] * h);
                let t2 = t * t;
                let t3 = t2 * t;
                (2.0 * t3 - 3.0 * t2 + 1.0) * y0
                    + (t3 - 2.0 * t2 + t) * m0
                    + (-2.0 * t3 + 3.0 * t2) * y1
                    + (t3 - t2) * m1
            }
        }
    }

    /// Render `n` frames of automation.
    pub fn render(&self, n: usize) -> Vec<f64> {
        let mut out = Vec::with_capacity(n);
        let mut k = 0usize;
        let last = self.frames.len() - 1;
        for i in 0..n {
            if i <= self.frames[0] {
                out.push(self.values[0]);
                continue;
            }
            if i >= self.frames[last] {
                out.push(self.values[last]);
                continue;
            }
            while self.frames[k + 1] <= i {
                k += 1;
            }
            if self.frames[k] == i {
                out.push(self.values[k]);
            } else {
                out.push(self.segment(k, i as f64));
            }
        }
        out
    }
}

/// Fritsch–Carlson monotone tangents (per frame unit).
fn monotone_tangents(x: &[usize], y: &[f64]) -> Vec<f64> {
    let n = x.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let d: Vec<f64> = (0..n - 1)
        .map(|i| (y[i + 1] - y[i]) / (x[i + 1] - x[i]) as f64)
        .collect();
    let mut m = vec![0.0; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 {
            0.0
        } else {
            (d[i - 1] + d[i]) / 2.0
        };
    }
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let a = m[i] / d[i];
        let b = m[i + 1] / d[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * d[i];
            m[i + 1] = t * b * d[i];
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions() {
        assert_eq!(parse_position("left").unwrap(), -100.0);
        assert_eq!(parse_position("L30").unwrap(), -30.0);
        assert_eq!(parse_position("R60").unwrap(), 60.0);
        assert_eq!(parse_position("center").unwrap(), 0.0);
        assert_eq!(parse_position("-45").unwrap(), -45.0);
        assert!(parse_position("L130").is_err());
        assert!(parse_position("banana").is_err());
    }

    #[test]
    fn constant_power_law() {
        for i in -100..=100 {
            let (l, r) = pan_gains(i as f64 / 100.0, PanLaw::Minus3);
            let db = 10.0 * (l * l + r * r).log10();
            assert!(db.abs() < 0.1);
        }
        let (l, _) = pan_gains(0.0, PanLaw::Minus6);
        assert!((20.0 * l.log10() + 6.02).abs() < 0.01);
        let (l, _) = pan_gains(0.0, PanLaw::Minus4_5);
        assert!((20.0 * l.log10() + 4.52).abs() < 0.02);
        assert!(pan_gains(-1.0, PanLaw::Minus3).1.abs() < 1e-12);
    }

    #[test]
    fn keyframes_hit_values() {
        let k = Keyframes::parse("0s=-100,1s=0,2s=100", 1000, 3000, None, Curve::Spline, parse_position).unwrap();
        let r = k.render(3000);
        assert_eq!(r[0], -100.0);
        assert_eq!(r[1000], 0.0);
        assert_eq!(r[2000], 100.0);
        assert_eq!(r[2999], 100.0);
        assert_eq!(k.value_at(1000), 0.0);
        assert!(r.windows(2).all(|w| w[1] >= w[0]));
    }
}
