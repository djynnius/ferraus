//! Tonic sol-fa: movable do (`d r m f s l t`, lah-mode minor) or fixed do, octave marks
//! (`d'` above, `d,` below), and Curwen rhythm punctuation (`|` bar, `:` beat, `.` half
//! beat, `,` quarter beat, `-` continuation, blank for a rest).

use crate::model::*;
use crate::musicxml::key_alter;
use crate::pitch::{key_name, key_tonic, step_index};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolfaKind {
    Movable,
    Fixed,
}

const SHARP_NAMES: [&str; 12] = ["d", "de", "r", "ri", "m", "f", "fe", "s", "se", "l", "li", "t"];
const FLAT_NAMES: [&str; 12] = ["d", "ra", "r", "ma", "m", "f", "fe", "s", "lo", "l", "ta", "t"];
const FIXED: [&str; 7] = ["do", "re", "mi", "fa", "sol", "la", "si"];

/// The "doh" of a key: the major tonic, or the relative major in lah-mode minor.
pub fn doh(key: KeySig) -> Spelled {
    key_tonic(KeySig::major(key.fifths))
}

/// Syllable without octave marks.
pub fn syllable(p: Pitch, key: KeySig, kind: SolfaKind) -> String {
    match kind {
        SolfaKind::Fixed => {
            let base = FIXED[step_index(p.step) as usize];
            let acc = match p.alter {
                1 => "#",
                2 => "##",
                -1 => "b",
                -2 => "bb",
                _ => "",
            };
            format!("{base}{acc}")
        }
        SolfaKind::Movable => {
            let d = doh(key);
            let semis = (p.midi() - d.pc()).rem_euclid(12) as usize;
            let expected = key_alter(key.fifths, p.step);
            if p.alter < expected {
                FLAT_NAMES[semis].to_string()
            } else {
                SHARP_NAMES[semis].to_string()
            }
        }
    }
}

/// MIDI number of the reference doh (no octave mark) for a melody: the doh at or below
/// the median note.
pub fn reference_doh(midis: &[i32], key: KeySig, kind: SolfaKind) -> i32 {
    let pc = match kind {
        SolfaKind::Movable => doh(key).pc(),
        SolfaKind::Fixed => 0,
    };
    let mut v: Vec<i32> = midis.to_vec();
    v.sort();
    let median = v.get(v.len() / 2).copied().unwrap_or(60);
    median - (median - pc).rem_euclid(12)
}

/// Syllable with octave marks relative to `doh_ref`.
pub fn syllable_marked(p: Pitch, key: KeySig, kind: SolfaKind, doh_ref: i32) -> String {
    let mut s = syllable(p, key, kind);
    // octave from the letter so that `t` just below doh is `t,`
    let d = match kind {
        SolfaKind::Movable => doh(key),
        SolfaKind::Fixed => Spelled { step: 'C', alter: 0 },
    };
    let steps_from = (p.octave as i32 * 7 + step_index(p.step)) - (oct_of(doh_ref, d) * 7 + step_index(d.step));
    let oct = steps_from.div_euclid(7);
    if oct > 0 {
        s.push_str(&"'".repeat(oct as usize));
    } else if oct < 0 {
        s.push_str(&",".repeat((-oct) as usize));
    }
    s
}

fn oct_of(midi: i32, d: Spelled) -> i32 {
    (midi - crate::pitch::step_pc(d.step) - d.alter as i32).div_euclid(12) - 1
}

/// The melody: first pitched part, first staff, voice 1, top note of each event.
fn melody_staff(score: &Score) -> Option<&Staff> {
    score.parts.iter().find(|p| !p.percussion).and_then(|p| p.staves.first())
}

