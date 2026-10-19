//! Pitch spelling, keys, key estimation, and note-value arithmetic.

use crate::model::{KeySig, Mode, Pitch, Spelled};

pub fn step_pc(step: char) -> i32 {
    match step.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => 0,
    }
}

/// Letter steps in order.
pub const STEPS: [char; 7] = ['C', 'D', 'E', 'F', 'G', 'A', 'B'];

pub fn step_index(step: char) -> i32 {
    STEPS
        .iter()
        .position(|s| *s == step.to_ascii_uppercase())
        .unwrap_or(0) as i32
}

/// Line-of-fifths position → spelled pitch class. 0 = C, 1 = G, -1 = F, 7 = C#.
pub fn lof_spelled(q: i32) -> Spelled {
    // F C G D A E B at q = -1..=5
    const ORDER: [char; 7] = ['F', 'C', 'G', 'D', 'A', 'E', 'B'];
    let idx = (q + 1).rem_euclid(7) as usize;
    let alter = (q + 1).div_euclid(7) as i8;
    Spelled {
        step: ORDER[idx],
        alter,
    }
}

/// Line-of-fifths position of a spelled pitch class.
pub fn spelled_lof(s: Spelled) -> i32 {
    let base = match s.step {
        'F' => -1,
        'C' => 0,
        'G' => 1,
        'D' => 2,
        'A' => 3,
        'E' => 4,
        'B' => 5,
        _ => 0,
    };
    base + 7 * s.alter as i32
}

/// Spell a pitch class for a key. Diatonic notes take the key's spelling; chromatic
/// notes take the spelling closest to the key on the line of fifths (raised leading
/// tones in minor, flat sixth/seventh degrees in flat keys).
pub fn spell_pc(pc: i32, key: KeySig) -> Spelled {
    let pc = pc.rem_euclid(12);
    let k = key.fifths as i32;
    let (center, prefer_sharp) = match key.mode {
        Mode::Major => (k + 2, k > 0),
        // minor: relative-major fifths; leading tones are sharp-side
        Mode::Minor => (k + 3, true),
    };
    // candidate q values with q*7 ≡ pc (mod 12): q0 = pc*7 mod 12 ∈ 0..12
    let q0 = (pc * 7).rem_euclid(12);
    let mut best: Option<(i32, i32)> = None; // (distance, q)
    for q in [q0 - 24, q0 - 12, q0, q0 + 12] {
        let d = (q - center).abs();
        let better = match best {
            None => true,
            Some((bd, bq)) => d < bd || (d == bd && ((prefer_sharp && q > bq) || (!prefer_sharp && q < bq))),
        };
        if better {
            best = Some((d, q));
        }
    }
    lof_spelled(best.unwrap().1)
}

/// Spell a MIDI note for a key.
pub fn spell_midi(midi: i32, key: KeySig) -> Pitch {
    let s = spell_pc(midi, key);
    // octave from the letter: B#3 sounds as C4 etc.
    let letter_pc = step_pc(s.step);
    let octave = (midi - letter_pc - s.alter as i32).div_euclid(12) - 1;
    Pitch {
        step: s.step,
        alter: s.alter,
        octave: octave as i8,
    }
}

/// Tonic of a key, spelled.
pub fn key_tonic(key: KeySig) -> Spelled {
    let q = key.fifths as i32 + if key.mode == Mode::Minor { 3 } else { 0 };
    lof_spelled(q)
}

/// `G major`, `E minor`.
pub fn key_name(key: KeySig) -> String {
    format!(
        "{} {}",
        key_tonic(key).name(),
        if key.mode == Mode::Minor { "minor" } else { "major" }
    )
}

/// Parse `G`, `Gmaj`, `G major`, `Em`, `E minor`, `Bb`, `F#m`.
pub fn parse_key(s: &str) -> Option<KeySig> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let (root, minor) = if let Some(r) = lower
        .strip_suffix(" minor")
        .or_else(|| lower.strip_suffix("minor"))
        .or_else(|| lower.strip_suffix(" min"))
        .or_else(|| lower.strip_suffix("min"))
    {
        (&t[..r.len()], true)
    } else if let Some(r) = lower
        .strip_suffix(" major")
        .or_else(|| lower.strip_suffix("major"))
        .or_else(|| lower.strip_suffix(" maj"))
        .or_else(|| lower.strip_suffix("maj"))
    {
        (&t[..r.len()], false)
    } else if t.len() > 1 && t.ends_with('m') {
        (&t[..t.len() - 1], true)
    } else {
        (t, false)
    };
    let root = root.trim();
    let mut chars = root.chars();
    let step = chars.next()?.to_ascii_uppercase();
    if !('A'..='G').contains(&step) {
        return None;
    }
    let mut alter = 0i8;
    for c in chars {
        match c {
            '#' | '♯' => alter += 1,
            'b' | '♭' => alter -= 1,
            _ => return None,
        }
    }
    let q = spelled_lof(Spelled { step, alter });
    let fifths = if minor { q - 3 } else { q };
    if !(-7..=7).contains(&fifths) {
        return None;
    }
    Some(KeySig {
        fifths: fifths as i8,
        mode: if minor { Mode::Minor } else { Mode::Major },
    })
}

