//! Chord recognition (best-effort): peak-picked STFT chroma, harmonic-aware chord
//! templates (maj, min, 7, maj7, min7, dim, sus4, sus2), a bass-note cue for root choice
//! and slash chords, and Viterbi smoothing with a chord-change penalty.

use crate::util::{self, next_pow2};
use ferraus_core::AudioBuffer;
use serde::Serialize;

/// Pitch-class names used in chord labels.
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

/// Chord qualities: (label suffix, quality name, intervals from the root).
pub const QUALITIES: [(&str, &str, &[usize]); 8] = [
    ("", "maj", &[0, 4, 7]),
    ("m", "min", &[0, 3, 7]),
    ("7", "7", &[0, 4, 7, 10]),
    ("maj7", "maj7", &[0, 4, 7, 11]),
    ("m7", "min7", &[0, 3, 7, 10]),
    ("dim", "dim", &[0, 3, 6]),
    ("sus4", "sus4", &[0, 5, 7]),
    ("sus2", "sus2", &[0, 2, 7]),
];

/// Options for [`detect_chords`].
#[derive(Clone, Debug)]
pub struct ChordOptions {
    /// Cost of changing chord between frames (higher = smoother).
    pub change_penalty: f64,
    /// Segments shorter than this are merged into a neighbour (seconds).
    pub min_duration_s: f64,
}

impl Default for ChordOptions {
    fn default() -> Self {
        ChordOptions {
            change_penalty: 0.5,
            min_duration_s: 0.25,
        }
    }
}

/// A chromagram: 12 pitch-class energies per frame plus the lowest prominent pitch class.
#[derive(Clone, Debug)]
pub struct Chromagram {
    pub hop_s: f64,
    /// Per frame, 12 values normalised to unit L2 norm (zeros for silent frames).
    pub chroma: Vec<[f64; 12]>,
    /// Per frame, the pitch class of the lowest prominent spectral peak (bass note).
    pub bass: Vec<Option<usize>>,
    /// Per frame, energy relative to the loudest frame (dB).
    pub level_db: Vec<f64>,
}

/// One chord segment.
#[derive(Clone, Debug, Serialize)]
pub struct ChordSegment {
    pub start_s: f64,
    pub end_s: f64,
    /// Label such as `C`, `F#m7`, `Bbmaj7`, `G/B`, or `N` (no chord).
    pub chord: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Bass note when it differs from the root (slash chord).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bass: Option<String>,
    /// 0..1 mean template match of the chosen chord.
    pub confidence: f64,
}

/// Result of [`detect_chords`].
#[derive(Clone, Debug, Serialize)]
pub struct ChordResult {
    pub best_effort: bool,
    pub segments: Vec<ChordSegment>,
    /// Number of distinct chords (excluding `N`).
    pub distinct_chords: usize,
}

