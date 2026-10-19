//! Model pitch trackers for `ferraus-pitch`: CREPE (onnxcrepe export) and SwiftF0, run on
//! ONNX Runtime and registered as `--tracker crepe` / `--tracker swiftf0` by
//! [`crate::register_all`].
//!
//! * CREPE: 16 kHz, 1024-sample frames centred on each hop (the graph normalizes every
//!   frame itself), 360 sigmoid outputs on a 20-cent grid starting at 1997.38 cents above
//!   10 Hz. Decoding is CREPE's local weighted average of ±4 bins around the peak; the voicing
//!   confidence is the peak salience. `crepe` resolves to the full model when it is cached and
//!   to the verified `pitch/crepe-tiny` otherwise.
//! * SwiftF0: 16 kHz audio in, one pitch/confidence pair per 256-sample hop (frame `k` at
//!   `k × 16 ms`), searched between `fmin` and `fmax` inside the model range
//!   46.875–2093.75 Hz; a frame is voiced at confidence ≥ 0.5. Long inputs run in 30 s
//!   windows with the receptive-field context the upstream wrapper uses, so results equal a
//!   single pass. The native 16 ms grid is interpolated onto the requested hop.

use crate::session::{load_for_hinted, ModelSession, TensorF32};
use ferraus_core::resample::{resample_channel, Quality};
use ferraus_core::{Error, OpCtx, Result};
use ferraus_pitch::pyin::{hop_samples, PitchFrame};
use ferraus_pitch::tracker::{PitchTracker, TrackOptions, TrackerKind};
use std::sync::Arc;

/// Model rate of both trackers.
pub const PITCH_RATE: u32 = 16_000;

/// Frames quieter than this (RMS, full scale = 1) are never voiced.
const SILENCE_RMS: f64 = 1e-3;

/// Resolve `name` through the download policy and load it on the context's device.
pub fn model_session(ctx: &mut OpCtx, name: &str) -> Result<(Arc<ModelSession>, (String, String, String))> {
    let m = ferraus_models::ensure_model(ctx, name)?;
    let session = load_for_hinted(ctx, &m.path, &m.spec.provider_hints)?;
    let sha = m.spec.sha256.clone().unwrap_or_default();
    Ok((session, (m.entry.name.clone(), m.entry.version.clone(), sha)))
}

/// Register the CREPE and SwiftF0 factories with `ferraus-pitch`.
pub fn register() {
    ferraus_pitch::tracker::register_model_tracker(TrackerKind::Crepe, crepe_factory);
    ferraus_pitch::tracker::register_model_tracker(TrackerKind::SwiftF0, swiftf0_factory);
}

fn crepe_factory(ctx: &mut OpCtx) -> Result<Box<dyn PitchTracker>> {
    // the full model is used when it was pulled; otherwise the verified tiny variant
    let full = ferraus_models::ensure_model_with(
        ctx,
        "pitch/crepe",
        &ferraus_models::EnsureOptions { allow_download: Some(false), ..Default::default() },
    );
    let name = if full.is_ok() { "pitch/crepe" } else { "pitch/crepe-tiny" };
    let (session, model) = model_session(ctx, name)?;
    Ok(Box::new(CrepeTracker { session, model }))
}

fn swiftf0_factory(ctx: &mut OpCtx) -> Result<Box<dyn PitchTracker>> {
    let (session, model) = model_session(ctx, "pitch/swiftf0")?;
    Ok(Box::new(SwiftF0Tracker { session, model }))
}

fn to_model_rate(samples: &[f32], sr: u32) -> Vec<f32> {
    resample_channel(samples, sr, PITCH_RATE, Quality::High)
}

/// Frame grid of the input: `(hop in input samples, frame count)`, matching pYIN.
fn grid(n: usize, sr: u32, hop_s: f64) -> (usize, usize) {
    let hop = hop_samples(sr, hop_s);
    (hop, n / hop + 1)
}

