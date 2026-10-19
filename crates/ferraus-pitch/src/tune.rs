//! Stage 2 of pitch correction: deciding where each note should go and building the
//! per-frame correction curve.
//!
//! * Each segmented note gets one target: the nearest note of the scale, or the written
//!   note of a target melody (left alone when it is farther than the tolerance).
//! * Within a note the deviation from the target is smoothed by a one-pole filter with the
//!   `retune` time constant (0 = instant). With vibrato preservation the vibrato (the
//!   fast component around the note's slow pitch trend, which contains the 4-8 Hz band) is
//!   removed before smoothing and so passes through untouched. Each note starts
//!   uncorrected and glides to its target, which keeps scoops into notes natural.
//! * `strength` scales the curve; `humanize` adds a bounded, seeded random drift.

use crate::key::nearest_in_scale;
use crate::notes::{track_hop, Note};
use crate::pyin::PitchFrame;
use crate::util::Rng;
use ferraus_core::units::{hz_to_midi, midi_to_name};
use ferraus_core::{Error, Result};
use serde::Serialize;

/// Correction settings.
#[derive(Clone, Debug)]
pub struct TuneOptions {
    /// 0..1, scales the correction curve.
    pub strength: f64,
    /// One-pole time constant of the correction (seconds); 0 = instant.
    pub retune_s: f64,
    /// 0..1, bounded random drift (±12 cents at 1).
    pub humanize: f64,
    /// Keep vibrato out of the correction.
    pub vibrato_preserve: bool,
    /// Seed for `humanize`.
    pub seed: u64,
}

impl Default for TuneOptions {
    fn default() -> Self {
        TuneOptions { strength: 1.0, retune_s: 0.025, humanize: 0.0, vibrato_preserve: true, seed: 0 }
    }
}

/// A written note of a target melody.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MelodyNote {
    pub start_s: f64,
    pub end_s: f64,
    pub midi: f64,
}

/// What notes are pulled to.
#[derive(Clone, Debug)]
pub enum Target {
    /// Allowed pitch classes (0 = C).
    Scale(Vec<i32>),
    /// A written melody; notes farther than `tolerance_cents` from it are left alone.
    Melody { notes: Vec<MelodyNote>, tolerance_cents: f64 },
}

