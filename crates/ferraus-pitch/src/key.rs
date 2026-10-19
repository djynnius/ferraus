//! Key and scale detection, key/scale name parsing, and scale pitch sets.
//!
//! Detection builds a 12-bin chroma profile and correlates it (Pearson) against the 24
//! rotations of the Krumhansl-Schmuckler major and minor key profiles.
//!
//! * [`chroma_stft`]: chroma from an STFT. Spectral peaks are located with parabolic
//!   interpolation, a global tuning offset is estimated as the magnitude-weighted circular
//!   mean of the peaks' deviation from equal temperament, and each peak is mapped to its
//!   pitch class (log-frequency mapping relative to the tuning).
//! * [`chroma_from_notes`]: a duration-weighted pitch-class histogram of segmented notes
//!   (used by `tune` on monophonic vocals, where harmonics would otherwise blur the profile).
//!
//! Confidence is a normalised margin: `r1` is the best correlation and `r2` the best
//! correlation of a key whose scale differs from the winner's (the relative major/minor
//! has the same notes, so it is not a competitor for choosing a scale). The confidence is
//! `clamp(r1, 0, 1) · clamp((r1 − r2) / (0.5 · (1 − r2)), 0, 1)` raised to the power 0.5,
//! so an unambiguous diatonic profile scores near 1 and a flat or chromatic one near 0.

use crate::notes::Note;
use crate::util::Fft;
use ferraus_core::units::{hz_to_midi, parse_pitch_class};
use ferraus_core::{Error, Result};
use serde::Serialize;

const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

/// Major or minor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Major,
    Minor,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Major => "major",
            Mode::Minor => "minor",
        }
    }
}

/// A key: tonic pitch class (0 = C) and mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Key {
    pub tonic: i32,
    pub mode: Mode,
}

impl Key {
    /// Conventional spelling, e.g. `Bb major`, `F# minor`.
    pub fn name(&self) -> String {
        format!("{} {}", self.tonic_name(), self.mode.as_str())
    }

    pub fn tonic_name(&self) -> &'static str {
        const MAJ: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
        const MIN: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "G#", "A", "Bb", "B"];
        let pc = self.tonic.rem_euclid(12) as usize;
        match self.mode {
            Mode::Major => MAJ[pc],
            Mode::Minor => MIN[pc],
        }
    }

    /// Pitch classes of the key's diatonic scale (natural minor for minor keys).
    pub fn pitch_classes(&self) -> Vec<i32> {
        let steps: &[i32] = match self.mode {
            Mode::Major => &[0, 2, 4, 5, 7, 9, 11],
            Mode::Minor => &[0, 2, 3, 5, 7, 8, 10],
        };
        steps.iter().map(|s| (self.tonic + s).rem_euclid(12)).collect()
    }

    /// Parse `C`, `F#`, `Bb`, `Bbm`, `Bbmin`, `A minor`, `Ebmaj`, `c#` (lowercase alone
    /// still means major unless a minor suffix is present). Returns the tonic and the mode
    /// if one was stated.
    pub fn parse(s: &str) -> Result<(i32, Option<Mode>)> {
        let t = s.trim();
        let bad = || Error::usage(format!("'{s}' is not a key; use forms like C, F#, Bbm, 'A minor' or 'Eb major'"));
        if t.is_empty() {
            return Err(bad());
        }
        // split tonic letters from the rest
        let mut end = 1;
        let chars: Vec<char> = t.chars().collect();
        while end < chars.len() && matches!(chars[end], '#' | 'b' | '♯' | '♭') {
            // 'b' could start "b" in a suffix only if followed by letters like "bm"? A
            // second 'b' after the letter is always an accidental here.
            end += 1;
        }
        let tonic_str: String = chars[..end].iter().collect();
        let rest: String = chars[end..].iter().collect::<String>().trim().to_lowercase();
        let tonic = parse_pitch_class(&tonic_str).ok_or_else(bad)?;
        let mode = match rest.as_str() {
            "" => None,
            "m" | "min" | "minor" | "-" | "aeolian" => Some(Mode::Minor),
            "maj" | "major" | "M" | "ionian" => Some(Mode::Major),
            _ => return Err(bad()),
        };
        Ok((tonic, mode))
    }
}

