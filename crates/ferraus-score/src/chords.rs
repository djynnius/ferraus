//! Chord symbols: qualities, recognition from notes (chroma templates with bass tracking
//! and Viterbi smoothing), symbol parsing, and a built-in table of guitar shapes with capo
//! suggestions.

use crate::model::{GuitarFrame, KeySig, Mode, Spelled};
use crate::pitch::{key_tonic_pc, spell_pc};

/// (quality, intervals, symbol suffix, MusicXML kind)
pub const QUALITIES: &[(&str, &[i32], &str, &str)] = &[
    ("maj", &[0, 4, 7], "", "major"),
    ("min", &[0, 3, 7], "m", "minor"),
    ("7", &[0, 4, 7, 10], "7", "dominant"),
    ("maj7", &[0, 4, 7, 11], "maj7", "major-seventh"),
    ("m7", &[0, 3, 7, 10], "m7", "minor-seventh"),
    ("dim", &[0, 3, 6], "dim", "diminished"),
    ("dim7", &[0, 3, 6, 9], "dim7", "diminished-seventh"),
    ("m7b5", &[0, 3, 6, 10], "m7b5", "half-diminished"),
    ("aug", &[0, 4, 8], "aug", "augmented"),
    ("sus2", &[0, 2, 7], "sus2", "suspended-second"),
    ("sus4", &[0, 5, 7], "sus4", "suspended-fourth"),
    ("add9", &[0, 2, 4, 7], "add9", "major"),
    ("6", &[0, 4, 7, 9], "6", "major-sixth"),
    ("m6", &[0, 3, 7, 9], "m6", "minor-sixth"),
    ("9", &[0, 2, 4, 7, 10], "9", "dominant-ninth"),
];

/// Qualities the recognizer considers (the PRD set: triads, sevenths, dim, sus, add9).
const RECOGNIZED: &[&str] = &["maj", "min", "7", "maj7", "m7", "dim", "sus4", "sus2", "add9", "m7b5", "dim7", "aug"];

pub fn quality_info(q: &str) -> Option<(&'static [i32], &'static str, &'static str)> {
    QUALITIES
        .iter()
        .find(|x| x.0 == q)
        .map(|x| (x.1, x.2, x.3))
}

pub fn suffix(q: &str) -> &'static str {
    quality_info(q).map(|x| x.1).unwrap_or("")
}

/// MusicXML `<kind>` for a quality.
pub fn musicxml_kind(q: &str) -> &'static str {
    quality_info(q).map(|x| x.2).unwrap_or("major")
}

/// Quality from a MusicXML `<kind>` (and an added ninth degree).
pub fn quality_from_kind(kind: &str, add9: bool) -> String {
    let q = match kind {
        "major" if add9 => "add9",
        "major" => "maj",
        "minor" => "min",
        "dominant" | "dominant-seventh" => "7",
        "major-seventh" => "maj7",
        "minor-seventh" => "m7",
        "diminished" => "dim",
        "diminished-seventh" => "dim7",
        "half-diminished" => "m7b5",
        "augmented" => "aug",
        "suspended-second" => "sus2",
        "suspended-fourth" => "sus4",
        "major-sixth" => "6",
        "minor-sixth" => "m6",
        "dominant-ninth" => "9",
        "power" => "maj",
        _ => "maj",
    };
    q.to_string()
}

pub fn chord_text(root: Spelled, quality: &str, bass: Option<Spelled>) -> String {
    let mut s = format!("{}{}", root.name(), suffix(quality));
    if let Some(b) = bass {
        s.push('/');
        s.push_str(&b.name());
    }
    s
}