/// RMS of `x` over `[c - half, c + half)` (zero outside).
fn rms_around(x: &[f32], c: isize, half: isize) -> f64 {
    let mut s = 0.0f64;
    for i in (c - half)..(c + half) {
        if i >= 0 && (i as usize) < x.len() {
            let v = x[i as usize] as f64;
            s += v * v;
        }
    }
    (s / (2 * half).max(1) as f64).sqrt()
}

/// Voicing with hysteresis: a frame at or above `high` starts a voiced run, which extends
/// to neighbouring frames at or above `low` as long as the pitch moves less than a semitone
/// from frame to frame.
pub fn hysteresis_voicing(freq: &[f64], conf: &[f64], high: f64, low: f64) -> Vec<bool> {
    let n = freq.len().min(conf.len());
    let mut v: Vec<bool> = (0..n).map(|k| conf[k] >= high && freq[k] > 0.0).collect();
    let near = |a: f64, b: f64| a > 0.0 && b > 0.0 && (1200.0 * (a / b).log2()).abs() < 100.0;
    for k in 1..n {
        if !v[k] && v[k - 1] && conf[k] >= low && near(freq[k], freq[k - 1]) {
            v[k] = true;
        }
    }
    for k in (0..n.saturating_sub(1)).rev() {
        if !v[k] && v[k + 1] && conf[k] >= low && near(freq[k], freq[k + 1]) {
            v[k] = true;
        }
    }
    v
}

// ---------------------------------------------------------------------------------------
// CREPE

/// CREPE frame length at 16 kHz.
pub const CREPE_FRAME: usize = 1024;
/// Number of salience bins.
pub const CREPE_BINS: usize = 360;
/// Voicing hysteresis on the peak salience: a run starts at `CREPE_VOICING` and extends
/// through continuous-pitch neighbours down to `CREPE_VOICING_LOW` (CREPE-tiny's salience
/// dips on low voices; white or brown noise rarely exceeds 0.45 and never holds a pitch).
pub const CREPE_VOICING: f64 = 0.5;
pub const CREPE_VOICING_LOW: f64 = 0.25;

/// Cents (relative to 10 Hz) of salience bin `i`.
pub fn crepe_bin_cents(i: f64) -> f64 {
    20.0 * i + 1_997.379_408_437_619
}

pub fn cents_to_hz(c: f64) -> f64 {
    10.0 * 2f64.powf(c / 1200.0)
}

pub fn hz_to_cents(f: f64) -> f64 {
    1200.0 * (f / 10.0).log2()
}

/// Decode one salience vector: CREPE's local weighted average of the ±4 bins around the
/// peak, with the peak searched only between `fmin` and `fmax`. Returns `(Hz, confidence)`
/// where the confidence is the peak salience.
pub fn crepe_decode(salience: &[f32], fmin: f64, fmax: f64) -> (f64, f64) {
    let n = salience.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let lo_c = hz_to_cents(fmin.max(1.0));
    let hi_c = hz_to_cents(fmax.max(1.0));
    let mut best: Option<usize> = None;
    for (i, s) in salience.iter().enumerate() {
        let c = crepe_bin_cents(i as f64);
        if c < lo_c - 10.0 || c > hi_c + 10.0 {
            continue;
        }
        if best.is_none_or(|b| *s > salience[b]) {
            best = Some(i);
        }
    }
    let Some(center) = best else { return (0.0, 0.0) };
    let start = center.saturating_sub(4);
    let end = (center + 5).min(n);
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for (i, s) in salience.iter().enumerate().take(end).skip(start) {
        let s = *s as f64;
        num += s * crepe_bin_cents(i as f64);
        den += s;
    }
    let cents = if den > 0.0 { num / den } else { crepe_bin_cents(center as f64) };
    (cents_to_hz(cents), salience[center] as f64)
}

/// `--tracker crepe`.
pub struct CrepeTracker {
    session: Arc<ModelSession>,
    model: (String, String, String),
}

impl CrepeTracker {
    pub fn new(session: Arc<ModelSession>, model: (String, String, String)) -> CrepeTracker {
        CrepeTracker { session, model }
    }