/// Fill `Event::solfa` for every pitched event of every non-percussion part.
pub fn annotate(score: &mut Score, kind: SolfaKind) {
    for p in score.parts.iter_mut().filter(|p| !p.percussion) {
        for st in p.staves.iter_mut() {
            let midis: Vec<i32> = st
                .measures
                .iter()
                .flat_map(|m| m.voices.iter().flat_map(|v| v.events.iter().filter_map(|e| e.notes.iter().map(|n| n.midi).max())))
                .collect();
            let first_key = Score::key_at(st, 0);
            let doh_ref = reference_doh(&midis, first_key, kind);
            for mi in 0..st.measures.len() {
                let key = Score::key_at(st, mi);
                for v in st.measures[mi].voices.iter_mut() {
                    for e in v.events.iter_mut() {
                        if let Some(top) = e.notes.iter().max_by_key(|n| n.midi) {
                            e.solfa = Some(syllable_marked(top.pitch, key, kind, doh_ref));
                        }
                    }
                }
            }
        }
    }
}

struct Seg {
    start: u64,
    end: u64,
    text: Option<String>,
    cont: bool,
    lyric: Option<String>,
}

/// One bar of sol-fa text plus its lyric syllables.
fn bar_text(m: &Measure, time: TimeSig, div: u32, key: KeySig, kind: SolfaKind, doh_ref: i32, carry: &mut bool) -> (String, Vec<String>) {
    let pulse = (div * 4 / time.beat_type.max(1)) as u64;
    let full = time.measure_divisions(div) as u64;
    let mut segs: Vec<Seg> = Vec::new();
    let mut lyrics = Vec::new();
    let mut pos = 0u64;
    if let Some(v) = m.voices.iter().find(|v| v.number == 1).or(m.voices.first()) {
        for e in &v.events {
            if e.grace {
                continue;
            }
            let d = e.duration as u64;
            if e.is_rest() {
                segs.push(Seg { start: pos, end: pos + d, text: None, cont: false, lyric: None });
            } else {
                let top = e.notes.iter().max_by_key(|n| n.midi).unwrap();
                let cont = top.tie_stop;
                let text = if cont { None } else { Some(syllable_marked(top.pitch, key, kind, doh_ref)) };
                let lyric = e.lyrics.first().map(|l| match l.syllabic.as_str() {
                    "begin" | "middle" => format!("{}-", l.text),
                    _ => l.text.clone(),
                });
                if let Some(l) = &lyric {
                    lyrics.push(l.clone());
                }
                segs.push(Seg { start: pos, end: pos + d, text, cont, lyric });
            }
            pos += d;
        }
    }
    let len = pos.max(1);
    // pickups are aligned to the end of the bar
    let shift = if m.implicit && len < full { full - len } else { 0 };
    let mut cells: Vec<String> = Vec::new();
    let mut p = 0u64;
    while p < full {
        let ps = p;
        let pe = p + pulse;
        p = pe;
        if pe <= shift {
            continue;
        }
        let rel_s = ps.saturating_sub(shift);
        let rel_e = pe - shift;
        // boundaries inside this pulse
        let mut bounds: Vec<u64> = segs
            .iter()
            .flat_map(|s| [s.start, s.end])
            .filter(|b| *b > rel_s && *b < rel_e)
            .map(|b| b - rel_s)
            .collect();
        bounds.sort();
        bounds.dedup();
        let width = rel_e - rel_s;
        let n = [1u64, 2, 4, 3, 6, 8]
            .into_iter()
            .find(|n| width % n == 0 && bounds.iter().all(|b| b % (width / n) == 0))
            .unwrap_or(4);
        let slot = width / n;
        let mut slots: Vec<String> = Vec::new();
        for k in 0..n {
            let at = rel_s + k * slot;
            let seg = segs.iter().find(|s| s.start <= at && at < s.end);
            let txt = match seg {
                None => String::new(),
                Some(s) if s.start == at => match &s.text {
                    Some(t) => t.clone(),
                    None if s.cont => "-".into(),
                    None => String::new(),
                },
                Some(s) => {
                    // continuing note: a dash at the start of a pulse, nothing inside it
                    if (s.text.is_some() || s.cont) && k == 0 {
                        "-".into()
                    } else {
                        String::new()
                    }
                }
            };
            slots.push(txt);
        }
        let _ = carry;
        let cell = match n {
            1 => slots[0].clone(),
            2 => format!("{}.{}", slots[0], slots[1]),
            3 => format!("{}.{}.{}", slots[0], slots[1], slots[2]),
            4 => {
                let h1 = join_q(&slots[0], &slots[1]);
                let h2 = join_q(&slots[2], &slots[3]);
                format!("{h1}.{h2}")
            }
            6 => format!(
                "{}.{}.{}",
                join_q(&slots[0], &slots[1]),
                join_q(&slots[2], &slots[3]),
                join_q(&slots[4], &slots[5])
            ),
            _ => slots.join(","),
        };
        cells.push(cell);
    }
    let _ = segs.iter().map(|s| &s.lyric).count();
    (cells.join(" :"), lyrics)
}