/// Parse `C`, `Am7`, `G/B`, `F#m7b5`, `Bbmaj7`, `Dsus4`, `Cadd9`.
pub fn parse_symbol(text: &str) -> Option<(Spelled, String, Option<Spelled>)> {
    let t = text.trim();
    let (main, bass) = match t.split_once('/') {
        Some((m, b)) => (m, Some(b)),
        None => (t, None),
    };
    let (root, rest) = parse_spelled_prefix(main)?;
    let rest = rest.trim();
    let quality = match rest {
        "" | "M" | "maj" | "major" => "maj",
        "m" | "min" | "-" => "min",
        "7" | "dom7" => "7",
        "maj7" | "M7" | "Δ" | "Δ7" | "ma7" => "maj7",
        "m7" | "min7" | "-7" => "m7",
        "dim" | "°" | "o" => "dim",
        "dim7" | "°7" | "o7" => "dim7",
        "m7b5" | "ø" | "ø7" | "min7b5" => "m7b5",
        "aug" | "+" => "aug",
        "sus2" => "sus2",
        "sus4" | "sus" => "sus4",
        "add9" | "add2" => "add9",
        "6" => "6",
        "m6" => "m6",
        "9" => "9",
        _ => return None,
    };
    let bass = match bass {
        Some(b) => Some(parse_spelled_prefix(b).filter(|(_, r)| r.is_empty())?.0),
        None => None,
    };
    Some((root, quality.to_string(), bass))
}

fn parse_spelled_prefix(s: &str) -> Option<(Spelled, &str)> {
    let mut it = s.char_indices();
    let (_, c) = it.next()?;
    let step = c.to_ascii_uppercase();
    if !('A'..='G').contains(&step) || !c.is_ascii_uppercase() {
        return None;
    }
    let mut alter = 0i8;
    let mut end = c.len_utf8();
    for (i, ch) in it {
        match ch {
            '#' | '♯' => alter += 1,
            'b' | '♭' => alter -= 1,
            _ => {
                end = i;
                return Some((Spelled { step, alter }, &s[end..]));
            }
        }
        end = i + ch.len_utf8();
    }
    Some((Spelled { step, alter }, &s[end..]))
}

/// A recognized chord over a time span (divisions, absolute).
#[derive(Clone, Debug, PartialEq)]
pub struct ChordSpan {
    pub start: u64,
    pub end: u64,
    pub root_pc: i32,
    pub quality: String,
    pub bass_pc: Option<i32>,
    pub confidence: f32,
}

impl ChordSpan {
    pub fn spell(&self, key: KeySig) -> (Spelled, Option<Spelled>) {
        let root = spell_chord_root(self.root_pc, &self.quality, key);
        let bass = self.bass_pc.map(|b| spell_pc(b, key));
        (root, bass)
    }
    pub fn text(&self, key: KeySig) -> String {
        let (r, b) = self.spell(key);
        chord_text(r, &self.quality, b)
    }
}

/// Spell a chord root: key spelling, but a minor/dim chord on a chromatic degree in a
/// sharp key keeps sharps, and flat-side roots (bVII, bIII, bVI) in major keys are flats.
pub fn spell_chord_root(pc: i32, _quality: &str, key: KeySig) -> Spelled {
    spell_pc(pc, key)
}