/// Key signature with tonic pitch class `pc` and mode, choosing the spelling with the
/// fewest accidentals (ties → flats for major, sharps for minor... resolved by `prefer`).
pub fn key_from_pc(pc: i32, mode: Mode, prefer_sharps: Option<bool>) -> KeySig {
    let pc = pc.rem_euclid(12);
    let offset = if mode == Mode::Minor { 3 } else { 0 };
    let mut cands: Vec<i32> = Vec::new();
    for q in -12i32..=12 {
        if (q * 7).rem_euclid(12) == pc {
            let f = q - offset;
            if (-7..=7).contains(&f) {
                cands.push(f);
            }
        }
    }
    cands.sort_by_key(|f| {
        let tie = match prefer_sharps {
            Some(true) => {
                if *f >= 0 {
                    0
                } else {
                    1
                }
            }
            Some(false) => {
                if *f <= 0 {
                    0
                } else {
                    1
                }
            }
            None => {
                if *f <= 0 {
                    0
                } else {
                    1
                }
            }
        };
        (f.abs(), tie)
    });
    KeySig {
        fifths: cands.first().copied().unwrap_or(0) as i8,
        mode,
    }
}

pub fn key_tonic_pc(key: KeySig) -> i32 {
    key_tonic(key).pc()
}

/// Transpose a key signature by semitones.
pub fn transpose_key(key: KeySig, semis: i32) -> KeySig {
    if semis.rem_euclid(12) == 0 {
        return key;
    }
    let pc = key_tonic_pc(key) + semis;
    key_from_pc(pc, key.mode, Some(key.fifths > 0))
}

/// Krumhansl-Kessler key profiles.
const KK_MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const KK_MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