    /// Salience for frames centred on the given 16 kHz sample positions.
    pub fn salience(&self, x16: &[f32], centres: &[isize]) -> Result<Vec<Vec<f32>>> {
        let input = self.session.input_names.first().cloned().unwrap_or_else(|| "frames".into());
        let mut out = Vec::with_capacity(centres.len());
        const BATCH: usize = 256;
        for chunk in centres.chunks(BATCH) {
            let mut data = Vec::with_capacity(chunk.len() * CREPE_FRAME);
            for &c in chunk {
                let start = c - (CREPE_FRAME / 2) as isize;
                data.extend((0..CREPE_FRAME as isize).map(|j| {
                    let i = start + j;
                    if i >= 0 && (i as usize) < x16.len() {
                        x16[i as usize]
                    } else {
                        0.0
                    }
                }));
            }
            let res =
                self.session.run(vec![TensorF32::new(&input, vec![chunk.len() as i64, CREPE_FRAME as i64], data)])?;
            let o = res.into_iter().next().ok_or_else(|| Error::processing("CREPE model has no output"))?;
            if o.data.len() != chunk.len() * CREPE_BINS {
                return Err(Error::processing(format!("unexpected CREPE output shape {:?}", o.shape)));
            }
            out.extend(o.data.chunks(CREPE_BINS).map(|c| c.to_vec()));
        }
        Ok(out)
    }
}

impl PitchTracker for CrepeTracker {
    fn name(&self) -> &str {
        "crepe"
    }

    fn model(&self) -> Option<(String, String, String)> {
        Some(self.model.clone())
    }