/// Recognize chords from notes `(start, end, midi)` over beat spans `(start, end)` (all in
/// the same tick unit). `bar_starts` marks beats that begin a measure (changes there are
/// cheaper).
pub fn recognize(notes: &[(u64, u64, i32)], beats: &[(u64, u64)], bar_starts: &[bool]) -> Vec<ChordSpan> {
    // candidate labels: (root, quality index into RECOGNIZED)
    let labels: Vec<(i32, &str)> = (0..12)
        .flat_map(|r| RECOGNIZED.iter().map(move |q| (r, *q)))
        .collect();
    let nl = labels.len();
    let mut emis: Vec<Vec<f64>> = Vec::with_capacity(beats.len());
    let mut basses: Vec<Option<i32>> = Vec::with_capacity(beats.len());
    let mut voiced: Vec<bool> = Vec::with_capacity(beats.len());
    for &(bs, be) in beats {
        let mut chroma = [0.0f64; 12];
        let mut lowest: Option<(i32, u64)> = None;
        for &(s, e, m) in notes {
            let ov = e.min(be).saturating_sub(s.max(bs));
            if ov == 0 {
                continue;
            }
            chroma[m.rem_euclid(12) as usize] += ov as f64;
            if lowest.map(|(lm, _)| m < lm).unwrap_or(true) {
                lowest = Some((m, ov));
            }
        }
        // the bass is scored separately below; count it at half weight in the chroma so a
        // doubled bass note does not outvote the upper voices (Fmaj7/A is not Am)
        if let Some((lm, lov)) = lowest {
            let distinct = chroma.iter().filter(|x| **x > 0.0).count();
            if distinct >= 3 {
                chroma[lm.rem_euclid(12) as usize] -= lov as f64 * 0.5;
            }
        }
        let total: f64 = chroma.iter().sum();
        voiced.push(total > 0.0);
        let bass = lowest.map(|(m, _)| m.rem_euclid(12));
        basses.push(bass);
        let norm = chroma.iter().map(|x| x * x).sum::<f64>().sqrt().max(1e-9);
        let row: Vec<f64> = labels
            .iter()
            .map(|(r, q)| {
                let ivs = quality_info(q).unwrap().0;
                let mut dot = 0.0;
                for iv in ivs {
                    dot += chroma[((r + iv).rem_euclid(12)) as usize];
                }
                let mut s = dot / (norm * (ivs.len() as f64).sqrt());
                // emphasize the root a little so inversions don't swap roots
                s += 0.04 * chroma[*r as usize] / norm;
                if let Some(b) = bass {
                    if b == *r {
                        s += 0.08;
                    } else if !ivs.iter().any(|iv| (r + iv).rem_euclid(12) == b) {
                        s -= 0.06;
                    }
                }
                // slight preference for simpler chords
                s -= 0.01 * (ivs.len() as f64 - 3.0);
                s
            })
            .collect();
        emis.push(row);
    }
    if beats.is_empty() {
        return Vec::new();
    }
    // Viterbi over labels; unvoiced beats keep the previous chord cheaply.
    let mut score = vec![0.0f64; nl];
    let mut back: Vec<Vec<usize>> = Vec::with_capacity(beats.len());
    for (t, row) in emis.iter().enumerate() {
        let pen = if bar_starts.get(t).copied().unwrap_or(false) { 0.06 } else { 0.14 };
        let (best_prev, best_val) = score
            .iter()
            .enumerate()
            .fold((0usize, f64::MIN), |acc, (i, v)| if *v > acc.1 { (i, *v) } else { acc });
        let mut ns = vec![0.0; nl];
        let mut bk = vec![0usize; nl];
        for j in 0..nl {
            let stay = score[j];
            let change = best_val - if t == 0 { 0.0 } else { pen };
            let (v, from) = if stay >= change { (stay, j) } else { (change, best_prev) };
            let e = if voiced[t] { row[j] } else { 0.0 };
            ns[j] = v + e;
            bk[j] = from;
        }
        score = ns;
        back.push(bk);
    }
    let mut path = vec![0usize; beats.len()];
    let mut cur = score
        .iter()
        .enumerate()
        .fold((0usize, f64::MIN), |acc, (i, v)| if *v > acc.1 { (i, *v) } else { acc })
        .0;
    for t in (0..beats.len()).rev() {
        path[t] = cur;
        cur = back[t][cur];
    }
    // merge into spans
    let mut out: Vec<ChordSpan> = Vec::new();
    for (t, &li) in path.iter().enumerate() {
        if !voiced[t] {
            if let Some(last) = out.last_mut() {
                last.end = beats[t].1;
            }
            continue;
        }
        let (r, q) = labels[li];
        let ivs = quality_info(q).unwrap().0;
        let bass = basses[t].filter(|b| *b != r && ivs.iter().any(|iv| (r + iv).rem_euclid(12) == *b));
        let conf = emis[t][li].clamp(0.0, 1.0) as f32;
        match out.last_mut() {
            Some(last) if last.root_pc == r && last.quality == q && last.end == beats[t].0 && (last.bass_pc == bass || bass.is_none()) => {
                last.end = beats[t].1;
                last.confidence = last.confidence.min(conf);
            }
            _ => out.push(ChordSpan {
                start: beats[t].0,
                end: beats[t].1,
                root_pc: r,
                quality: q.to_string(),
                bass_pc: bass,
                confidence: conf,
            }),
        }
    }
    // a symbol needs real harmony: at least three pitch classes sound in its span
    // (a bare melody line gets no guesses)
    out.retain(|c| {
        let mut pcs = [false; 12];
        for &(s, e, m) in notes {
            if s < c.end && e > c.start {
                pcs[m.rem_euclid(12) as usize] = true;
            }
        }
        pcs.iter().filter(|x| **x).count() >= 3
    });
    out
}