fn pearson(a: &[f64; 12], b: &[f64]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
    for i in 0..12 {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// Estimate a key from a duration-weighted pitch-class histogram (Krumhansl-Schmuckler).
/// Returns the key and a confidence 0..1 (correlation margin).
pub fn estimate_key(hist: &[f64; 12]) -> (KeySig, f64) {
    if hist.iter().sum::<f64>() <= 0.0 {
        return (KeySig::major(0), 0.0);
    }
    let mut scores: Vec<(f64, i32, Mode)> = Vec::new();
    for tonic in 0..12 {
        let rot: Vec<f64> = (0..12).map(|i| hist[(i + tonic) % 12]).collect();
        scores.push((pearson(&KK_MAJOR, &rot), tonic as i32, Mode::Major));
        scores.push((pearson(&KK_MINOR, &rot), tonic as i32, Mode::Minor));
    }
    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let (best, tonic, mode) = scores[0];
    let second = scores.get(1).map(|s| s.0).unwrap_or(0.0);
    let conf = ((best - second) * 4.0 + best * 0.5).clamp(0.0, 1.0);
    (key_from_pc(tonic, mode, None), conf)
}

// ------------------------------------------------------------------------------------
// Note values
// ------------------------------------------------------------------------------------

/// MusicXML note type names from whole (index 0) to 256th.
pub const TYPES: [&str; 9] = [
    "whole", "half", "quarter", "eighth", "16th", "32nd", "64th", "128th", "256th",
];

/// Written type and dots for a duration (in divisions) under a tuplet ratio. Returns
/// `None` when the value is not expressible as a single (dotted) note.
pub fn type_for(duration: u32, divisions: u32, tuplet: Option<(u32, u32)>) -> Option<(&'static str, u8)> {
    if duration == 0 {
        return None;
    }
    // displayed duration in units of 1/(divisions*actual) quarter
    let (act, norm) = tuplet.unwrap_or((1, 1));
    // displayed = duration * actual / normal (in divisions)
    let num = duration as u64 * act as u64;
    let den = norm as u64;
    // whole note = 4*divisions
    for (i, t) in TYPES.iter().enumerate() {
        // base = 4*div / 2^i
        for dots in 0..=3u8 {
            // dotted length = base * (2 - 1/2^dots) = base * (2^(dots+1)-1)/2^dots
            let base_num = 4 * divisions as u64 * ((1u64 << (dots + 1)) - 1);
            let base_den = (1u64 << i) * (1u64 << dots);
            // compare num/den == base_num/base_den
            if num * base_den == base_num * den {
                return Some((t, dots));
            }
        }
    }
    None
}

/// Duration in divisions of a written type with dots.
pub fn duration_of(note_type: &str, dots: u8, divisions: u32, tuplet: Option<(u32, u32)>) -> Option<u32> {
    let i = TYPES.iter().position(|t| *t == note_type)? as u32;
    let base_num = 4 * divisions as u64 * ((1u64 << (dots + 1)) - 1);
    let base_den = (1u64 << i) * (1u64 << dots);
    let (act, norm) = tuplet.unwrap_or((1, 1));
    let num = base_num * norm as u64;
    let den = base_den * act as u64;
    if num % den != 0 {
        return None;
    }
    Some((num / den) as u32)
}

pub fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

pub fn lcm(a: u64, b: u64) -> u64 {
    if a == 0 || b == 0 {
        a.max(b)
    } else {
        a / gcd(a, b) * b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(key: KeySig, pcs: &[i32]) -> Vec<String> {
        pcs.iter().map(|p| spell_pc(*p, key).name()).collect()
    }

    #[test]
    fn spelling_in_keys() {
        // G major: F#
        assert_eq!(names(KeySig::major(1), &[6]), vec!["F#"]);
        // D major: F# C#
        assert_eq!(names(KeySig::major(2), &[6, 1]), vec!["F#", "C#"]);
        // Bb major: Bb Eb
        assert_eq!(names(KeySig::major(-2), &[10, 3]), vec!["Bb", "Eb"]);
        // Db major: Db Eb Gb Ab Bb
        assert_eq!(
            names(KeySig::major(-5), &[1, 3, 6, 8, 10]),
            vec!["Db", "Eb", "Gb", "Ab", "Bb"]
        );
        // E minor: F#, raised leading tone D#
        assert_eq!(names(KeySig::minor(1), &[6, 3]), vec!["F#", "D#"]);
        // A minor: G#
        assert_eq!(names(KeySig::minor(0), &[8]), vec!["G#"]);
        // C minor: Eb Ab Bb, raised B natural
        assert_eq!(names(KeySig::minor(-3), &[3, 8, 10, 11]), vec!["Eb", "Ab", "Bb", "B"]);
        // F# major: E#
        assert_eq!(names(KeySig::major(6), &[5]), vec!["E#"]);
        // Gb major: Cb
        assert_eq!(names(KeySig::major(-6), &[11]), vec!["Cb"]);
    }

    #[test]
    fn spelled_octaves() {
        let p = spell_midi(71, KeySig::major(-6)); // Cb5 sounds as B4
        assert_eq!((p.step, p.alter, p.octave), ('C', -1, 5));
        assert_eq!(p.midi(), 71);
        let p = spell_midi(60, KeySig::major(7)); // B#3
        assert_eq!((p.step, p.alter, p.octave), ('B', 1, 3));
        assert_eq!(p.midi(), 60);
    }

    #[test]
    fn keys_parse_and_name() {
        assert_eq!(parse_key("G"), Some(KeySig::major(1)));
        assert_eq!(parse_key("Em"), Some(KeySig::minor(1)));
        assert_eq!(parse_key("Bb major"), Some(KeySig::major(-2)));
        assert_eq!(parse_key("c# minor"), Some(KeySig::minor(4)));
        assert_eq!(key_name(KeySig::major(-5)), "Db major");
        assert_eq!(key_name(KeySig::minor(1)), "E minor");
        assert_eq!(transpose_key(KeySig::major(0), 2), KeySig::major(2));
        assert_eq!(transpose_key(KeySig::major(0), 1), KeySig::major(-5));
    }

    #[test]
    fn note_types() {
        assert_eq!(type_for(48, 48, None), Some(("quarter", 0)));
        assert_eq!(type_for(72, 48, None), Some(("quarter", 1)));
        assert_eq!(type_for(16, 48, Some((3, 2))), Some(("eighth", 0)));
        assert_eq!(type_for(60, 48, None), None);
        assert_eq!(duration_of("eighth", 0, 48, Some((3, 2))), Some(16));
        assert_eq!(duration_of("half", 1, 48, None), Some(144));
    }

    #[test]
    fn krumhansl_finds_keys() {
        // G major scale weighted toward tonic triad
        let mut h = [0.0; 12];
        for (pc, w) in [(7, 3.0), (9, 1.0), (11, 2.0), (0, 1.0), (2, 2.0), (4, 1.0), (6, 1.0)] {
            h[pc] = w;
        }
        assert_eq!(estimate_key(&h).0, KeySig::major(1));
    }
}
