//! Monophony test for `--mode auto`.
//!
//! A recording counts as a single voiced line when the pitch tracker finds voicing in a
//! reasonable share of the non-silent frames and the spectrum of those frames is explained
//! by the harmonics of the tracked f0. For a sample of voiced frames a 46 ms Hann-windowed
//! spectrum is taken at the frame centre and its peaks (within 25 dB of the strongest,
//! 60 Hz to 5 kHz) are classified as harmonic when they lie within
//! `min(max(3 % of k·f0, 1.5 bins), 0.3·f0)` of a harmonic `k·f0`. A frame is polyphonic
//! when more than 20 % of the peak energy is not harmonic (another note sounds), or when
//! neither the fundamental nor the second harmonic is present among peaks within 30 dB of
//! the strongest (the tracker locked onto the common sub-harmonic of a chord). The
//! recording is monophonic when fewer than 30 % of the analysed frames are polyphonic.

use ferraus_pitch::pyin::PitchFrame;
use realfft::RealFftPlanner;
use serde::Serialize;

/// Result of the monophony test.
#[derive(Clone, Debug, Serialize)]
pub struct PolyphonyEstimate {
    /// Share of non-silent frames that the tracker marked voiced.
    pub voiced_fraction: f64,
    /// Mean voicing confidence of voiced frames.
    pub mean_confidence: f64,
    /// Share of analysed voiced frames whose spectrum shows more than one pitch.
    pub polyphonic_fraction: f64,
    /// Frames analysed.
    pub frames_analysed: usize,
    /// The verdict.
    pub monophonic: bool,
}

/// Analyse `samples` (mono) given its pitch track.
pub fn estimate(samples: &[f32], sr: u32, frames: &[PitchFrame]) -> PolyphonyEstimate {
    let loud: Vec<&PitchFrame> = frames.iter().filter(|f| f.rms > 1e-3).collect();
    let voiced: Vec<&PitchFrame> = loud.iter().copied().filter(|f| f.voiced && f.freq_hz > 0.0).collect();
    let voiced_fraction = if loud.is_empty() { 0.0 } else { voiced.len() as f64 / loud.len() as f64 };
    let mean_confidence =
        if voiced.is_empty() { 0.0 } else { voiced.iter().map(|f| f.confidence).sum::<f64>() / voiced.len() as f64 };

    let srf = sr as f64;
    let win_len = ((0.046 * srf) as usize).max(64);
    let n = (win_len * 2).next_power_of_two();
    let hann: Vec<f32> = (0..win_len)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / win_len as f64).cos()) as f32)
        .collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let bin_hz = srf / n as f64;
    let kmin = ((60.0 / bin_hz).ceil() as usize).max(2);
    let kmax = (((5000.0f64).min(0.45 * srf) / bin_hz) as usize).min(n / 2 - 2);

    // analyse at most ~300 evenly spaced voiced frames
    let step = (voiced.len() / 300).max(1);
    let mut analysed = 0usize;
    let mut poly = 0usize;
    let mut mag = vec![0.0f64; n / 2 + 1];
    for f in voiced.iter().step_by(step) {
        let centre = (f.time_s * srf).round() as i64;
        let start = centre - win_len as i64 / 2;
        if start < 0 || (start as usize + win_len) > samples.len() {
            continue;
        }
        input.iter_mut().for_each(|v| *v = 0.0);
        for i in 0..win_len {
            input[i] = samples[start as usize + i] * hann[i];
        }
        if fft.process(&mut input, &mut spec).is_err() {
            continue;
        }
        let mut mx = 0.0f64;
        for (m, c) in mag.iter_mut().zip(&spec) {
            *m = c.norm() as f64;
        }
        for &m in &mag[kmin..=kmax] {
            mx = mx.max(m);
        }
        if mx <= 1e-9 {
            continue;
        }
        let f0 = f.freq_hz;
        let floor25 = mx * 10f64.powf(-25.0 / 20.0);
        let floor30 = mx * 10f64.powf(-30.0 / 20.0);
        let (mut harm_e, mut other_e) = (0.0, 0.0);
        let mut low_harmonic = false;
        for k in kmin..=kmax {
            let m = mag[k];
            if !(m > floor30 && m > mag[k - 1] && m >= mag[k + 1]) {
                continue;
            }
            let (a, b, c) = (mag[k - 1].max(1e-12).ln(), m.ln(), mag[k + 1].max(1e-12).ln());
            let den = a - 2.0 * b + c;
            let off = if den.abs() > 1e-12 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
            let freq = (k as f64 + off) * bin_hz;
            let h = (freq / f0).round().max(1.0);
            let tol = (0.03 * h * f0).max(1.5 * bin_hz).min(0.3 * f0);
            let harmonic = (freq - h * f0).abs() <= tol;
            if harmonic && h <= 2.0 {
                low_harmonic = true;
            }
            if m > floor25 {
                if harmonic {
                    harm_e += m * m;
                } else {
                    other_e += m * m;
                }
            }
        }
        analysed += 1;
        let total = harm_e + other_e;
        if (total > 0.0 && other_e / total > 0.2) || !low_harmonic {
            poly += 1;
        }
    }
    let polyphonic_fraction = if analysed == 0 { 0.0 } else { poly as f64 / analysed as f64 };
    PolyphonyEstimate {
        voiced_fraction: r3(voiced_fraction),
        mean_confidence: r3(mean_confidence),
        polyphonic_fraction: r3(polyphonic_fraction),
        frames_analysed: analysed,
        monophonic: polyphonic_fraction < 0.3 && (voiced_fraction >= 0.2 || loud.is_empty()),
    }
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