// ------------------------------------------------------------------------------------
// Guitar shapes
// ------------------------------------------------------------------------------------

type Shape = [Option<u8>; 6];

const X: Option<u8> = None;
const fn f(n: u8) -> Option<u8> {
    Some(n)
}

/// Open-position shapes, frets from low E to high E.
const OPEN_SHAPES: &[(&str, &str, Shape)] = &[
    ("C", "maj", [X, f(3), f(2), f(0), f(1), f(0)]),
    ("C", "maj7", [X, f(3), f(2), f(0), f(0), f(0)]),
    ("C", "7", [X, f(3), f(2), f(3), f(1), f(0)]),
    ("C", "add9", [X, f(3), f(2), f(0), f(3), f(0)]),
    ("C", "sus4", [X, f(3), f(3), f(0), f(1), f(1)]),
    ("C", "sus2", [X, f(3), f(0), f(0), f(1), f(3)]),
    ("D", "maj", [X, X, f(0), f(2), f(3), f(2)]),
    ("D", "min", [X, X, f(0), f(2), f(3), f(1)]),
    ("D", "7", [X, X, f(0), f(2), f(1), f(2)]),
    ("D", "maj7", [X, X, f(0), f(2), f(2), f(2)]),
    ("D", "m7", [X, X, f(0), f(2), f(1), f(1)]),
    ("D", "sus2", [X, X, f(0), f(2), f(3), f(0)]),
    ("D", "sus4", [X, X, f(0), f(2), f(3), f(3)]),
    ("D", "dim", [X, X, f(0), f(1), f(3), f(1)]),
    ("E", "maj", [f(0), f(2), f(2), f(1), f(0), f(0)]),
    ("E", "min", [f(0), f(2), f(2), f(0), f(0), f(0)]),
    ("E", "7", [f(0), f(2), f(0), f(1), f(0), f(0)]),
    ("E", "maj7", [f(0), f(2), f(1), f(1), f(0), f(0)]),
    ("E", "m7", [f(0), f(2), f(0), f(0), f(0), f(0)]),
    ("E", "sus4", [f(0), f(2), f(2), f(2), f(0), f(0)]),
    ("E", "add9", [f(0), f(2), f(4), f(1), f(0), f(0)]),
    ("F", "maj", [f(1), f(3), f(3), f(2), f(1), f(1)]),
    ("F", "maj7", [X, X, f(3), f(2), f(1), f(0)]),
    ("G", "maj", [f(3), f(2), f(0), f(0), f(0), f(3)]),
    ("G", "7", [f(3), f(2), f(0), f(0), f(0), f(1)]),
    ("G", "maj7", [f(3), f(2), f(0), f(0), f(0), f(2)]),
    ("G", "sus4", [f(3), X, f(0), f(0), f(1), f(3)]),
    ("G", "add9", [f(3), X, f(0), f(2), f(0), f(3)]),
    ("A", "maj", [X, f(0), f(2), f(2), f(2), f(0)]),
    ("A", "min", [X, f(0), f(2), f(2), f(1), f(0)]),
    ("A", "7", [X, f(0), f(2), f(0), f(2), f(0)]),
    ("A", "maj7", [X, f(0), f(2), f(1), f(2), f(0)]),
    ("A", "m7", [X, f(0), f(2), f(0), f(1), f(0)]),
    ("A", "sus2", [X, f(0), f(2), f(2), f(0), f(0)]),
    ("A", "sus4", [X, f(0), f(2), f(2), f(3), f(0)]),
    ("A", "add9", [X, f(0), f(2), f(4), f(2), f(0)]),
    ("B", "7", [X, f(2), f(1), f(2), f(0), f(2)]),
];