/// The decision for one note.
#[derive(Clone, Debug, Serialize)]
pub struct NotePlan {
    pub start_s: f64,
    pub end_s: f64,
    /// Detected note centre (fractional MIDI).
    pub midi: f64,
    pub note: String,
    pub detected_hz: f64,
    /// Target (MIDI) or `None` when left alone.
    pub target_midi: Option<f64>,
    pub target_note: Option<String>,
    /// Target minus detected, cents.
    pub deviation_cents: Option<f64>,
    pub confidence: f64,
    pub peak_rms: f64,
    /// Why the note was left alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Choose a target for every note.
pub fn plan_targets(notes: &[Note], target: &Target) -> Vec<NotePlan> {
    notes
        .iter()
        .map(|n| {
            let (tgt, reason) = match target {
                Target::Scale(pcs) => (Some(nearest_in_scale(n.midi, pcs)), None),
                Target::Melody { notes: mel, tolerance_cents } => {
                    // written note with the largest overlap, else the nearest onset ≤ 100 ms
                    let best = mel
                        .iter()
                        .map(|m| (m, (m.end_s.min(n.end_s) - m.start_s.max(n.start_s)).max(0.0)))
                        .filter(|(_, o)| *o > 0.0)
                        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                        .map(|(m, _)| m)
                        .or_else(|| {
                            mel.iter().filter(|m| (m.start_s - n.start_s).abs() <= 0.1).min_by(|a, b| {
                                (a.start_s - n.start_s)
                                    .abs()
                                    .partial_cmp(&(b.start_s - n.start_s).abs())
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                        });
                    match best {
                        None => (None, Some("no written note at this time".to_string())),
                        Some(m) => {
                            let d = (m.midi - n.midi) * 100.0;
                            if d.abs() > *tolerance_cents {
                                (
                                    None,
                                    Some(format!(
                                        "{:.0} cents from the written {} (tolerance {:.0} cents)",
                                        d.abs(),
                                        midi_to_name(m.midi.round() as i32),
                                        tolerance_cents
                                    )),
                                )
                            } else {
                                (Some(m.midi), None)
                            }
                        }
                    }
                }
            };
            NotePlan {
                start_s: r2(n.start_s * 100.0) / 100.0,
                end_s: r2(n.end_s * 100.0) / 100.0,
                midi: r2(n.midi * 100.0) / 100.0,
                note: midi_to_name(n.midi.round() as i32),
                detected_hz: r2(n.hz()),
                target_midi: tgt,
                target_note: tgt.map(|t| midi_to_name(t.round() as i32)),
                deviation_cents: tgt.map(|t| r2((t - n.midi) * 100.0)),
                confidence: r2(n.confidence),
                peak_rms: (n.peak_rms * 1e4).round() / 1e4,
                reason,
            }
        })
        .collect()
}

/// Vibrato component (cents) of a note's pitch residual: the residual minus its slow
/// trend (Hann-weighted ±200 ms average), lightly smoothed (3-tap) to drop tracker jitter.
pub fn vibrato_component(residual: &[f64], hop_s: f64) -> Vec<f64> {
    let n = residual.len();
    let h = ((0.2 / hop_s).round() as usize).max(1);
    let w: Vec<f64> = (0..=2 * h)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::PI * 2.0 * (i as f64 + 1.0) / (2 * h + 2) as f64).cos())
        .collect();
    let trend: Vec<f64> = (0..n)
        .map(|i| {
            let (mut s, mut ws) = (0.0, 0.0);
            for (k, wk) in w.iter().enumerate() {
                let j = i as isize + k as isize - h as isize;
                if j >= 0 && (j as usize) < n {
                    s += wk * residual[j as usize];
                    ws += wk;
                }
            }
            if ws > 0.0 {
                s / ws
            } else {
                0.0
            }
        })
        .collect();
    let raw: Vec<f64> = residual.iter().zip(&trend).map(|(r, t)| r - t).collect();
    let mut v: Vec<f64> = (0..n)
        .map(|i| {
            let a = raw[i.saturating_sub(1)];
            let c = raw[(i + 1).min(n - 1)];
            0.25 * a + 0.5 * raw[i] + 0.25 * c
        })
        .collect();
    // Gate: only a periodic 4-8 Hz oscillation counts as vibrato. Scoops, glides and note
    // transitions also leave a residual around the trend, but it is not periodic.
    let lag_lo = ((0.125 / hop_s).round() as usize).max(1);
    let lag_hi = ((0.25 / hop_s).round() as usize).max(lag_lo);
    if n < 2 * lag_hi || n as f64 * hop_s < 0.25 {
        return vec![0.0; n];
    }
    let edge = ((0.06 / hop_s).round() as usize).min(n / 4);
    let core = &v[edge..n - edge];
    let r0: f64 = core.iter().map(|x| x * x).sum();
    let rms = (r0 / core.len().max(1) as f64).sqrt();
    let periodic = (lag_lo..=lag_hi.min(core.len().saturating_sub(1)))
        .map(|l| {
            let num: f64 = core.iter().zip(&core[l..]).map(|(a, b)| a * b).sum();
            num / r0.max(1e-12) * core.len() as f64 / (core.len() - l) as f64
        })
        .fold(f64::NEG_INFINITY, f64::max);
    if periodic < 0.4 || rms < 3.0 {
        return vec![0.0; n];
    }
    // fade in/out over 60 ms so note transitions are left to the retune smoother
    for i in 0..edge {
        let g = 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / edge as f64).cos();
        v[i] *= g;
        v[n - 1 - i] *= g;
    }
    v
}

/// Per-frame correction in cents (0 outside notes and for notes left alone).
pub fn correction_curve(frames: &[PitchFrame], notes: &[Note], plans: &[NotePlan], opts: &TuneOptions) -> Vec<f64> {
    let hop = track_hop(frames);
    let mut curve = vec![0.0f64; frames.len()];
    let alpha = if opts.retune_s <= 0.0 { 1.0 } else { 1.0 - (-hop / opts.retune_s).exp() };
    let mut rng = Rng::new(opts.seed);
    let amp = 12.0 * opts.humanize.clamp(0.0, 1.0);
    let beta = 1.0 - (-2.0 * std::f64::consts::PI * 2.0 * hop).exp();
    for (n, plan) in notes.iter().zip(plans) {
        let Some(target) = plan.target_midi else {
            continue;
        };
        let (s, e) = (n.start_frame, n.end_frame.min(frames.len()));
        if e <= s {
            continue;
        }
        // pitch in cents with unvoiced gaps filled by interpolation
        let mut p: Vec<Option<f64>> = (s..e)
            .map(|k| {
                let f = &frames[k];
                (f.voiced && f.freq_hz > 0.0).then(|| hz_to_midi(f.freq_hz) * 100.0)
            })
            .collect();
        fill_gaps(&mut p);
        let p: Vec<f64> = p.into_iter().map(|v| v.unwrap_or(n.midi * 100.0)).collect();
        let centre = n.midi * 100.0;
        let vib = if opts.vibrato_preserve {
            let res: Vec<f64> = p.iter().map(|v| v - centre).collect();
            vibrato_component(&res, hop)
        } else {
            vec![0.0; p.len()]
        };
        // every note starts uncorrected and glides to its target at the retune speed
        let mut y = 0.0;
        let u = rng.bipolar();
        let mut drift = 0.0f64;
        for (i, k) in (s..e).enumerate() {
            let dev = target * 100.0 - (p[i] - vib[i]);
            y += alpha * (dev - y);
            let mut c = opts.strength * y;
            if amp > 0.0 {
                drift += beta * (rng.bipolar() - drift);
                let d = (0.5 * u + 0.5 * (drift / 0.3)).clamp(-1.0, 1.0);
                c += amp * d;
            }
            curve[k] = c;
        }
    }
    curve
}

