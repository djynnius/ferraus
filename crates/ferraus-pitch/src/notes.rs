//! Note segmentation from a pitch track.
//!
//! A note starts where voicing starts or where the pitch jumps by more than
//! [`NoteOptions::jump_cents`] (80 cents) from the running note centre *and stays there*
//! for [`NoteOptions::stay_s`]. Notes are also split at energy dips whose depth exceeds a
//! threshold set by [`NoteOptions::sensitivity`] (repeated notes on the same pitch). Notes
//! shorter than [`NoteOptions::min_note_s`] (40 ms) merge into the neighbour closest in
//! pitch, or are dropped when isolated. Each note reports the median of its voiced frames
//! as a fractional MIDI number, its mean voicing confidence and its peak RMS.
//!
//! The same segmentation feeds `tune` (per-note correction) and `transcribe` (hum-to-MIDI).

use crate::pyin::PitchFrame;
use crate::util::median;
use ferraus_core::units::hz_to_midi;
use serde::Serialize;

/// Segmentation settings.
#[derive(Clone, Debug)]
pub struct NoteOptions {
    /// Minimum note length (seconds); shorter segments merge into a neighbour.
    pub min_note_s: f64,
    /// Pitch jump (cents) from the running note centre that starts a new note.
    pub jump_cents: f64,
    /// How long (seconds) a jump must persist to count as a new note.
    pub stay_s: f64,
    /// Energy-dip sensitivity 0..1: 0 splits only on very deep dips (15 dB), 1 on 3 dB dips.
    pub sensitivity: f64,
    /// Unvoiced gaps up to this long (seconds) inside a note are bridged.
    pub gap_fill_s: f64,
}

impl Default for NoteOptions {
    fn default() -> Self {
        NoteOptions {
            min_note_s: 0.040,
            jump_cents: 80.0,
            stay_s: 0.030,
            sensitivity: 0.5,
            gap_fill_s: 0.020,
        }
    }
}

/// One segmented note.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Note {
    /// Onset (seconds, frame centre of the first frame).
    pub start_s: f64,
    /// Offset (seconds, one hop after the last frame's centre).
    pub end_s: f64,
    /// Median pitch of the note's voiced frames as a fractional MIDI number.
    pub midi: f64,
    /// Mean voicing confidence of the note's voiced frames (0..1).
    pub confidence: f64,
    /// Peak frame RMS (linear).
    pub peak_rms: f64,
    /// First frame index (inclusive).
    pub start_frame: usize,
    /// Last frame index (exclusive).
    pub end_frame: usize,
}

impl Note {
    pub fn duration_s(&self) -> f64 {
        self.end_s - self.start_s
    }
    /// Note centre frequency in Hz.
    pub fn hz(&self) -> f64 {
        ferraus_core::units::midi_to_hz(self.midi)
    }
}

/// Hop (seconds) of a pitch track, from its frame times (10 ms if unknown).
pub fn track_hop(frames: &[PitchFrame]) -> f64 {
    if frames.len() >= 2 {
        (frames[1].time_s - frames[0].time_s).max(1e-6)
    } else {
        0.010
    }
}

fn frame_cents(f: &PitchFrame) -> f64 {
    hz_to_midi(f.freq_hz) * 100.0
}

