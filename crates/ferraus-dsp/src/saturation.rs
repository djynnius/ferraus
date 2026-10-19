//! Waveshaping saturation (tape, tube, soft, hard) with 2x oversampling, DC blocking,
//! unity small-signal gain, and an optional post tone tilt.

use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::oversample::Oversampler;
use crate::util::{db_to_lin, DcBlocker, OnePoleLp};

/// Saturation character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SatType {
    /// Gentle arctangent curve with a touch of high-frequency loss.
    Tape,
    /// Asymmetric curve (even harmonics), DC blocked.
    Tube,
    /// tanh.
    Soft,
    /// Hard clip at ±1.
    Hard,
}

impl SatType {
    pub fn parse(s: &str) -> SatType {
        match s.to_ascii_lowercase().as_str() {
            "tube" => SatType::Tube,
            "soft" => SatType::Soft,
            "hard" => SatType::Hard,
            _ => SatType::Tape,
        }
    }
}

const TUBE_BIAS: f32 = 0.25;

/// The static curve of each type, normalised to unit slope at the origin.
#[inline]
pub fn shape(kind: SatType, x: f32) -> f32 {
    match kind {
        SatType::Tape => {
            let k = std::f32::consts::FRAC_PI_2;
            x.mul_add(k, 0.0).atan() / k
        }
        SatType::Tube => {
            let tb = TUBE_BIAS.tanh();
            let slope = 1.0 - tb * tb;
            ((x + TUBE_BIAS).tanh() - tb) / slope
        }
        SatType::Soft => x.tanh(),
        SatType::Hard => x.clamp(-1.0, 1.0),
    }
}

/// A saturation stage for one channel.
pub struct Saturator {
    kind: SatType,
    drive: f32,
    os: Oversampler,
    dc: DcBlocker,
    tape_lp: Option<OnePoleLp>,
    tone: Option<(Biquad, Biquad)>,
}

impl Saturator {
    /// `drive_db` boosts into the curve (and is removed after it, so small signals pass at
    /// unity). `tone` in −1..1 tilts the result darker/brighter by up to ±6 dB around 1 kHz.
    pub fn new(kind: SatType, drive_db: f32, tone: f32, sample_rate: f64) -> Saturator {
        let tone = if tone.abs() > 1e-4 {
            let g = 6.0 * tone.clamp(-1.0, 1.0) as f64;
            Some((
                Biquad::new(Coeffs::design(FilterType::LowShelf, 1000.0, 0.5, -g / 2.0, sample_rate)),
                Biquad::new(Coeffs::design(FilterType::HighShelf, 1000.0, 0.5, g / 2.0, sample_rate)),
            ))
        } else {
            None
        };
        Saturator {
            kind,
            drive: db_to_lin(drive_db),
            os: Oversampler::new(2, 16),
            dc: DcBlocker::new(5.0, sample_rate),
            tape_lp: (kind == SatType::Tape).then(|| OnePoleLp::new(16000.0, sample_rate)),
            tone,
        }
    }

    /// Process a channel in place (same length, latency compensated).
    pub fn process(&mut self, x: &mut [f32]) {
        let (kind, d) = (self.kind, self.drive);
        self.os.process_with(x, |v| shape(kind, v * d) / d);
        for v in x.iter_mut() {
            let mut s = *v;
            if kind == SatType::Tube {
                s = self.dc.process(s);
            }
            if let Some(lp) = &mut self.tape_lp {
                s = lp.process(s);
            }
            if let Some((a, b)) = &mut self.tone {
                s = b.process(a.process(s));
            }
            *v = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_slope_at_origin() {
        for k in [SatType::Tape, SatType::Tube, SatType::Soft, SatType::Hard] {
            let s = shape(k, 1e-4) / 1e-4;
            assert!((s - 1.0).abs() < 1e-2, "{k:?}: {s}");
            assert!(shape(k, 50.0).abs() < 2.0);
        }
    }
}