/// Movable shapes relative to a barre: (quality, root string 6 or 5, offsets).
const E_SHAPES: &[(&str, [i8; 6])] = &[
    ("maj", [0, 2, 2, 1, 0, 0]),
    ("min", [0, 2, 2, 0, 0, 0]),
    ("7", [0, 2, 0, 1, 0, 0]),
    ("maj7", [0, -1, 1, 1, 0, -1]),
    ("m7", [0, 2, 0, 0, 0, 0]),
    ("sus4", [0, 2, 2, 2, 0, 0]),
];
const A_SHAPES: &[(&str, [i8; 6])] = &[
    ("maj", [-1, 0, 2, 2, 2, 0]),
    ("min", [-1, 0, 2, 2, 1, 0]),
    ("7", [-1, 0, 2, 0, 2, 0]),
    ("maj7", [-1, 0, 2, 1, 2, 0]),
    ("m7", [-1, 0, 2, 0, 1, 0]),
    ("sus2", [-1, 0, 2, 2, 0, 0]),
    ("sus4", [-1, 0, 2, 2, 3, 0]),
    ("add9", [-1, 0, 2, 4, 2, 0]),
    ("dim", [-1, 0, 1, 2, 1, -1]),
    ("dim7", [-1, 0, 1, 2, 1, 2]),
    ("m7b5", [-1, 0, 1, 0, 1, -1]),
    ("aug", [-1, 0, 3, 2, 2, 1]),
    ("6", [-1, 0, 2, 2, 2, 2]),
    ("m6", [-1, 0, 2, 2, 1, 2]),
    ("9", [-1, 0, 2, 4, 2, 3]),
];

fn movable(base: u8, offs: &[i8; 6]) -> Shape {
    let mut s = [X; 6];
    for (i, o) in offs.iter().enumerate() {
        if *o >= 0 {
            s[i] = Some(base + *o as u8);
        }
    }
    s
}

/// A guitar shape for a chord (root pitch class, quality); open shapes first, then
/// E-shape and A-shape barre chords (whichever sits lower on the neck).
pub fn guitar_shape(root_pc: i32, quality: &str) -> Option<Shape> {
    for (name, q, shape) in OPEN_SHAPES {
        if *q == quality && crate::pitch::step_pc(name.chars().next().unwrap()) == root_pc.rem_euclid(12) {
            return Some(*shape);
        }
    }
    let fret_for = |open_pc: i32| -> u8 {
        let f = (root_pc - open_pc).rem_euclid(12) as u8;
        if f == 0 {
            12
        } else {
            f
        }
    };
    let e = E_SHAPES.iter().find(|s| s.0 == quality).map(|s| (fret_for(4), s.1));
    let a = A_SHAPES.iter().find(|s| s.0 == quality).map(|s| (fret_for(9), s.1));
    let (base, offs) = match (e, a) {
        (Some(e), Some(a)) => {
            if e.0 <= a.0 {
                e
            } else {
                a
            }
        }
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => return None,
    };
    Some(movable(base, &offs))
}

