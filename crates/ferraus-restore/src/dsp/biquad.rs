//! Biquad sections (RBJ cookbook designs), second-order-section cascades, and their
//! frequency response and group delay.

use super::fft::C;
use std::f64::consts::PI;

/// Normalised biquad coefficients (`a0 == 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Coefs {
        Coefs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    fn w(freq: f64, sr: f64) -> f64 {
        2.0 * PI * (freq / sr).clamp(1e-6, 0.4999)
    }

    pub fn lowpass(freq: f64, q: f64, sr: f64) -> Coefs {
        let w = Self::w(freq, sr);
        let (s, c) = w.sin_cos();
        let al = s / (2.0 * q);
        Self::norm((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al)
    }

    pub fn highpass(freq: f64, q: f64, sr: f64) -> Coefs {
        let w = Self::w(freq, sr);
        let (s, c) = w.sin_cos();
        let al = s / (2.0 * q);
        Self::norm((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al)
    }

    /// Constant 0 dB peak gain band-pass.
    pub fn bandpass(freq: f64, q: f64, sr: f64) -> Coefs {
        let w = Self::w(freq, sr);
        let (s, c) = w.sin_cos();
        let al = s / (2.0 * q);
        Self::norm(al, 0.0, -al, 1.0 + al, -2.0 * c, 1.0 - al)
    }

    /// Full-depth notch.
    pub fn notch(freq: f64, q: f64, sr: f64) -> Coefs {
        let w = Self::w(freq, sr);
        let (s, c) = w.sin_cos();
        let al = s / (2.0 * q);
        Self::norm(1.0, -2.0 * c, 1.0, 1.0 + al, -2.0 * c, 1.0 - al)
    }

    /// Notch whose centre attenuation is limited to `depth_db` (negative), built as
    /// `g + (1 - g) * notch`, so the response is exactly `depth_db` at the centre and
    /// returns to unity away from it.
    pub fn notch_depth(freq: f64, q: f64, depth_db: f64, sr: f64) -> Coefs {
        let n = Self::notch(freq, q, sr);
        let g = 10f64.powf(depth_db.min(0.0) / 20.0);
        Coefs {
            b0: g + (1.0 - g) * n.b0,
            b1: g * n.a1 + (1.0 - g) * n.b1,
            b2: g * n.a2 + (1.0 - g) * n.b2,
            a1: n.a1,
            a2: n.a2,
        }
    }

    pub fn peak(freq: f64, q: f64, gain_db: f64, sr: f64) -> Coefs {
        let w = Self::w(freq, sr);
        let (s, c) = w.sin_cos();
        let a = 10f64.powf(gain_db / 40.0);
        let al = s / (2.0 * q);
        Self::norm(1.0 + al * a, -2.0 * c, 1.0 - al * a, 1.0 + al / a, -2.0 * c, 1.0 - al / a)
    }

    pub fn lowshelf(freq: f64, q: f64, gain_db: f64, sr: f64) -> Coefs {
        let w = Self::w(freq, sr);
        let (s, c) = w.sin_cos();
        let a = 10f64.powf(gain_db / 40.0);
        let al = s / (2.0 * q);
        let sa = 2.0 * a.sqrt() * al;
        Self::norm(
            a * ((a + 1.0) - (a - 1.0) * c + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sa),
            (a + 1.0) + (a - 1.0) * c + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sa,
        )
    }

    pub fn highshelf(freq: f64, q: f64, gain_db: f64, sr: f64) -> Coefs {
        let w = Self::w(freq, sr);
        let (s, c) = w.sin_cos();
        let a = 10f64.powf(gain_db / 40.0);
        let al = s / (2.0 * q);
        let sa = 2.0 * a.sqrt() * al;
        Self::norm(
            a * ((a + 1.0) + (a - 1.0) * c + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sa),
            (a + 1.0) - (a - 1.0) * c + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sa,
        )
    }

    /// Complex response at `freq`.
    pub fn response(&self, freq: f64, sr: f64) -> C {
        let w = 2.0 * PI * freq / sr;
        let z1 = C::polar(1.0, -w);
        let z2 = C::polar(1.0, -2.0 * w);
        let num = C::new(self.b0, 0.0) + z1.scale(self.b1) + z2.scale(self.b2);
        let den = C::new(1.0, 0.0) + z1.scale(self.a1) + z2.scale(self.a2);
        num / den
    }

    /// Group delay in samples at `freq`.
    pub fn group_delay(&self, freq: f64, sr: f64) -> f64 {
        let w = 2.0 * PI * freq / sr;
        poly_gd(&[self.b0, self.b1, self.b2], w) - poly_gd(&[1.0, self.a1, self.a2], w)
    }
}

/// Group delay (samples) of the polynomial `sum c_k z^-k` at angular frequency `w`.
fn poly_gd(c: &[f64], w: f64) -> f64 {
    let mut num = C::ZERO;
    let mut den = C::ZERO;
    for (k, v) in c.iter().enumerate() {
        let e = C::polar(*v, -w * k as f64);
        den += e;
        num += e.scale(k as f64);
    }
    if den.norm_sqr() < 1e-300 {
        return 0.0;
    }
    (num / den).re
}

/// Biquad state (transposed direct form II, f64).
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    pub c: Option<Coefs>,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(c: Coefs) -> Biquad {
        Biquad {
            c: Some(c),
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn set(&mut self, c: Coefs) {
        self.c = Some(c);
    }

    #[inline]
    pub fn tick(&mut self, x: f64) -> f64 {
        let c = match &self.c {
            Some(c) => c,
            None => return x,
        };
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        // flush denormals
        if self.z1.abs() < 1e-30 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-30 {
            self.z2 = 0.0;
        }
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// A cascade of biquads with an overall gain.
#[derive(Clone, Debug)]
pub struct Sos {
    pub sections: Vec<Coefs>,
    pub gain: f64,
}

impl Sos {
    pub fn new(sections: Vec<Coefs>) -> Sos {
        Sos {
            sections,
            gain: 1.0,
        }
    }

    pub fn response(&self, freq: f64, sr: f64) -> C {
        let mut h = C::new(self.gain, 0.0);
        for s in &self.sections {
            h *= s.response(freq, sr);
        }
        h
    }

    pub fn magnitude_db(&self, freq: f64, sr: f64) -> f64 {
        20.0 * self.response(freq, sr).abs().max(1e-15).log10()
    }

    pub fn group_delay(&self, freq: f64, sr: f64) -> f64 {
        self.sections.iter().map(|s| s.group_delay(freq, sr)).sum()
    }

    pub fn filter(&self) -> SosFilter {
        SosFilter {
            stages: self.sections.iter().map(|c| Biquad::new(*c)).collect(),
            gain: self.gain,
        }
    }

    /// Filter a channel (f32 in, f32 out).
    pub fn apply(&self, x: &[f32]) -> Vec<f32> {
        let mut f = self.filter();
        x.iter().map(|v| f.tick(*v as f64) as f32).collect()
    }

    /// Filter a channel in place.
    pub fn apply_in_place(&self, x: &mut [f32]) {
        let mut f = self.filter();
        for v in x.iter_mut() {
            *v = f.tick(*v as f64) as f32;
        }
    }

    /// Zero-phase forward-backward filtering with odd-reflection padding (magnitude
    /// response squared, no phase shift).
    pub fn filtfilt(&self, x: &[f32], pad: usize) -> Vec<f64> {
        let n = x.len();
        if n == 0 {
            return Vec::new();
        }
        let pad = pad.min(n.saturating_sub(1));
        let mut ext = Vec::with_capacity(n + 2 * pad);
        let first = x[0] as f64;
        let last = x[n - 1] as f64;
        for i in (1..=pad).rev() {
            ext.push(2.0 * first - x[i] as f64);
        }
        ext.extend(x.iter().map(|v| *v as f64));
        for i in 1..=pad {
            ext.push(2.0 * last - x[n - 1 - i] as f64);
        }
        let mut f = self.filter();
        for v in ext.iter_mut() {
            *v = f.tick(*v);
        }
        let mut f = self.filter();
        for v in ext.iter_mut().rev() {
            *v = f.tick(*v);
        }
        ext[pad..pad + n].to_vec()
    }
}

/// Running state of an `Sos`.
#[derive(Clone, Debug)]
pub struct SosFilter {
    pub stages: Vec<Biquad>,
    pub gain: f64,
}

impl SosFilter {
    #[inline]
    pub fn tick(&mut self, x: f64) -> f64 {
        let mut y = x * self.gain;
        for s in &mut self.stages {
            y = s.tick(y);
        }
        y
    }
}

/// Butterworth lowpass/highpass of any order as an SOS cascade (bilinear, prewarped).
pub fn butterworth(order: usize, freq: f64, sr: f64, highpass: bool) -> Sos {
    let order = order.max(1);
    let mut secs = Vec::new();
    for k in 0..order / 2 {
        let theta = PI * (2 * k + 1) as f64 / (2 * order) as f64;
        let q = 1.0 / (2.0 * theta.sin());
        secs.push(if highpass {
            Coefs::highpass(freq, q, sr)
        } else {
            Coefs::lowpass(freq, q, sr)
        });
    }
    if order % 2 == 1 {
        // first-order section via bilinear transform
        let k = (PI * (freq / sr).clamp(1e-6, 0.4999)).tan();
        let c = if highpass {
            Coefs {
                b0: 1.0 / (1.0 + k),
                b1: -1.0 / (1.0 + k),
                b2: 0.0,
                a1: (k - 1.0) / (k + 1.0),
                a2: 0.0,
            }
        } else {
            Coefs {
                b0: k / (1.0 + k),
                b1: k / (1.0 + k),
                b2: 0.0,
                a1: (k - 1.0) / (k + 1.0),
                a2: 0.0,
            }
        };
        secs.push(c);
    }
    Sos::new(secs)
}

/// Parse a filter spec such as `lowpass:4kHz`, `highpass:200Hz:q=0.7`, `bandpass:1kHz`,
/// `peak:3kHz:+3dB`, `lowshelf:200Hz:-2dB`, `none`.
pub fn parse_filter_spec(spec: &str, sr: f64) -> ferraus_core::Result<Option<Coefs>> {
    use ferraus_core::units;
    let s = spec.trim();
    if s.is_empty() || s == "none" || s == "off" {
        return Ok(None);
    }
    let parts: Vec<&str> = s.split(':').collect();
    let kind = parts[0].trim().to_ascii_lowercase();
    let freq = parts
        .get(1)
        .map(|f| units::parse_freq(f))
        .transpose()?
        .ok_or_else(|| {
            ferraus_core::Error::usage(format!(
                "filter '{s}' needs a frequency, e.g. lowpass:4kHz"
            ))
        })?;
    let mut gain = 0.0;
    let mut q = std::f64::consts::FRAC_1_SQRT_2;
    for p in parts.iter().skip(2) {
        let p = p.trim();
        if let Some(v) = p.strip_prefix("q=") {
            q = v
                .parse::<f64>()
                .map_err(|_| ferraus_core::Error::usage(format!("bad q in '{s}'")))?;
        } else {
            gain = units::parse_db(p)?;
        }
    }
    let q = q.max(0.05);
    Ok(Some(match kind.as_str() {
        "lowpass" | "lp" => Coefs::lowpass(freq, q, sr),
        "highpass" | "hp" => Coefs::highpass(freq, q, sr),
        "bandpass" | "bp" => Coefs::bandpass(freq, q, sr),
        "notch" => Coefs::notch(freq, q, sr),
        "peak" | "bell" => Coefs::peak(freq, q, gain, sr),
        "lowshelf" => Coefs::lowshelf(freq, q, gain, sr),
        "highshelf" => Coefs::highshelf(freq, q, gain, sr),
        other => {
            return Err(ferraus_core::Error::usage(format!(
                "unknown filter type '{other}' (lowpass, highpass, bandpass, notch, peak, lowshelf, highshelf, none)"
            )))
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn butterworth_minus_3db() {
        for order in 1..7 {
            let s = butterworth(order, 1000.0, 48000.0, false);
            assert!((s.magnitude_db(1000.0, 48000.0) + 3.0103).abs() < 0.01, "{order}");
            let h = butterworth(order, 1000.0, 48000.0, true);
            assert!((h.magnitude_db(1000.0, 48000.0) + 3.0103).abs() < 0.01);
        }
    }

    #[test]
    fn depth_limited_notch() {
        let c = Coefs::notch_depth(60.0, 30.0, -40.0, 48000.0);
        let m = 20.0 * c.response(60.0, 48000.0).abs().log10();
        assert!((m + 40.0).abs() < 1e-6);
        assert!(c.response(1000.0, 48000.0).abs() > 0.999);
    }

    #[test]
    fn filtfilt_zero_phase() {
        let s = butterworth(2, 1000.0, 48000.0, false);
        let x: Vec<f32> = (0..4800)
            .map(|i| (2.0 * PI * 200.0 * i as f64 / 48000.0).sin() as f32)
            .collect();
        let y = s.filtfilt(&x, 1000);
        // zero phase: peak alignment preserved
        for i in 1000..3800 {
            assert!((y[i] - x[i] as f64).abs() < 0.01);
        }
    }
}