fn fill_gaps(v: &mut [Option<f64>]) {
    let n = v.len();
    let mut last: Option<(usize, f64)> = None;
    let mut i = 0;
    while i < n {
        if let Some(x) = v[i] {
            if let Some((li, lx)) = last {
                for (j, slot) in v.iter_mut().enumerate().take(i).skip(li + 1) {
                    let f = (j - li) as f64 / (i - li) as f64;
                    *slot = Some(lx + (x - lx) * f);
                }
            } else {
                for slot in v.iter_mut().take(i) {
                    *slot = Some(x);
                }
            }
            last = Some((i, x));
        }
        i += 1;
    }
    if let Some((li, lx)) = last {
        for slot in v.iter_mut().skip(li + 1) {
            *slot = Some(lx);
        }
    }
}

/// Read the notes of a standard MIDI file (all tracks and channels) with times from its
/// tempo map.
pub fn parse_midi_melody(bytes: &[u8]) -> Result<Vec<MelodyNote>> {
    use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
    let smf = Smf::parse(bytes).map_err(|e| Error::input(format!("cannot read MIDI file: {e}")))?;
    // tempo map (absolute tick → microseconds per quarter)
    let mut tempos: Vec<(u64, u32)> = Vec::new();
    for tr in &smf.tracks {
        let mut tick = 0u64;
        for ev in tr {
            tick += ev.delta.as_int() as u64;
            if let TrackEventKind::Meta(MetaMessage::Tempo(t)) = ev.kind {
                tempos.push((tick, t.as_int()));
            }
        }
    }
    tempos.sort_by_key(|t| t.0);
    let to_s: Box<dyn Fn(u64) -> f64> = match smf.header.timing {
        Timing::Metrical(ppq) => {
            let ppq = ppq.as_int().max(1) as f64;
            let tempos = tempos.clone();
            Box::new(move |tick: u64| {
                let mut s = 0.0;
                let mut last_tick = 0u64;
                let mut us = 500_000.0;
                for &(t, tempo) in &tempos {
                    if t >= tick {
                        break;
                    }
                    s += (t - last_tick) as f64 / ppq * us / 1e6;
                    last_tick = t;
                    us = tempo as f64;
                }
                s + (tick - last_tick) as f64 / ppq * us / 1e6
            })
        }
        Timing::Timecode(fps, sub) => {
            let rate = fps.as_f32() as f64 * sub.max(1) as f64;
            Box::new(move |tick: u64| tick as f64 / rate)
        }
    };
    let mut out = Vec::new();
    for tr in &smf.tracks {
        let mut tick = 0u64;
        let mut open: Vec<(u8, u8, u64)> = Vec::new(); // channel, key, start tick
        for ev in tr {
            tick += ev.delta.as_int() as u64;
            if let TrackEventKind::Midi { channel, message } = ev.kind {
                let ch = channel.as_int();
                match message {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                        open.push((ch, key.as_int(), tick));
                    }
                    MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                        if let Some(i) = open.iter().position(|o| o.0 == ch && o.1 == key.as_int()) {
                            let (_, k, st) = open.remove(i);
                            out.push(MelodyNote { start_s: to_s(st), end_s: to_s(tick), midi: k as f64 });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out.sort_by(|a, b| a.start_s.partial_cmp(&b.start_s).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_for(cents: &[f64]) -> Vec<PitchFrame> {
        cents
            .iter()
            .enumerate()
            .map(|(i, c)| PitchFrame {
                time_s: i as f64 * 0.01,
                freq_hz: ferraus_core::units::midi_to_hz(c / 100.0),
                confidence: 0.9,
                voiced: true,
                rms: 0.3,
            })
            .collect()
    }

    fn note(s: usize, e: usize, midi: f64) -> Note {
        Note {
            start_s: s as f64 * 0.01,
            end_s: e as f64 * 0.01,
            midi,
            confidence: 0.9,
            peak_rms: 0.3,
            start_frame: s,
            end_frame: e,
        }
    }

    #[test]
    fn instant_full_strength_lands_on_target() {
        let fr = frames_for(&vec![6040.0; 50]);
        let notes = vec![note(0, 50, 60.4)];
        let plans = plan_targets(&notes, &Target::Scale(vec![0, 2, 4, 5, 7, 9, 11]));
        assert_eq!(plans[0].target_midi, Some(60.0));
        let o = TuneOptions { retune_s: 0.0, vibrato_preserve: false, ..Default::default() };
        let c = correction_curve(&fr, &notes, &plans, &o);
        assert!(c.iter().all(|v| (v + 40.0).abs() < 1e-9));
        let o = TuneOptions { retune_s: 0.025, strength: 0.5, vibrato_preserve: false, ..Default::default() };
        let c = correction_curve(&fr, &notes, &plans, &o);
        assert!(c[0].abs() < 20.0 && (c[49] + 20.0).abs() < 0.01);
    }

    #[test]
    fn vibrato_is_kept_out_of_the_correction() {
        let cents: Vec<f64> =
            (0..100).map(|i| 6040.0 + 40.0 * (2.0 * std::f64::consts::PI * 6.0 * i as f64 * 0.01).sin()).collect();
        let fr = frames_for(&cents);
        let notes = vec![note(0, 100, 60.4)];
        let plans = plan_targets(&notes, &Target::Scale(vec![0, 2, 4, 5, 7, 9, 11]));
        let c = correction_curve(&fr, &notes, &plans, &TuneOptions::default());
        // corrected pitch keeps the vibrato around 6000
        let out: Vec<f64> = cents.iter().zip(&c).map(|(p, c)| p + c).collect();
        let mid = &out[30..70];
        let mean = mid.iter().sum::<f64>() / mid.len() as f64;
        let amp = mid.iter().map(|v| (v - mean).abs()).fold(0.0, f64::max);
        assert!((mean - 6000.0).abs() < 3.0, "mean {mean}");
        assert!((amp - 40.0).abs() < 8.0, "vibrato amplitude {amp}");
    }

    #[test]
    fn humanize_is_bounded_and_seeded() {
        let fr = frames_for(&vec![6000.0; 80]);
        let notes = vec![note(0, 80, 60.0)];
        let plans = plan_targets(&notes, &Target::Scale(vec![0]));
        let o = TuneOptions { humanize: 1.0, seed: 9, ..Default::default() };
        let a = correction_curve(&fr, &notes, &plans, &o);
        let b = correction_curve(&fr, &notes, &plans, &o);
        assert_eq!(a, b);
        assert!(a.iter().all(|v| v.abs() <= 12.0 + 1e-9));
        assert!(a.iter().any(|v| v.abs() > 0.5));
    }

    #[test]
    fn melody_target_and_tolerance() {
        let notes = vec![note(0, 30, 60.3), note(30, 60, 64.0)];
        let mel = vec![
            MelodyNote { start_s: 0.0, end_s: 0.3, midi: 60.0 },
            MelodyNote { start_s: 0.3, end_s: 0.6, midi: 62.0 },
        ];
        let plans = plan_targets(&notes, &Target::Melody { notes: mel, tolerance_cents: 150.0 });
        assert_eq!(plans[0].target_midi, Some(60.0));
        assert_eq!(plans[1].target_midi, None);
        assert!(plans[1].reason.as_deref().unwrap().contains("200 cents"));
    }

    #[test]
    fn midi_file_round_trip() {
        use midly::num::{u15, u24, u28, u4, u7};
        use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
        let ev = |delta: u32, kind| TrackEvent { delta: u28::new(delta), kind };
        let on = |k: u8| TrackEventKind::Midi {
            channel: u4::new(0),
            message: MidiMessage::NoteOn { key: u7::new(k), vel: u7::new(100) },
        };
        let off = |k: u8| TrackEventKind::Midi {
            channel: u4::new(0),
            message: MidiMessage::NoteOff { key: u7::new(k), vel: u7::new(0) },
        };
        let track = vec![
            ev(0, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(1_000_000)))), // 60 bpm
            ev(0, on(60)),
            ev(480, off(60)),
            ev(0, on(62)),
            ev(240, off(62)),
            ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];
        let smf =
            Smf { header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))), tracks: vec![track] };
        let mut bytes = Vec::new();
        smf.write(&mut bytes).unwrap();
        let mel = parse_midi_melody(&bytes).unwrap();
        assert_eq!(mel.len(), 2);
        assert!((mel[0].end_s - 1.0).abs() < 1e-9);
        assert!((mel[1].start_s - 1.0).abs() < 1e-9 && (mel[1].end_s - 1.5).abs() < 1e-9);
        assert_eq!(mel[1].midi, 62.0);
        assert!(parse_midi_melody(b"not midi").is_err());
    }
}