/// One scored key.
#[derive(Clone, Debug, Serialize)]
pub struct KeyCandidate {
    pub key: String,
    pub tonic: String,
    pub mode: Mode,
    pub correlation: f64,
}

/// Result of key detection.
#[derive(Clone, Debug, Serialize)]
pub struct KeyEstimate {
    #[serde(skip)]
    pub best: Key,
    /// e.g. `G major`.
    pub key: String,
    pub tonic: String,
    pub mode: Mode,
    /// 0..1, see the module docs.
    pub confidence: f64,
    /// Pearson correlation of the winner.
    pub correlation: f64,
    /// The next three keys by correlation.
    pub alternatives: Vec<KeyCandidate>,
    /// Estimated tuning offset from A440 in cents (STFT method), 0 otherwise.
    pub tuning_cents: f64,
    /// Normalised chroma profile, C..B.
    pub chroma: [f64; 12],
}

fn pearson(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let x = a[i] - ma;
        let y = b[i] - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 1e-18 || db <= 1e-18 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// All 24 keys with their correlation, best first.
pub fn score_keys(chroma: &[f64; 12]) -> Vec<(Key, f64)> {
    let mut out = Vec::with_capacity(24);
    for (mode, prof) in [(Mode::Major, &MAJOR), (Mode::Minor, &MINOR)] {
        for tonic in 0..12 {
            let mut rot = [0.0; 12];
            for (i, r) in rot.iter_mut().enumerate() {
                *r = prof[(i + 12 - tonic as usize) % 12];
            }
            out.push((Key { tonic, mode }, pearson(chroma, &rot)));
        }
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn same_scale(a: &Key, b: &Key) -> bool {
    let mut x = a.pitch_classes();
    let mut y = b.pitch_classes();
    x.sort();
    y.sort();
    x == y
}

/// Detect the key from a chroma profile. `restrict` limits the winner to one mode.
pub fn detect_from_chroma(chroma: &[f64; 12], restrict: Option<Mode>) -> KeyEstimate {
    let total: f64 = chroma.iter().sum();
    let mut norm = *chroma;
    if total > 0.0 {
        norm.iter_mut().for_each(|v| *v /= total);
    }
    let scored = score_keys(&norm);
    let pool: Vec<&(Key, f64)> = scored.iter().filter(|(k, _)| restrict.map(|m| k.mode == m).unwrap_or(true)).collect();
    let (best, r1) = *pool[0];
    let r2 = scored.iter().filter(|(k, _)| !same_scale(k, &best)).map(|(_, r)| *r).next().unwrap_or(0.0);
    let margin = ((r1 - r2) / (0.5 * (1.0 - r2).max(1e-6))).clamp(0.0, 1.0);
    let confidence = if total > 0.0 { (r1.clamp(0.0, 1.0) * margin).sqrt() } else { 0.0 };
    let alternatives = scored
        .iter()
        .filter(|(k, _)| *k != best)
        .take(3)
        .map(|(k, r)| KeyCandidate {
            key: k.name(),
            tonic: k.tonic_name().to_string(),
            mode: k.mode,
            correlation: round3(*r),
        })
        .collect();
    KeyEstimate {
        best,
        key: best.name(),
        tonic: best.tonic_name().to_string(),
        mode: best.mode,
        confidence: round3(confidence),
        correlation: round3(r1),
        alternatives,
        tuning_cents: 0.0,
        chroma: norm.map(round3),
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Duration-weighted pitch-class histogram of notes (pitch rounded to the nearest semitone).
pub fn chroma_from_notes(notes: &[Note]) -> [f64; 12] {
    let mut c = [0.0; 12];
    for n in notes {
        let pc = (n.midi.round() as i64).rem_euclid(12) as usize;
        c[pc] += n.duration_s() * n.confidence.max(0.1);
    }
    c
}

/// Chroma from an STFT of a mono signal, with a tuning estimate (cents from A440).
pub fn chroma_stft(samples: &[f32], sr: u32) -> ([f64; 12], f64) {
    let srf = sr as f64;
    let n = ((0.186 * srf) as usize).next_power_of_two().max(256);
    let hop = n / 4;
    let win = crate::util::hann(n);
    let mut fft = Fft::new(n);
    let kmin = ((55.0 * n as f64 / srf).ceil() as usize).max(2);
    let kmax = ((5000.0f64.min(0.45 * srf) * n as f64 / srf) as usize).min(n / 2 - 2);
    let mut peaks: Vec<(f64, f64)> = Vec::new(); // (freq, weight)
    let mut start = 0usize;
    let mut mag = vec![0.0f64; n / 2 + 1];
    while start < samples.len().max(1) {
        for i in 0..n {
            let v = samples.get(start + i).copied().unwrap_or(0.0);
            fft.time[i] = v * win[i];
        }
        fft.forward();
        let mut mx = 0.0f64;
        for (m, c) in mag.iter_mut().zip(&fft.freq) {
            *m = c.norm() as f64;
            mx = mx.max(*m);
        }
        if mx > 1e-6 {
            let floor = mx * 1e-3; // -60 dB
            for k in kmin..=kmax {
                let m = mag[k];
                if m > floor && m > mag[k - 1] && m >= mag[k + 1] {
                    let (a, b, c) = (mag[k - 1].max(1e-12).ln(), m.ln(), mag[k + 1].max(1e-12).ln());
                    let den = a - 2.0 * b + c;
                    let off = if den.abs() > 1e-12 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
                    let f = (k as f64 + off) * srf / n as f64;
                    peaks.push((f, m / mx));
                }
            }
        }
        if start + n >= samples.len() {
            break;
        }
        start += hop;
    }
    // tuning: weighted circular mean of deviations from the semitone grid
    let (mut sx, mut sy) = (0.0, 0.0);
    for (f, w) in &peaks {
        let m = hz_to_midi(*f);
        let dev = m - m.round();
        let a = 2.0 * std::f64::consts::PI * dev;
        sx += w * a.cos();
        sy += w * a.sin();
    }
    let tuning = if sx == 0.0 && sy == 0.0 { 0.0 } else { sy.atan2(sx) / (2.0 * std::f64::consts::PI) };
    let mut chroma = [0.0; 12];
    for (f, w) in &peaks {
        let m = hz_to_midi(*f) - tuning;
        let pc = (m.round() as i64).rem_euclid(12) as usize;
        chroma[pc] += w;
    }
    (chroma, (tuning * 100.0 * 10.0).round() / 10.0)
}

/// Detect the key of a mono signal with STFT chroma.
pub fn detect(samples: &[f32], sr: u32) -> KeyEstimate {
    let (c, tuning) = chroma_stft(samples, sr);
    let mut k = detect_from_chroma(&c, None);
    k.tuning_cents = tuning;
    k
}

/// Pitch-class set for a scale description relative to `tonic`:
/// `major`, `minor`, `chromatic`, a mode name (`dorian`, `harmonic-minor`, `blues`, ...), or
/// a custom note list such as `C,D,Eb,G,A`.
pub fn scale_pitch_classes(scale: &str, tonic: i32) -> Result<Vec<i32>> {
    let steps = named_steps(scale);
    if let Some(st) = steps {
        return Ok(st.iter().map(|x| (tonic + x).rem_euclid(12)).collect());
    }
    let mut pcs = Vec::new();
    for part in scale.split([',', ' ']).filter(|p| !p.trim().is_empty()) {
        let pc = parse_pitch_class(part.trim()).ok_or_else(|| {
            Error::usage(format!(
                "'{scale}' is not a scale: use major, minor, chromatic, a mode name, or notes like \"C,D,Eb,G,A\""
            ))
        })?;
        if !pcs.contains(&pc) {
            pcs.push(pc);
        }
    }
    if pcs.is_empty() {
        return Err(Error::usage("the scale has no notes"));
    }
    pcs.sort();
    Ok(pcs)
}

fn named_steps(scale: &str) -> Option<&'static [i32]> {
    match scale.trim().to_lowercase().as_str() {
        "major" | "ionian" => Some(&[0, 2, 4, 5, 7, 9, 11]),
        "minor" | "aeolian" | "natural-minor" => Some(&[0, 2, 3, 5, 7, 8, 10]),
        "harmonic-minor" => Some(&[0, 2, 3, 5, 7, 8, 11]),
        "melodic-minor" => Some(&[0, 2, 3, 5, 7, 9, 11]),
        "dorian" => Some(&[0, 2, 3, 5, 7, 9, 10]),
        "phrygian" => Some(&[0, 1, 3, 5, 7, 8, 10]),
        "lydian" => Some(&[0, 2, 4, 6, 7, 9, 11]),
        "mixolydian" => Some(&[0, 2, 4, 5, 7, 9, 10]),
        "locrian" => Some(&[0, 1, 3, 5, 6, 8, 10]),
        "major-pentatonic" | "pentatonic" => Some(&[0, 2, 4, 7, 9]),
        "minor-pentatonic" => Some(&[0, 3, 5, 7, 10]),
        "blues" => Some(&[0, 3, 5, 6, 7, 10]),
        "chromatic" => Some(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
        _ => None,
    }
}

/// Is this a scale or mode name (as opposed to a custom note list)?
pub fn is_scale_name(scale: &str) -> bool {
    named_steps(scale).is_some()
}

/// `C,D,Eb,G,A`-style names for pitch classes (flats for black keys).
pub fn pitch_class_names(pcs: &[i32]) -> String {
    const N: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"];
    pcs.iter().map(|p| N[p.rem_euclid(12) as usize]).collect::<Vec<_>>().join(",")
}

/// Nearest MIDI note (integer) to `midi` whose pitch class is in `pcs`.
pub fn nearest_in_scale(midi: f64, pcs: &[i32]) -> f64 {
    let base = midi.round() as i64;
    let mut best = base as f64;
    let mut bd = f64::INFINITY;
    for d in -7..=7i64 {
        let m = base + d;
        if pcs.contains(&(m.rem_euclid(12) as i32)) {
            let dist = (m as f64 - midi).abs();
            if dist < bd {
                bd = dist;
                best = m as f64;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Chord tones (MIDI) as harmonic-rich tones, one chord per `dur` seconds.
    fn progression(sr: u32, chords: &[Vec<i32>], dur: f64) -> Vec<f32> {
        let n = (sr as f64 * dur) as usize;
        let mut out = Vec::new();
        for ch in chords {
            for i in 0..n {
                let t = i as f64 / sr as f64;
                let env = (1.0 - t / dur).max(0.0) * 0.8 + 0.2;
                let mut v = 0.0;
                for &m in ch {
                    let f = 440.0 * 2f64.powf((m as f64 - 69.0) / 12.0);
                    for h in 1..=4 {
                        v += (2.0 * PI * f * h as f64 * t).sin() / (h * h) as f64;
                    }
                }
                out.push((0.1 * v * env) as f32);
            }
        }
        out
    }

    fn triad(root: i32, minor: bool) -> Vec<i32> {
        vec![root, root + if minor { 3 } else { 4 }, root + 7]
    }

    #[test]
    fn detects_major_and_minor_progressions() {
        let sr = 11025;
        for tonic in [0, 7, 2, 10, 5] {
            let r = 48 + tonic;
            // I IV V I vi IV V I
            let chords = vec![
                triad(r, false),
                triad(r + 5, false),
                triad(r + 7, false),
                triad(r, false),
                triad(r + 9, true),
                triad(r + 5, false),
                triad(r + 7, false),
                triad(r, false),
            ];
            let x = progression(sr, &chords, 0.3);
            let k = detect(&x, sr);
            assert_eq!(k.best, Key { tonic, mode: Mode::Major }, "{k:?}");
            assert!(k.confidence >= 0.6, "{k:?}");
        }
        for tonic in [9, 4, 0, 2] {
            let r = 45 + (tonic + 3) % 12;
            // i iv V i VI iv V i (harmonic minor V)
            let chords = vec![
                triad(r, true),
                triad(r + 5, true),
                triad(r + 7, false),
                triad(r, true),
                triad(r + 8, false),
                triad(r + 5, true),
                triad(r + 7, false),
                triad(r, true),
            ];
            let x = progression(sr, &chords, 0.3);
            let k = detect(&x, sr);
            assert_eq!(k.best, Key { tonic: tonic % 12, mode: Mode::Minor }, "{k:?}");
            assert!(k.confidence >= 0.6, "{k:?}");
        }
    }

    #[test]
    fn detects_melody_keys_and_tuning() {
        let sr = 11025;
        // G major scale melody, 30 cents sharp
        let mel = [67, 69, 71, 72, 74, 72, 71, 69, 67, 74, 71, 67, 66, 67];
        let mut x = Vec::new();
        for m in mel {
            let f = 440.0 * 2f64.powf((m as f64 + 0.3 - 69.0) / 12.0);
            for i in 0..(0.25 * sr as f64) as usize {
                let t = i as f64 / sr as f64;
                x.push((0.3 * (2.0 * PI * f * t).sin()) as f32);
            }
        }
        let k = detect(&x, sr);
        assert_eq!(k.key, "G major", "{k:?}");
        assert!((k.tuning_cents - 30.0).abs() < 5.0, "{k:?}");
        assert_eq!(k.alternatives.len(), 3);
    }

    #[test]
    fn chromatic_material_has_low_confidence() {
        let c = [1.0; 12];
        assert!(detect_from_chroma(&c, None).confidence < 0.1);
        let silent = detect(&vec![0.0; 22050], 11025);
        assert_eq!(silent.confidence, 0.0);
        // chromatic scale melody
        let sr = 11025;
        let mut x = Vec::new();
        for m in 60..72 {
            let f = 440.0 * 2f64.powf((m as f64 - 69.0) / 12.0);
            for i in 0..(0.2 * sr as f64) as usize {
                x.push((0.3 * (2.0 * PI * f * i as f64 / sr as f64).sin()) as f32);
            }
        }
        assert!(detect(&x, sr).confidence < 0.6);
    }

    #[test]
    fn parse_keys_and_scales() {
        assert_eq!(Key::parse("C").unwrap(), (0, None));
        assert_eq!(Key::parse("F#").unwrap(), (6, None));
        assert_eq!(Key::parse("Bbm").unwrap(), (10, Some(Mode::Minor)));
        assert_eq!(Key::parse("A minor").unwrap(), (9, Some(Mode::Minor)));
        assert_eq!(Key::parse("Eb major").unwrap(), (3, Some(Mode::Major)));
        assert!(Key::parse("H").is_err());
        assert_eq!(scale_pitch_classes("major", 7).unwrap(), vec![7, 9, 11, 0, 2, 4, 6]);
        assert_eq!(scale_pitch_classes("C,D,Eb,G,A", 0).unwrap(), vec![0, 2, 3, 7, 9]);
        assert!(scale_pitch_classes("wobble", 0).is_err());
        assert_eq!(nearest_in_scale(61.4, &[0, 2, 4, 5, 7, 9, 11]), 62.0);
        assert_eq!(nearest_in_scale(60.4, &[0, 2, 4, 5, 7, 9, 11]), 60.0);
        assert_eq!(Key { tonic: 10, mode: Mode::Major }.name(), "Bb major");
        assert_eq!(Key { tonic: 6, mode: Mode::Minor }.name(), "F# minor");
    }
}