/// Segment a pitch track into notes.
pub fn segment(frames: &[PitchFrame], opts: &NoteOptions) -> Vec<Note> {
    let hop = track_hop(frames);
    let n = frames.len();
    let voiced = |i: usize| frames[i].voiced && frames[i].freq_hz > 0.0;
    let gap = (opts.gap_fill_s / hop).round() as usize;
    let stay = ((opts.stay_s / hop).round() as usize).max(1);
    let min_len = ((opts.min_note_s / hop).round() as usize).max(1);

    // 1. voiced runs, bridging short gaps
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if !voiced(i) {
            i += 1;
            continue;
        }
        let s = i;
        let mut e = i + 1;
        loop {
            while e < n && voiced(e) {
                e += 1;
            }
            // bridge a short gap followed by voicing
            let mut g = e;
            while g < n && !voiced(g) && g - e < gap {
                g += 1;
            }
            if g < n && g > e && voiced(g) && g - e <= gap {
                e = g;
                continue;
            }
            break;
        }
        runs.push((s, e));
        i = e;
    }

    // 2. split runs on sustained pitch jumps
    let mut segs: Vec<(usize, usize)> = Vec::new();
    for (s, e) in runs {
        let mut cur = s;
        let mut window: Vec<f64> = Vec::new();
        let mut t = s;
        while t < e {
            if !voiced(t) {
                t += 1;
                continue;
            }
            let c = frame_cents(&frames[t]);
            if window.len() >= 2 {
                let tail = &window[window.len().saturating_sub(15)..];
                let r = median(tail).unwrap_or(c);
                if (c - r).abs() > opts.jump_cents && t > cur {
                    // does the new level persist?
                    let mut seen = 0;
                    let mut ok = true;
                    let mut u = t + 1;
                    while u < e && seen < stay {
                        if voiced(u) {
                            let cu = frame_cents(&frames[u]);
                            if (cu - r).abs() <= opts.jump_cents
                                || (cu - c).abs() > opts.jump_cents
                            {
                                ok = false;
                                break;
                            }
                            seen += 1;
                        }
                        u += 1;
                    }
                    if ok && seen >= stay.min(e - t - 1) {
                        segs.push((cur, t));
                        cur = t;
                        window.clear();
                    }
                }
            }
            window.push(c);
            t += 1;
        }
        segs.push((cur, e));
    }

    // 3. energy-dip splitting
    let dip_db = 15.0 - 12.0 * opts.sensitivity.clamp(0.0, 1.0);
    let look = (0.3 / hop).round() as usize;
    let mut split: Vec<(usize, usize)> = Vec::new();
    let mut stack: Vec<(usize, usize)> = segs.into_iter().rev().collect();
    while let Some((s, e)) = stack.pop() {
        if e - s < 2 * min_len + 1 {
            split.push((s, e));
            continue;
        }
        let db: Vec<f64> = (s..e)
            .map(|k| 20.0 * frames[k].rms.max(1e-9).log10())
            .collect();
        let mut best: Option<(usize, f64)> = None;
        for k in min_len..(e - s - min_len) {
            if !(db[k] <= db[k - 1] && db[k] <= db[k + 1]) {
                continue;
            }
            let before = db[k.saturating_sub(look)..k]
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            let after = db[k + 1..(k + 1 + look).min(db.len())]
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            let depth = before.min(after) - db[k];
            if depth >= dip_db && best.map(|b| depth > b.1).unwrap_or(true) {
                best = Some((k, depth));
            }
        }
        match best {
            Some((k, _)) => {
                // continue on both halves (left first)
                stack.push((s + k, e));
                stack.push((s, s + k));
            }
            None => split.push((s, e)),
        }
    }
    split.sort();

    // 4. merge or drop short segments
    let centre = |s: usize, e: usize| -> Option<f64> {
        let v: Vec<f64> = (s..e)
            .filter(|&k| voiced(k))
            .map(|k| hz_to_midi(frames[k].freq_hz))
            .collect();
        median(&v)
    };
    let mut segs = split;
    loop {
        let Some(idx) = segs.iter().position(|(s, e)| e - s < min_len) else {
            break;
        };
        let (s, e) = segs[idx];
        let me = centre(s, e);
        let left = (idx > 0 && segs[idx - 1].1 == s).then(|| idx - 1);
        let right = (idx + 1 < segs.len() && segs[idx + 1].0 == e).then_some(idx + 1);
        let dist = |j: usize| -> f64 {
            match (me, centre(segs[j].0, segs[j].1)) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => f64::INFINITY,
            }
        };
        let target = match (left, right) {
            (Some(l), Some(r)) => Some(if dist(l) <= dist(r) { l } else { r }),
            (Some(l), None) => Some(l),
            (None, Some(r)) => Some(r),
            (None, None) => None,
        };
        match target {
            Some(j) => {
                let (a, b) = (segs[j].0.min(s), segs[j].1.max(e));
                segs[j] = (a, b);
                segs.remove(idx);
            }
            None => {
                segs.remove(idx);
            }
        }
    }

    // 5. describe notes
    segs.into_iter()
        .filter_map(|(s, e)| {
            let vf: Vec<usize> = (s..e).filter(|&k| voiced(k)).collect();
            if vf.is_empty() {
                return None;
            }
            let mids: Vec<f64> = vf.iter().map(|&k| hz_to_midi(frames[k].freq_hz)).collect();
            let conf = vf.iter().map(|&k| frames[k].confidence).sum::<f64>() / vf.len() as f64;
            let peak = (s..e).map(|k| frames[k].rms).fold(0.0, f64::max);
            Some(Note {
                start_s: frames[s].time_s,
                end_s: frames[e - 1].time_s + hop,
                midi: median(&mids)?,
                confidence: conf,
                peak_rms: peak,
                start_frame: s,
                end_frame: e,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use crate::pyin::{track, PyinOptions};

    fn synth_frames(spec: &[(Option<f64>, usize, f64)]) -> Vec<PitchFrame> {
        // (midi or None, frames, rms)
        let mut out = Vec::new();
        for (m, len, rms) in spec {
            for _ in 0..*len {
                let t = out.len() as f64 * 0.01;
                out.push(PitchFrame {
                    time_s: t,
                    freq_hz: m.map(midi_hz).unwrap_or(0.0),
                    confidence: if m.is_some() { 0.9 } else { 0.0 },
                    voiced: m.is_some(),
                    rms: *rms,
                });
            }
        }
        out
    }

    #[test]
    fn splits_on_voicing_and_jumps() {
        let fr = synth_frames(&[
            (None, 10, 0.0),
            (Some(60.0), 30, 0.3),
            (Some(62.0), 30, 0.3),
            (None, 10, 0.0),
            (Some(64.4), 25, 0.3),
        ]);
        let notes = segment(&fr, &NoteOptions::default());
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!((notes[0].midi - 60.0).abs() < 1e-9);
        assert!((notes[1].midi - 62.0).abs() < 1e-9);
        assert!((notes[2].midi - 64.4).abs() < 1e-9);
        assert!((notes[1].start_s - 0.40).abs() < 1e-9);
        assert_eq!(notes[0].end_frame, notes[1].start_frame);
    }

    #[test]
    fn short_blips_and_vibrato_do_not_split() {
        // a 2-frame spike, then ±40 cent vibrato
        let mut spec = vec![(Some(60.0), 20, 0.3), (Some(61.5), 2, 0.3), (Some(60.0), 20, 0.3)];
        for k in 0..60 {
            let v = 0.4 * (2.0 * std::f64::consts::PI * 6.0 * k as f64 * 0.01).sin();
            spec.push((Some(60.0 + v), 1, 0.3));
        }
        let fr = synth_frames(&spec);
        let notes = segment(&fr, &NoteOptions::default());
        assert_eq!(notes.len(), 1, "{notes:?}");
    }

    #[test]
    fn energy_dip_splits_repeated_note() {
        let fr = synth_frames(&[
            (Some(67.0), 30, 0.3),
            (Some(67.0), 3, 0.02),
            (Some(67.0), 30, 0.3),
        ]);
        let notes = segment(&fr, &NoteOptions::default());
        assert_eq!(notes.len(), 2, "{notes:?}");
        // insensitive setting keeps one note when the dip is shallow
        let fr = synth_frames(&[
            (Some(67.0), 30, 0.3),
            (Some(67.0), 3, 0.12),
            (Some(67.0), 30, 0.3),
        ]);
        let o = NoteOptions {
            sensitivity: 0.0,
            ..Default::default()
        };
        assert_eq!(segment(&fr, &o).len(), 1);
    }

    #[test]
    fn min_note_length_and_empty() {
        let fr = synth_frames(&[(None, 5, 0.0), (Some(70.0), 3, 0.3), (None, 5, 0.0)]);
        assert!(segment(&fr, &NoteOptions::default()).is_empty());
        assert!(segment(&[], &NoteOptions::default()).is_empty());
    }

    #[test]
    fn segments_synthetic_vocal_melody() {
        let sr = 16000;
        let melody = [60.0, 62.0, 64.0, 65.0, 67.0, 67.0];
        let notes: Vec<(f64, f64)> = melody.iter().map(|m| (midi_hz(*m), 0.25)).collect();
        // the repeated G only splits with an energy dip; this fixture has none, so 5 notes
        let x = vocal(sr, &notes, Vowel::A, 0.0, 0.0);
        let fr = track(&x, sr, &PyinOptions::default());
        let got = segment(&fr, &NoteOptions::default());
        assert_eq!(got.len(), 5, "{got:?}");
        for (g, m) in got.iter().zip([60.0, 62.0, 64.0, 65.0, 67.0]) {
            assert!((g.midi - m).abs() < 0.05, "{g:?} vs {m}");
        }
    }
}