fn join_q(a: &str, b: &str) -> String {
    if b.is_empty() {
        a.to_string()
    } else {
        format!("{a},{b}")
    }
}

fn header_lines(score: &Score, key: KeySig, time: TimeSig, kind: SolfaKind) -> Vec<String> {
    let mut h = Vec::new();
    let keytxt = match kind {
        SolfaKind::Movable => {
            if key.mode == Mode::Minor {
                format!("Doh is {} ({}, lah is {})", doh(key).name(), key_name(key), key_tonic(key).name())
            } else {
                format!("Doh is {} ({})", doh(key).name(), key_name(key))
            }
        }
        SolfaKind::Fixed => format!("Fixed do ({})", key_name(key)),
    };
    h.push(format!("Key: {keytxt}"));
    h.push(format!("Time: {}/{}", time.beats, time.beat_type));
    let tempo = score
        .parts
        .first()
        .and_then(|p| p.staves.first())
        .and_then(|s| s.measures.iter().flat_map(|m| m.tempo.iter()).next())
        .map(|t| t.bpm);
    if let Some(t) = tempo {
        h.push(format!("Tempo: {} bpm", (t * 10.0).round() / 10.0));
    }
    h
}

/// Lines of sol-fa (each up to `bars_per_line` bars) with their lyrics.
fn lines(score: &Score, kind: SolfaKind, bars_per_line: usize) -> (KeySig, TimeSig, Vec<(String, Vec<String>)>) {
    let Some(st) = melody_staff(score) else {
        return (KeySig::major(0), TimeSig::new(4, 4), Vec::new());
    };
    let midis: Vec<i32> = st
        .measures
        .iter()
        .flat_map(|m| m.voices.iter().filter(|v| v.number == 1).flat_map(|v| v.events.iter().filter_map(|e| e.notes.iter().map(|n| n.midi).max())))
        .collect();
    let key0 = Score::key_at(st, 0);
    let time0 = Score::time_at(st, 0);
    let doh_ref = reference_doh(&midis, key0, kind);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_lyr = Vec::new();
    let mut nbars = 0;
    let mut carry = false;
    for (mi, m) in st.measures.iter().enumerate() {
        let key = Score::key_at(st, mi);
        let time = Score::time_at(st, mi);
        let (t, l) = bar_text(m, time, score.divisions, key, kind, doh_ref, &mut carry);
        cur.push('|');
        cur.push_str(&t);
        cur.push(' ');
        cur_lyr.extend(l);
        nbars += 1;
        if nbars == bars_per_line {
            out.push((std::mem::take(&mut cur), std::mem::take(&mut cur_lyr)));
            nbars = 0;
        }
    }
    if !cur.is_empty() {
        out.push((cur, cur_lyr));
    }
    if let Some(last) = out.last_mut() {
        last.0.push_str("||");
    }
    (key0, time0, out)
}

/// Plain-text tonic sol-fa.
pub fn write_text(score: &Score, kind: SolfaKind) -> String {
    let (key, time, ls) = lines(score, kind, 4);
    let mut s = String::new();
    if let Some(t) = &score.title {
        s.push_str(t);
        s.push('\n');
    }
    if let Some(c) = &score.composer {
        s.push_str(c);
        s.push('\n');
    }
    for h in header_lines(score, key, time, kind) {
        s.push_str(&h);
        s.push('\n');
    }
    s.push('\n');
    for (l, lyr) in ls {
        s.push_str(l.trim_end());
        s.push('\n');
        if !lyr.is_empty() {
            s.push_str(&lyr.join(" "));
            s.push('\n');
        }
    }
    s
}