    fn track(&self, samples: &[f32], sr: u32, opts: &TrackOptions) -> Result<Vec<PitchFrame>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let x16 = to_model_rate(samples, sr);
        let (hop, n) = grid(samples.len(), sr, opts.hop_s);
        let times: Vec<f64> = (0..n).map(|k| (k * hop) as f64 / sr as f64).collect();
        let centres: Vec<isize> = times.iter().map(|t| (t * PITCH_RATE as f64).round() as isize).collect();
        let sal = self.salience(&x16, &centres)?;
        let decoded: Vec<(f64, f64)> = sal.iter().map(|s| crepe_decode(s, opts.fmin, opts.fmax)).collect();
        let rms: Vec<f64> = centres.iter().map(|c| rms_around(&x16, *c, (CREPE_FRAME / 2) as isize)).collect();
        let freq: Vec<f64> = decoded.iter().map(|d| d.0).collect();
        let conf: Vec<f64> = decoded.iter().zip(&rms).map(|(d, r)| if *r >= SILENCE_RMS { d.1 } else { 0.0 }).collect();
        let voiced = hysteresis_voicing(&freq, &conf, CREPE_VOICING, CREPE_VOICING_LOW);
        Ok((0..n)
            .map(|k| PitchFrame {
                time_s: times[k],
                freq_hz: if voiced[k] { freq[k] } else { 0.0 },
                confidence: decoded[k].1,
                voiced: voiced[k],
                rms: rms[k],
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------------------
// SwiftF0

pub const SWIFTF0_HOP: usize = 256;
pub const SWIFTF0_FMIN: f64 = 46.875;
pub const SWIFTF0_FMAX: f64 = 2093.75;
/// A voiced run starts at this confidence (the model's calibration point) and extends
/// through continuous-pitch neighbours down to `SWIFTF0_VOICING_LOW`.
pub const SWIFTF0_VOICING: f64 = 0.5;
pub const SWIFTF0_VOICING_LOW: f64 = 0.3;
/// Hops whose peak is below this get confidence 0 (digital silence gives random output).
const SWIFTF0_SILENCE_PEAK: f32 = 1e-3;
/// Windowing of long inputs (frames): receptive field and window length of the wrapper.
const SWIFTF0_LOOKAHEAD: usize = 10;
const SWIFTF0_LEFT: usize = 11;
const SWIFTF0_WINDOW: usize = 1875;

/// `--tracker swiftf0`.
pub struct SwiftF0Tracker {
    session: Arc<ModelSession>,
    model: (String, String, String),
}

impl SwiftF0Tracker {
    pub fn new(session: Arc<ModelSession>, model: (String, String, String)) -> SwiftF0Tracker {
        SwiftF0Tracker { session, model }
    }

    fn run(&self, audio: &[f32], fmin: f32, fmax: f32) -> Result<(Vec<f32>, Vec<f32>)> {
        // the model needs at least one analysis window; pad short windows with silence
        let mut a = audio.to_vec();
        if a.len() < 4 * SWIFTF0_HOP {
            a.resize(4 * SWIFTF0_HOP, 0.0);
        }
        let out = self.session.run(vec![
            TensorF32::new("audio", vec![1, a.len() as i64], a),
            TensorF32::new("fmin", vec![], vec![fmin]),
            TensorF32::new("fmax", vec![], vec![fmax]),
        ])?;
        let pick = |n: &str| {
            out.iter()
                .find(|t| t.name == n)
                .map(|t| t.data.clone())
                .ok_or_else(|| Error::processing(format!("SwiftF0 model produced no '{n}' output")))
        };
        let (pitch, mut conf) = (pick("pitch")?, pick("confidence")?);
        for (k, c) in conf.iter_mut().enumerate() {
            let s = (k * SWIFTF0_HOP).min(audio.len());
            let e = ((k + 1) * SWIFTF0_HOP).min(audio.len());
            if audio[s..e].iter().fold(0f32, |m, v| m.max(v.abs())) < SWIFTF0_SILENCE_PEAK {
                *c = 0.0;
            }
        }
        Ok((pitch, conf))
    }

    /// Native frames (`k × 16 ms`) for a 16 kHz signal: `(pitch Hz, confidence)`.
    pub fn native(&self, x16: &[f32], fmin: f64, fmax: f64) -> Result<(Vec<f32>, Vec<f32>)> {
        let lo = fmin.max(SWIFTF0_FMIN);
        let hi = fmax.min(SWIFTF0_FMAX);
        if lo >= hi {
            return Err(Error::usage(format!(
                "SwiftF0 searches {SWIFTF0_FMIN}–{SWIFTF0_FMAX} Hz; the range {fmin:.1}–{fmax:.1} Hz is outside it"
            )));
        }
        let n = (x16.len() / SWIFTF0_HOP).max(1);
        let (mut pitch, mut conf) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let mut start = 0;
        while start < n {
            let end = (start + SWIFTF0_WINDOW).min(n);
            let left = start.saturating_sub(SWIFTF0_LEFT);
            let a = left * SWIFTF0_HOP;
            let b = if end < n { ((end + SWIFTF0_LOOKAHEAD) * SWIFTF0_HOP).min(x16.len()) } else { x16.len() };
            let (p, c) = self.run(&x16[a..b], lo as f32, hi as f32)?;
            for k in (start - left)..(end - left) {
                pitch.push(p.get(k).copied().unwrap_or(0.0));
                conf.push(c.get(k).copied().unwrap_or(0.0));
            }
            start = end;
        }
        Ok((pitch, conf))
    }
}

impl PitchTracker for SwiftF0Tracker {
    fn name(&self) -> &str {
        "swiftf0"
    }

    fn model(&self) -> Option<(String, String, String)> {
        Some(self.model.clone())
    }

    fn track(&self, samples: &[f32], sr: u32, opts: &TrackOptions) -> Result<Vec<PitchFrame>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let x16 = to_model_rate(samples, sr);
        let (pitch, conf) = self.native(&x16, opts.fmin, opts.fmax)?;
        let pf: Vec<f64> = pitch.iter().map(|p| *p as f64).collect();
        let cf: Vec<f64> = conf.iter().map(|c| *c as f64).collect();
        let nv = hysteresis_voicing(&pf, &cf, SWIFTF0_VOICING, SWIFTF0_VOICING_LOW);
        let (hop, n) = grid(samples.len(), sr, opts.hop_s);
        let period = SWIFTF0_HOP as f64 / PITCH_RATE as f64;
        let last = pitch.len().saturating_sub(1);
        Ok((0..n)
            .map(|k| {
                let t = (k * hop) as f64 / sr as f64;
                let p = t / period;
                let k0 = (p.floor() as usize).min(last);
                let k1 = (k0 + 1).min(last);
                let w = (p - k0 as f64).clamp(0.0, 1.0);
                let c = conf[k0] as f64 * (1.0 - w) + conf[k1] as f64 * w;
                let (v0, v1) = (nv[k0], nv[k1]);
                let f = match (v0, v1) {
                    (true, true) => 2f64.powf((pitch[k0] as f64).log2() * (1.0 - w) + (pitch[k1] as f64).log2() * w),
                    (true, false) if w < 0.5 => pitch[k0] as f64,
                    (false, true) if w >= 0.5 => pitch[k1] as f64,
                    _ => 0.0,
                };
                let rms = rms_around(&x16, (t * PITCH_RATE as f64).round() as isize, 256);
                let voiced = f > 0.0 && rms >= SILENCE_RMS;
                PitchFrame { time_s: t, freq_hz: if voiced { f } else { 0.0 }, confidence: c, voiced, rms }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A salience vector with a Gaussian bump centred on `cents` (σ = 25 cents, CREPE's
    /// training target).
    fn bump(cents: f64, peak: f32) -> Vec<f32> {
        (0..CREPE_BINS)
            .map(|i| {
                let d = crepe_bin_cents(i as f64) - cents;
                peak * (-(d * d) / (2.0 * 25.0 * 25.0)).exp() as f32
            })
            .collect()
    }

    #[test]
    fn decoding_recovers_off_grid_pitches() {
        for f in [55.0, 100.0, 220.0, 261.63, 440.0, 987.77, 1500.0] {
            let (hz, conf) = crepe_decode(&bump(hz_to_cents(f), 0.9), 30.0, 2000.0);
            let err = 1200.0 * (hz / f).log2();
            assert!(err.abs() < 2.0, "{f} Hz decoded as {hz} ({err} cents)");
            // the peak bin sits at most 10 cents off the bump centre
            assert!(conf > 0.8 && conf <= 0.9, "{conf}");
        }
    }

    #[test]
    fn decoding_respects_the_search_range() {
        // two bumps: the stronger one (880 Hz) lies outside fmax
        let a = bump(hz_to_cents(220.0), 0.6);
        let b = bump(hz_to_cents(880.0), 0.95);
        let s: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x.max(*y)).collect();
        let (hz, conf) = crepe_decode(&s, 60.0, 500.0);
        assert!((1200.0 * (hz / 220.0).log2()).abs() < 2.0, "{hz}");
        assert!(conf > 0.5 && conf <= 0.6, "{conf}");
        let (hz, _) = crepe_decode(&s, 60.0, 1200.0);
        assert!((1200.0 * (hz / 880.0).log2()).abs() < 2.0, "{hz}");
        assert_eq!(crepe_decode(&[], 60.0, 500.0), (0.0, 0.0));
    }

    #[test]
    fn hysteresis_extends_runs_through_continuous_pitch_only() {
        let f = [200.0, 201.0, 202.0, 203.0, 450.0, 204.0, 205.0, 300.0];
        let c = [0.3, 0.3, 0.6, 0.3, 0.3, 0.3, 0.1, 0.9];
        let v = hysteresis_voicing(&f, &c, 0.5, 0.25);
        assert_eq!(v, vec![true, true, true, true, false, false, false, true]);
        // no strong frame: nothing voiced
        assert!(hysteresis_voicing(&f[..2], &c[..2], 0.5, 0.25).iter().all(|v| !v));
    }

    #[test]
    fn bin_mapping_matches_crepe() {
        // bin 0 ≈ 31.7 Hz, bin 359 ≈ 2005.5 Hz
        assert!((cents_to_hz(crepe_bin_cents(0.0)) - 31.70).abs() < 0.01);
        assert!((cents_to_hz(crepe_bin_cents(359.0)) - 2005.5).abs() < 0.2);
        assert!((hz_to_cents(cents_to_hz(3456.0)) - 3456.0).abs() < 1e-9);
    }
}