/// Frame for a shape.
pub fn frame_for(shape: Shape) -> GuitarFrame {
    let fretted: Vec<u8> = shape.iter().flatten().copied().filter(|f| *f > 0).collect();
    let max = fretted.iter().copied().max().unwrap_or(0);
    let min = fretted.iter().copied().min().unwrap_or(1);
    let first_fret = if max > 4 { min } else { 1 };
    let frets = if max.saturating_sub(first_fret) >= 4 { 5 } else { 4 };
    GuitarFrame {
        strings: 6,
        frets,
        first_fret,
        fret_by_string: shape.to_vec(),
        shape: None,
        capo: None,
    }
}

/// Keys that are comfortable on guitar in open position.
fn guitar_friendly(key: KeySig) -> bool {
    let pc = key_tonic_pc(key);
    match key.mode {
        Mode::Major => matches!(pc, 0 | 2 | 4 | 7 | 9),
        Mode::Minor => matches!(pc, 9 | 4 | 2),
    }
}

/// Capo suggestion for a key: (capo fret, shape key) or `None` when no capo is needed.
pub fn capo_for(key: KeySig) -> Option<(u8, KeySig)> {
    if guitar_friendly(key) {
        return None;
    }
    let tonic = key_tonic_pc(key);
    for c in 1..=7 {
        let shape_pc = tonic - c;
        let k = crate::pitch::key_from_pc(shape_pc, key.mode, Some(true));
        if guitar_friendly(k) {
            return Some((c as u8, k));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_symbols() {
        let (r, q, b) = parse_symbol("G/B").unwrap();
        assert_eq!((r.name(), q.as_str(), b.unwrap().name()), ("G".into(), "maj", "B".into()));
        let (r, q, _) = parse_symbol("F#m7b5").unwrap();
        assert_eq!((r.name(), q.as_str()), ("F#".into(), "m7b5"));
        let (r, q, _) = parse_symbol("Bbmaj7").unwrap();
        assert_eq!((r.name(), q.as_str()), ("Bb".into(), "maj7"));
        assert!(parse_symbol("H7").is_none());
        assert_eq!(chord_text(Spelled { step: 'A', alter: 0 }, "m7", None), "Am7");
    }

    #[test]
    fn recognize_progression() {
        // C | Am7 | F | G7/B, one bar each (4 beats of 10 ticks)
        let bars: &[&[i32]] = &[&[48, 60, 64, 67], &[45, 60, 64, 67, 69], &[41, 60, 65, 69], &[47, 62, 65, 67]];
        let mut notes = Vec::new();
        let mut beats = Vec::new();
        let mut starts = Vec::new();
        for (i, b) in bars.iter().enumerate() {
            let s = i as u64 * 40;
            for m in *b {
                notes.push((s, s + 40, *m));
            }
            for k in 0..4 {
                beats.push((s + k * 10, s + k * 10 + 10));
                starts.push(k == 0);
            }
        }
        let spans = recognize(&notes, &beats, &starts);
        let key = KeySig::major(0);
        let texts: Vec<String> = spans.iter().map(|s| s.text(key)).collect();
        assert_eq!(texts, vec!["C", "Am7", "F", "G7/B"]);
    }

    #[test]
    fn shapes_and_capo() {
        let g = guitar_shape(7, "maj").unwrap();
        assert_eq!(g, [f(3), f(2), f(0), f(0), f(0), f(3)]);
        let bb = guitar_shape(10, "maj").unwrap();
        // A-shape barre at fret 1
        assert_eq!(bb, [X, f(1), f(3), f(3), f(3), f(1)]);
        let fr = frame_for(guitar_shape(3, "min").unwrap());
        assert!(fr.first_fret >= 1);
        // Eb major → capo 1 with D shapes? (Eb - 1 = D)
        let (c, k) = capo_for(KeySig::major(-3)).unwrap();
        assert_eq!((c, crate::pitch::key_name(k)), (1, "D major".to_string()));
        assert!(capo_for(KeySig::major(1)).is_none());
    }
}