/// Compute a peak-picked chromagram (55 Hz – 5 kHz) of the mono mix.
pub fn chromagram(buf: &AudioBuffer) -> Chromagram {
    let sr = buf.sample_rate as f64;
    let x = buf.mono_mix();
    let n_fft = next_pow2((sr * 0.35) as usize).max(1024);
    let hop = n_fft / 4;
    let hop_s = hop as f64 / sr;
    if x.len() < n_fft / 4 {
        return Chromagram {
            hop_s,
            chroma: Vec::new(),
            bass: Vec::new(),
            level_db: Vec::new(),
        };
    }
    let frames = util::stft_mag(&x, n_fft, hop);
    let df = sr / n_fft as f64;
    let k_lo = (50.0 / df).floor().max(1.0) as usize;
    let k_hi = ((5000.0f64.min(sr / 2.0 - df)) / df) as usize;
    let energies: Vec<f64> = frames
        .iter()
        .map(|f| f[k_lo..=k_hi].iter().map(|m| (*m as f64).powi(2)).sum())
        .collect();
    let emax = energies.iter().copied().fold(0.0, f64::max);
    let mut chroma = Vec::with_capacity(frames.len());
    let mut bass = Vec::with_capacity(frames.len());
    let mut level_db = Vec::with_capacity(frames.len());
    for (fi, f) in frames.iter().enumerate() {
        let lvl = if emax > 0.0 {
            util::pow_db(energies[fi] / emax)
        } else {
            -240.0
        };
        level_db.push(lvl);
        let fmax = f[k_lo..=k_hi].iter().fold(0.0f32, |m, v| m.max(*v));
        let mut c = [0.0f64; 12];
        let mut lowest: Option<(f64, usize)> = None;
        if fmax > 0.0 && lvl > -60.0 {
            for k in k_lo.max(1)..k_hi {
                let m = f[k];
                if m <= f[k - 1] || m < f[k + 1] || m < fmax * 0.01 {
                    continue;
                }
                // parabolic interpolation on log magnitude
                let (a, b, g) = (
                    (f[k - 1].max(1e-12) as f64).ln(),
                    (m as f64).ln(),
                    (f[k + 1].max(1e-12) as f64).ln(),
                );
                let den = a - 2.0 * b + g;
                let delta = if den.abs() > 1e-12 {
                    (0.5 * (a - g) / den).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                let freq = (k as f64 + delta) * df;
                if !(55.0..=5000.0).contains(&freq) {
                    continue;
                }
                let midi = 69.0 + 12.0 * (freq / 440.0).log2();
                let near = midi.round();
                let dev = (midi - near).abs();
                let pc = (near as i64).rem_euclid(12) as usize;
                let w = m as f64 * (1.0 - dev);
                c[pc] += w;
                if m >= fmax * 0.2 && freq < 400.0 && lowest.map(|l| freq < l.0).unwrap_or(true) {
                    lowest = Some((freq, pc));
                }
            }
        }
        let norm = c.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm > 0.0 {
            c.iter_mut().for_each(|v| *v /= norm);
        }
        chroma.push(c);
        bass.push(lowest.map(|l| l.1));
    }
    Chromagram {
        hop_s,
        chroma,
        bass,
        level_db,
    }
}

/// Chord template for a root and quality, including the first harmonics of each note
/// (octaves, the fifth from the 3rd harmonic, the third from the 5th), L2-normalised.
pub fn chord_template(root: usize, quality: usize) -> [f64; 12] {
    let mut t = [0.0f64; 12];
    // harmonic number → (semitone offset, weight)
    let harmonics = [(0usize, 1.0), (12, 0.6), (19, 0.36), (24, 0.22), (28, 0.13)];
    for iv in QUALITIES[quality].2 {
        for (off, w) in harmonics {
            t[(root + iv + off) % 12] += w;
        }
    }
    let n = t.iter().map(|v| v * v).sum::<f64>().sqrt();
    t.iter_mut().for_each(|v| *v /= n);
    t
}

fn label(root: usize, quality: usize) -> String {
    format!("{}{}", NOTE_NAMES[root], QUALITIES[quality].0)
}

/// Recognise chords over time. Best-effort: suited to clear harmonic material.
pub fn detect_chords(buf: &AudioBuffer, opts: &ChordOptions) -> ChordResult {
    let cg = chromagram(buf);
    chords_from_chromagram(&cg, opts)
}

/// Chord recognition from a precomputed chromagram.
pub fn chords_from_chromagram(cg: &Chromagram, opts: &ChordOptions) -> ChordResult {
    let nq = QUALITIES.len();
    let ns = 12 * nq + 1; // last state = N
    let n_state = ns - 1;
    let templates: Vec<[f64; 12]> = (0..12 * nq)
        .map(|s| chord_template(s / nq, s % nq))
        .collect();
    let t = cg.chroma.len();
    if t == 0 {
        return ChordResult {
            best_effort: true,
            segments: Vec::new(),
            distinct_chords: 0,
        };
    }
    // emissions
    let emis: Vec<Vec<f64>> = (0..t)
        .map(|i| {
            let silent = cg.level_db[i] < -45.0 || cg.chroma[i].iter().all(|v| *v == 0.0);
            let mut e = vec![0.0f64; ns];
            if silent {
                e[n_state] = 1.0;
                return e;
            }
            for s in 0..12 * nq {
                let sim: f64 = templates[s].iter().zip(&cg.chroma[i]).map(|(a, b)| a * b).sum();
                let bonus = if cg.bass[i] == Some(s / nq) { 0.04 } else { 0.0 };
                e[s] = sim + bonus;
            }
            e[n_state] = 0.2;
            e
        })
        .collect();
    // Viterbi with a uniform change penalty
    let pen = opts.change_penalty;
    let mut score = emis[0].clone();
    let mut back = vec![vec![0usize; ns]; t];
    for i in 1..t {
        let (best_prev, best_val) = score
            .iter()
            .enumerate()
            .fold((0usize, f64::NEG_INFINITY), |m, (s, v)| if *v > m.1 { (s, *v) } else { m });
        let mut next = vec![0.0f64; ns];
        for s in 0..ns {
            let stay = score[s];
            let switch = best_val - pen;
            if stay >= switch {
                next[s] = stay + emis[i][s];
                back[i][s] = s;
            } else {
                next[s] = switch + emis[i][s];
                back[i][s] = best_prev;
            }
        }
        score = next;
    }
    let mut path = vec![0usize; t];
    path[t - 1] = score
        .iter()
        .enumerate()
        .fold((0usize, f64::NEG_INFINITY), |m, (s, v)| if *v > m.1 { (s, *v) } else { m })
        .0;
    for i in (1..t).rev() {
        path[i - 1] = back[i][path[i]];
    }
    // segments
    let mut raw: Vec<(usize, usize, usize)> = Vec::new(); // (state, start frame, end frame excl)
    let mut s0 = 0;
    for i in 1..=t {
        if i == t || path[i] != path[s0] {
            raw.push((path[s0], s0, i));
            s0 = i;
        }
    }
    // merge very short segments into the previous one
    let min_frames = (opts.min_duration_s / cg.hop_s).ceil() as usize;
    let mut merged: Vec<(usize, usize, usize)> = Vec::new();
    for seg in raw {
        if seg.2 - seg.1 < min_frames {
            if let Some(last) = merged.last_mut() {
                last.2 = seg.2;
                continue;
            }
        }
        if let Some(last) = merged.last_mut() {
            if last.0 == seg.0 {
                last.2 = seg.2;
                continue;
            }
        }
        merged.push(seg);
    }
    let mut distinct = std::collections::BTreeSet::new();
    let segments: Vec<ChordSegment> = merged
        .into_iter()
        .map(|(s, a, b)| {
            // frame i is centred at i*hop; segment spans half a hop either side
            let start = ((a as f64 - 0.5).max(0.0)) * cg.hop_s;
            let end = (b as f64 - 0.5) * cg.hop_s;
            let conf = util::mean(&(a..b).map(|i| emis[i][s].min(1.0)).collect::<Vec<_>>());
            if s == n_state {
                return ChordSegment {
                    start_s: start,
                    end_s: end,
                    chord: "N".into(),
                    root: None,
                    quality: None,
                    bass: None,
                    confidence: conf,
                };
            }
            let (root, q) = (s / nq, s % nq);
            distinct.insert(s);
            // bass vote over the segment
            let mut votes = [0usize; 12];
            let mut total = 0usize;
            for i in a..b {
                if let Some(p) = cg.bass[i] {
                    votes[p] += 1;
                    total += 1;
                }
            }
            let (bp, bv) = votes
                .iter()
                .enumerate()
                .fold((0usize, 0usize), |m, (p, v)| if *v > m.1 { (p, *v) } else { m });
            let in_chord = QUALITIES[q].2.iter().any(|iv| (root + iv) % 12 == bp);
            let slash = total > 0
                && bp != root
                && bv as f64 >= 0.6 * total as f64
                && (in_chord || bv as f64 >= 0.8 * total as f64);
            let mut chord = label(root, q);
            let bass = if slash {
                chord = format!("{chord}/{}", NOTE_NAMES[bp]);
                Some(NOTE_NAMES[bp].to_string())
            } else {
                None
            };
            ChordSegment {
                start_s: start,
                end_s: end,
                chord,
                root: Some(NOTE_NAMES[root].to_string()),
                quality: Some(QUALITIES[q].1.to_string()),
                bass,
                confidence: conf,
            }
        })
        .collect();
    ChordResult {
        best_effort: true,
        segments,
        distinct_chords: distinct.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tempo::tests::Rng;
    use std::f64::consts::PI;

    fn add_note(x: &mut [f32], sr: f64, start: usize, len: usize, midi: f64, amp: f64) {
        let f0 = 440.0 * 2f64.powf((midi - 69.0) / 12.0);
        for h in 1..=6 {
            let f = f0 * h as f64;
            if f > sr / 2.0 {
                break;
            }
            let a = amp / h as f64;
            for i in 0..len {
                let t = i as f64 / sr;
                let env = (t / 0.01).min(1.0) * ((len - i) as f64 / (0.01 * sr)).min(1.0);
                x[start + i] += (a * env * (2.0 * PI * f * t).sin()) as f32;
            }
        }
    }

    #[test]
    fn recognises_synthetic_triads_and_sevenths() {
        let sr = 22050.0;
        let dur = 1.0;
        let n_chords = 24;
        let mut rng = Rng::new(42);
        let mut x = vec![0.0f32; (sr * dur * n_chords as f64) as usize + 100];
        let mut truth = Vec::new();
        for c in 0..n_chords {
            let root = (rng.next() * 12.0) as usize % 12;
            let q = (rng.next() * 7.0) as usize % 7; // all but sus2 (ambiguous with sus4)
            let start = (c as f64 * dur * sr) as usize;
            let len = (dur * sr) as usize;
            for iv in QUALITIES[q].2 {
                add_note(&mut x, sr, start, len, 48.0 + (root + iv) as f64, 0.12);
            }
            truth.push((root, q));
        }
        let buf = AudioBuffer::from_mono(sr as u32, x);
        let r = detect_chords(&buf, &ChordOptions::default());
        let mut correct = 0;
        for (c, (root, q)) in truth.iter().enumerate() {
            let mid = (c as f64 + 0.5) * dur;
            let seg = r.segments.iter().find(|s| s.start_s <= mid && s.end_s > mid);
            let want = label(*root, *q);
            let got = seg.map(|s| s.chord.clone()).unwrap_or_default();
            if got == want {
                correct += 1;
            } else {
                eprintln!("chord {c}: want {want} got {got}");
            }
        }
        let acc = correct as f64 / n_chords as f64;
        assert!(acc >= 0.75, "chord accuracy {acc}");
    }

    #[test]
    fn slash_chord_and_silence() {
        let sr = 22050.0;
        let len = (2.0 * sr) as usize;
        let mut x = vec![0.0f32; len * 2];
        // C major, first inversion: E3 G3 C4
        for m in [52.0, 55.0, 60.0] {
            add_note(&mut x, sr, 0, len, m, 0.15);
        }
        let buf = AudioBuffer::from_mono(sr as u32, x);
        let r = detect_chords(&buf, &ChordOptions::default());
        assert_eq!(r.segments[0].chord, "C/E", "{:?}", r.segments);
        assert_eq!(r.segments.last().unwrap().chord, "N");
        let empty = detect_chords(&AudioBuffer::new(22050, 1, 100), &ChordOptions::default());
        assert!(empty.segments.is_empty());
    }
}