/// Markdown tonic sol-fa.
pub fn write_markdown(score: &Score, kind: SolfaKind) -> String {
    let (key, time, ls) = lines(score, kind, 4);
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", score.title.clone().unwrap_or_else(|| "Untitled".into())));
    if let Some(c) = &score.composer {
        s.push_str(&format!("*{c}*\n\n"));
    }
    s.push_str(match kind {
        SolfaKind::Movable => "Tonic sol-fa, movable doh.\n\n",
        SolfaKind::Fixed => "Sol-fa, fixed do.\n\n",
    });
    for h in header_lines(score, key, time, kind) {
        let (k, v) = h.split_once(": ").unwrap_or((&h, ""));
        s.push_str(&format!("- **{k}:** {v}\n"));
    }
    s.push_str("\n```text\n");
    for (l, lyr) in &ls {
        s.push_str(l.trim_end());
        s.push('\n');
        if !lyr.is_empty() {
            s.push_str(&lyr.join(" "));
            s.push('\n');
        }
    }
    s.push_str("```\n\n");
    s.push_str("`|` bar, `:` beat, `.` half beat, `,` quarter beat, `-` held, blank = rest, `'` octave up, `,` after a syllable octave down.\n");
    s
}

/// Extract the syllables (with octave marks) from sol-fa text, in order.
pub fn syllables_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.starts_with('|') {
            continue;
        }
        let mut cur = String::new();
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_lowercase() {
                cur.push(c);
            } else if (c == '\'' || c == ',') && !cur.is_empty() {
                // octave mark only when directly after a syllable and followed by a
                // non-letter (a quarter-beat comma is followed by a syllable)
                let next = chars.get(i + 1).copied().unwrap_or(' ');
                if c == '\'' || !next.is_ascii_lowercase() && next != '-' {
                    cur.push(c);
                } else {
                    out.push(std::mem::take(&mut cur));
                }
            } else if c == '#' || c == 'b' && !cur.is_empty() {
                cur.push(c);
            } else if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            i += 1;
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pitch::spell_midi;

    #[test]
    fn movable_syllables() {
        let g = KeySig::major(1);
        let names: Vec<String> = [67, 69, 71, 72, 74, 76, 78, 79]
            .iter()
            .map(|m| syllable(spell_midi(*m, g), g, SolfaKind::Movable))
            .collect();
        assert_eq!(names, vec!["d", "r", "m", "f", "s", "l", "t", "d"]);
        // E minor is lah-mode: E = l, D# = se
        let em = KeySig::minor(1);
        assert_eq!(syllable(spell_midi(64, em), em, SolfaKind::Movable), "l");
        assert_eq!(syllable(spell_midi(63, em), em, SolfaKind::Movable), "se");
        // Bb in C major = ta, F# = fe
        let c = KeySig::major(0);
        assert_eq!(syllable(spell_midi(70, c), c, SolfaKind::Movable), "ta");
        assert_eq!(syllable(spell_midi(66, c), c, SolfaKind::Movable), "fe");
        assert_eq!(syllable(spell_midi(66, g), g, SolfaKind::Fixed), "fa#");
    }

    #[test]
    fn octave_marks() {
        let c = KeySig::major(0);
        let r = reference_doh(&[60, 62, 64], c, SolfaKind::Movable);
        assert_eq!(r, 60);
        assert_eq!(syllable_marked(spell_midi(72, c), c, SolfaKind::Movable, r), "d'");
        assert_eq!(syllable_marked(spell_midi(59, c), c, SolfaKind::Movable, r), "t,");
        assert_eq!(syllable_marked(spell_midi(55, c), c, SolfaKind::Movable, r), "s,");
    }
}
