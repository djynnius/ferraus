//! LilyPond source writer (absolute pitches, chord names, lyrics, drum staves, grand
//! staves, repeats as repeat commands) and a reader for the same subset.

use crate::chords;
use crate::model::*;
use crate::pitch::{lcm, type_for, TYPES};
use ferraus_core::{Error, Result};
use std::fmt::Write as _;

fn ly_pitch(p: Pitch) -> String {
    let mut s = String::new();
    s.push(p.step.to_ascii_lowercase());
    let acc = match p.alter {
        2 => "isis",
        1 => "is",
        -1 => "es",
        -2 => "eses",
        _ => "",
    };
    s.push_str(acc);
    // c = C3; c' = C4
    let marks = p.octave as i32 - 3;
    if marks > 0 {
        s.push_str(&"'".repeat(marks as usize));
    } else if marks < 0 {
        s.push_str(&",".repeat((-marks) as usize));
    }
    s
}

fn ly_dur(t: &str, dots: u8) -> String {
    let base = match t {
        "breve" => "\\breve",
        "whole" => "1",
        "half" => "2",
        "quarter" => "4",
        "eighth" => "8",
        "16th" => "16",
        "32nd" => "32",
        "64th" => "64",
        "128th" => "128",
        _ => "4",
    };
    format!("{base}{}", ".".repeat(dots as usize))
}

/// Duration string for a plain (non-tuplet) length; falls back to a scaled duration.
fn dur_for(d: u32, div: u32, tup: Option<(u32, u32)>) -> String {
    match type_for(d, div, tup) {
        Some((t, dots)) => ly_dur(t, dots),
        None => {
            // quarter scaled: 4*n/m
            let g = crate::pitch::gcd(d as u64, div as u64);
            format!("4*{}/{}", d as u64 / g, div as u64 / g)
        }
    }
}

fn drum_name(key: i32) -> &'static str {
    match key {
        35 => "bda",
        36 => "bd",
        37 => "ss",
        38 => "sn",
        39 => "hc",
        40 => "sne",
        41 => "tomfl",
        42 => "hhc",
        43 => "tomfh",
        44 => "hhp",
        45 => "toml",
        46 => "hho",
        47 => "tomml",
        48 => "tommh",
        49 => "cymc",
        50 => "tomh",
        51 => "cymr",
        52 => "cymch",
        53 => "rb",
        54 => "tamb",
        55 => "cyms",
        56 => "cb",
        57 => "cymcb",
        59 => "cymrb",
        _ => "sn",
    }
}

fn drum_key(name: &str) -> Option<i32> {
    Some(match name {
        "bda" => 35,
        "bd" | "bassdrum" => 36,
        "ss" | "sidestick" => 37,
        "sn" | "snare" => 38,
        "hc" | "handclap" => 39,
        "sne" => 40,
        "tomfl" => 41,
        "hhc" | "hh" => 42,
        "tomfh" => 43,
        "hhp" => 44,
        "toml" => 45,
        "hho" => 46,
        "tomml" => 47,
        "tommh" => 48,
        "cymc" | "cymca" => 49,
        "tomh" => 50,
        "cymr" | "cymra" => 51,
        "cymch" => 52,
        "rb" => 53,
        "tamb" => 54,
        "cyms" => 55,
        "cb" => 56,
        "cymcb" => 57,
        "cymrb" => 59,
        _ => return None,
    })
}

fn quality_ly(q: &str) -> &'static str {
    match q {
        "maj" => "",
        "min" => ":m",
        "7" => ":7",
        "maj7" => ":maj7",
        "m7" => ":m7",
        "dim" => ":dim",
        "dim7" => ":dim7",
        "m7b5" => ":m7.5-",
        "aug" => ":aug",
        "sus2" => ":sus2",
        "sus4" => ":sus4",
        "add9" => ":5.9",
        "6" => ":6",
        "m6" => ":m6",
        "9" => ":9",
        _ => "",
    }
}

fn spelled_ly(s: Spelled) -> String {
    ly_pitch(Pitch { step: s.step, alter: s.alter, octave: 3 })
}

/// Serialize as LilyPond source.
pub fn write(score: &Score) -> String {
    let mut s = String::new();
    s.push_str("\\version \"2.24.0\"\n\n");
    s.push_str("\\header {\n");
    if let Some(t) = &score.title {
        let _ = writeln!(s, "  title = \"{}\"", t.replace('"', "'"));
    }
    if let Some(c) = &score.composer {
        let _ = writeln!(s, "  composer = \"{}\"", c.replace('"', "'"));
    }
    s.push_str("  tagline = ##f\n}\n\n");
    s.push_str("\\score {\n  <<\n");
    for p in &score.parts {
        // chord names above the first staff of the part
        let st0 = &p.staves[0];
        if st0.measures.iter().any(|m| !m.harmonies.is_empty()) {
            s.push_str("    \\new ChordNames \\chordmode {\n      ");
            s.push_str(&chord_line(score, st0));
            s.push_str("\n    }\n");
        }
        if p.percussion {
            let _ = writeln!(s, "    \\new DrumStaff \\with {{ instrumentName = \"{}\" }} <<", p.name.replace('"', "'"));
            write_voices(&mut s, score, st0, true, "      ");
            s.push_str("    >>\n");
            continue;
        }
        if p.staves.len() > 1 {
            let _ = writeln!(s, "    \\new PianoStaff \\with {{ instrumentName = \"{}\" }} <<", p.name.replace('"', "'"));
            for st in &p.staves {
                let _ = writeln!(s, "      \\new Staff = \"{}{}\" <<", p.id, st.number);
                write_voices(&mut s, score, st, false, "        ");
                s.push_str("      >>\n");
            }
            s.push_str("    >>\n");
        } else {
            let _ = writeln!(s, "    \\new Staff \\with {{ instrumentName = \"{}\" }} <<", p.name.replace('"', "'"));
            write_voices(&mut s, score, st0, false, "      ");
            s.push_str("    >>\n");
        }
        // lyrics
        let mut syl = Vec::new();
        for m in &st0.measures {
            if let Some(v) = m.voices.iter().find(|v| v.number == 1) {
                for e in &v.events {
                    if e.is_rest() || e.notes.iter().all(|n| n.tie_stop) {
                        continue;
                    }
                    match e.lyrics.first() {
                        Some(l) => {
                            let t = if l.text.chars().all(|c| c.is_alphanumeric()) { l.text.clone() } else { format!("\"{}\"", l.text.replace('"', "'")) };
                            syl.push(t);
                            if l.syllabic == "begin" || l.syllabic == "middle" {
                                syl.push("--".into());
                            }
                        }
                        None => syl.push("_".into()),
                    }
                }
            }
        }
        if syl.iter().any(|x| x != "_" && x != "--") {
            let _ = writeln!(s, "    \\new Lyrics \\lyricsto \"{}-1-v1\" {{ {} }}", p.id, syl.join(" "));
        }
    }
    s.push_str("  >>\n  \\layout { }\n}\n");
    s
}

fn chord_line(score: &Score, st: &Staff) -> String {
    let div = score.divisions;
    let mut out = Vec::new();
    for (mi, m) in st.measures.iter().enumerate() {
        let len: u32 = m.voices.first().map(|v| v.events.iter().map(|e| e.duration).sum()).unwrap_or(0);
        let len = if len == 0 { Score::time_at(st, mi).measure_divisions(div) } else { len };
        if mi == 0 && m.implicit {
            out.push(format!("\\partial {}", dur_for(len, div, None)));
        }
        let mut hs: Vec<&Harmony> = m.harmonies.iter().collect();
        hs.sort_by_key(|h| h.offset);
        let mut pos = 0u32;
        let mut cur: Option<String> = None;
        let mut bounds: Vec<(u32, Option<String>)> = Vec::new();
        for h in hs {
            if h.offset > pos {
                bounds.push((h.offset - pos, cur.clone()));
                pos = h.offset;
            }
            let mut c = format!("{}{}", spelled_ly(h.root), "");
            let q = quality_ly(&h.quality);
            let dur_marker = "{D}";
            c = format!("{c}{dur_marker}{q}");
            if let Some(b) = h.bass {
                c.push('/');
                c.push_str(&spelled_ly(b));
            }
            cur = Some(c);
        }
        if len > pos {
            bounds.push((len - pos, cur.clone()));
        }
        for (d, c) in bounds {
            let ds = dur_for(d, div, None);
            match c {
                Some(c) => out.push(c.replace("{D}", &ds)),
                None => out.push(format!("s{ds}")),
            }
        }
        out.push("|".into());
    }
    out.join(" ")
}

fn write_voices(s: &mut String, score: &Score, st: &Staff, drums: bool, ind: &str) {
    let two = st.measures.iter().any(|m| m.voices.len() > 1);
    let nv = if two { 2 } else { 1 };
    let part_id = "";
    let _ = part_id;
    for vn in 1..=nv {
        let name = format!("{}-{}-v{}", "P", st.number, vn);
        let _ = name;
        let mode = if drums { "\\drummode" } else { "" };
        let voice_cmd = if drums { "DrumVoice" } else { "Voice" };
        let _ = writeln!(s, "{ind}\\new {voice_cmd} = \"{}\" {mode} {{", voice_id(score, st, vn));
        let mut body = String::new();
        if two {
            body.push_str(if vn == 1 { "\\voiceOne " } else { "\\voiceTwo " });
        }
        body.push_str(&voice_music(score, st, vn, drums, vn == 1));
        for line in wrap(&body, 90) {
            let _ = writeln!(s, "{ind}  {line}");
        }
        let _ = writeln!(s, "{ind}}}");
    }
}

fn voice_id(score: &Score, st: &Staff, vn: u32) -> String {
    // the lyrics refer to "<part>-<staff>-v<voice>"
    let pid = score
        .parts
        .iter()
        .find(|p| p.staves.iter().any(|x| std::ptr::eq(x, st)))
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "P1".into());
    format!("{pid}-{}-v{vn}", st.number)
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for tok in s.split(' ') {
        if !cur.is_empty() && cur.len() + tok.len() + 1 > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(tok);
        if tok == "|" {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn voice_music(score: &Score, st: &Staff, vn: u32, drums: bool, with_meta: bool) -> String {
    let div = score.divisions;
    let mut toks: Vec<String> = Vec::new();
    let mut cur_time: Option<TimeSig> = None;
    let mut cur_key: Option<KeySig> = None;
    for (mi, m) in st.measures.iter().enumerate() {
        let time = Score::time_at(st, mi);
        let key = Score::key_at(st, mi);
        if mi == 0 {
            if let Some(c) = &m.clef {
                if !drums {
                    toks.push(format!(
                        "\\clef {}",
                        match c.sign.as_str() {
                            "F" => "bass",
                            "C" => "alto",
                            _ => "treble",
                        }
                    ));
                }
            }
        }
        if cur_key != Some(key) && !drums {
            let t = crate::pitch::key_tonic(key);
            toks.push(format!(
                "\\key {} {}",
                spelled_ly(t).trim_end_matches(','),
                if key.mode == Mode::Minor { "\\minor" } else { "\\major" }
            ));
            cur_key = Some(key);
        }
        if cur_time != Some(time) {
            toks.push(format!("\\time {}/{}", time.beats, time.beat_type));
            cur_time = Some(time);
        }
        if with_meta {
            if let Some(t) = m.tempo.first() {
                toks.push(format!("\\tempo 4 = {}", t.bpm.round() as i64));
            }
        }
        let voice = m.voices.iter().find(|v| v.number == vn);
        let len: u32 = m
            .voices
            .iter()
            .map(|v| v.events.iter().map(|e| e.duration).sum::<u32>())
            .max()
            .unwrap_or(0);
        if mi == 0 && m.implicit {
            toks.push(format!("\\partial {}", dur_for(len, div, None)));
        }
        if with_meta {
            if m.repeat_start {
                toks.push("\\set Score.repeatCommands = #'(start-repeat)".into());
            }
            if let Some(e) = &m.ending {
                if e.starts() {
                    toks.push(format!("\\set Score.repeatCommands = #'((volta \"{}\"))", e.number));
                }
            }
        }
        match voice {
            None => {
                // spacer for an absent voice
                toks.push(format!("s{}", dur_for(len.max(1), div, None)));
            }
            Some(v) => {
                let mut in_tuplet = false;
                for e in &v.events {
                    if let Some(t) = e.tuplet {
                        if t.start && !in_tuplet {
                            toks.push(format!("\\tuplet {}/{} {{", t.actual, t.normal));
                            in_tuplet = true;
                        }
                    }
                    let tup = e.tuplet.map(|t| (t.actual, t.normal));
                    let d = if e.measure_rest {
                        dur_for(e.duration, div, None)
                    } else {
                        dur_for(e.duration, div, tup)
                    };
                    let mut tok = String::new();
                    if let Some(dy) = &e.dynamic {
                        let _ = dy;
                    }
                    if e.is_rest() {
                        if e.measure_rest && e.visible {
                            tok.push_str(&format!("R{d}"));
                        } else {
                            tok.push_str(&format!("{}{d}", if e.visible { "r" } else { "s" }));
                        }
                    } else if e.notes.len() == 1 {
                        let n = &e.notes[0];
                        let p = if drums { drum_name(n.midi).to_string() } else { ly_pitch(n.pitch) };
                        tok.push_str(&format!("{p}{d}"));
                        if n.tie_start {
                            tok.push('~');
                        }
                    } else {
                        tok.push('<');
                        let ps: Vec<String> = e
                            .notes
                            .iter()
                            .map(|n| {
                                let mut p = if drums { drum_name(n.midi).to_string() } else { ly_pitch(n.pitch) };
                                if n.tie_start {
                                    p.push('~');
                                }
                                p
                            })
                            .collect();
                        tok.push_str(&ps.join(" "));
                        tok.push('>');
                        tok.push_str(&d);
                    }
                    for a in &e.articulations {
                        tok.push_str(match a.as_str() {
                            "staccato" => "-.",
                            "tenuto" => "--",
                            "accent" => "->",
                            "marcato" => "-^",
                            "staccatissimo" => "-!",
                            "fermata" => "\\fermata",
                            _ => "",
                        });
                    }
                    if let Some(dy) = &e.dynamic {
                        tok.push('\\');
                        tok.push_str(dy);
                    }
                    for sl in &e.slurs {
                        tok.push_str(if sl == "start" { "(" } else { ")" });
                    }
                    toks.push(tok);
                    if let Some(t) = e.tuplet {
                        if t.stop && in_tuplet {
                            toks.push("}".into());
                            in_tuplet = false;
                        }
                    }
                }
                if in_tuplet {
                    toks.push("}".into());
                }
            }
        }
        if with_meta {
            if let Some(e) = &m.ending {
                if e.stops() {
                    toks.push("\\set Score.repeatCommands = #'((volta #f))".into());
                }
            }
            if m.repeat_end.is_some() {
                toks.push("\\set Score.repeatCommands = #'(end-repeat)".into());
            }
            if m.barline.as_deref() == Some("light-heavy") {
                toks.push("\\bar \"|.\"".into());
            } else if m.barline.as_deref() == Some("light-light") {
                toks.push("\\bar \"||\"".into());
            }
        }
        toks.push("|".into());
    }
    toks.join(" ")
}

// ------------------------------------------------------------------------------------
// Reader (subset)
// ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Cmd(String),
    Word(String),
    Str(String),
    Open,
    Close,
    ChordOpen,
    ChordClose,
    SimOpen,
    SimClose,
    Bar,
    Other(char),
}

fn tokenize(src: &str) -> Vec<Tok> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '%' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                while i + 1 < chars.len() && !(chars[i] == '%' && chars[i + 1] == '}') {
                    i += 1;
                }
                i += 2;
            } else {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '{' => {
                out.push(Tok::Open);
                i += 1;
            }
            '}' => {
                out.push(Tok::Close);
                i += 1;
            }
            '<' if i + 1 < chars.len() && chars[i + 1] == '<' => {
                out.push(Tok::SimOpen);
                i += 2;
            }
            '>' if i + 1 < chars.len() && chars[i + 1] == '>' => {
                out.push(Tok::SimClose);
                i += 2;
            }
            '<' => {
                out.push(Tok::ChordOpen);
                i += 1;
            }
            '>' => {
                out.push(Tok::ChordClose);
                i += 1;
            }
            '|' => {
                out.push(Tok::Bar);
                i += 1;
            }
            '"' => {
                let mut j = i + 1;
                let mut s = String::new();
                while j < chars.len() && chars[j] != '"' {
                    s.push(chars[j]);
                    j += 1;
                }
                out.push(Tok::Str(s));
                i = j + 1;
            }
            '\\' => {
                let mut j = i + 1;
                let mut s = String::new();
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '.' && s.is_empty()) {
                    s.push(chars[j]);
                    j += 1;
                }
                if s.is_empty() && j < chars.len() {
                    s.push(chars[j]);
                    j += 1;
                }
                out.push(Tok::Cmd(s));
                i = j;
            }
            '#' => {
                // scheme expression: skip a balanced (...) or a single token
                let mut j = i + 1;
                if j < chars.len() && chars[j] == '\'' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == '(' {
                    let mut depth = 0;
                    while j < chars.len() {
                        if chars[j] == '(' {
                            depth += 1;
                        } else if chars[j] == ')' {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                        }
                        j += 1;
                    }
                } else {
                    while j < chars.len() && !chars[j].is_whitespace() && chars[j] != '}' {
                        j += 1;
                    }
                }
                let s: String = chars[i..j].iter().collect();
                out.push(Tok::Word(s));
                i = j;
            }
            c if c.is_alphanumeric() || c == '\'' || c == ',' || c == '.' || c == '~' || c == '*' || c == '/' || c == '-' || c == '_' || c == '(' || c == ')' || c == '^' || c == '!' || c == ':' || c == '=' => {
                let mut j = i;
                let mut s = String::new();
                while j < chars.len() {
                    let d = chars[j];
                    if d.is_alphanumeric() || matches!(d, '\'' | ',' | '.' | '~' | '*' | '/' | '-' | '_' | '(' | ')' | '^' | '!' | ':' | '=') {
                        s.push(d);
                        j += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Word(s));
                i = j;
            }
            other => {
                out.push(Tok::Other(other));
                i += 1;
            }
        }
    }
    out
}

struct LyNote {
    pitches: Vec<(Pitch, bool, i32)>,
    rest: bool,
    visible: bool,
    measure_rest: bool,
    /// Length in quarter notes as a rational (num, den).
    len: (u64, u64),
    tuplet: Option<(u32, u32)>,
    articulations: Vec<String>,
    dynamic: Option<String>,
}

struct LyVoice {
    notes: Vec<LyNote>,
    /// (note index, key, time, partial, repeat cmd)
    events: Vec<(usize, String)>,
}

fn parse_dur(s: &str) -> Option<((u64, u64), usize)> {
    // returns length in quarters and characters consumed
    let chars: Vec<char> = s.chars().collect();
    let mut j = 0;
    let mut num = String::new();
    while j < chars.len() && chars[j].is_ascii_digit() {
        num.push(chars[j]);
        j += 1;
    }
    if num.is_empty() {
        return None;
    }
    let d: u64 = num.parse().ok()?;
    if d == 0 {
        return None;
    }
    // quarters = 4/d
    let mut n = 4u64;
    let mut den = d;
    let mut dots = 0;
    while j < chars.len() && chars[j] == '.' {
        dots += 1;
        j += 1;
    }
    if dots > 0 {
        n *= (1 << (dots + 1)) - 1;
        den *= 1 << dots;
    }
    // *a/b scaling
    if j < chars.len() && chars[j] == '*' {
        j += 1;
        let mut a = String::new();
        while j < chars.len() && chars[j].is_ascii_digit() {
            a.push(chars[j]);
            j += 1;
        }
        let mut b = String::from("1");
        if j < chars.len() && chars[j] == '/' {
            j += 1;
            b.clear();
            while j < chars.len() && chars[j].is_ascii_digit() {
                b.push(chars[j]);
                j += 1;
            }
        }
        n *= a.parse::<u64>().unwrap_or(1);
        den *= b.parse::<u64>().unwrap_or(1);
    }
    let g = crate::pitch::gcd(n, den);
    Some(((n / g, den / g), j))
}

fn parse_ly_pitch(w: &str) -> Option<(Pitch, usize)> {
    let chars: Vec<char> = w.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let step = chars[0].to_ascii_uppercase();
    if !('A'..='G').contains(&step) || !chars[0].is_ascii_lowercase() {
        return None;
    }
    let rest: String = chars[1..].iter().collect();
    let (alter, used) = if rest.starts_with("isis") {
        (2, 4)
    } else if rest.starts_with("eses") {
        (-2, 4)
    } else if rest.starts_with("is") {
        (1, 2)
    } else if rest.starts_with("es") {
        (-1, 2)
    } else if rest.starts_with('s') && (step == 'E' || step == 'A') {
        (-1, 1)
    } else {
        (0, 0)
    };
    let mut j = 1 + used;
    let mut octave = 3i8;
    while j < chars.len() && (chars[j] == '\'' || chars[j] == ',') {
        octave += if chars[j] == '\'' { 1 } else { -1 };
        j += 1;
    }
    Some((Pitch { step, alter, octave }, j))
}

/// Read LilyPond source written by [`write`] (and simple hand-written files in absolute mode).
pub fn read(src: &str) -> Result<Score> {
    let toks = tokenize(src);
    let mut title = None;
    let mut composer = None;
    // header
    for (i, t) in toks.iter().enumerate() {
        if let Tok::Word(w) = t {
            if (w == "title" || w == "composer") && matches!(toks.get(i + 1), Some(Tok::Word(x)) if x == "=") {
                if let Some(Tok::Str(s)) = toks.get(i + 2) {
                    if w == "title" {
                        title = Some(s.clone());
                    } else {
                        composer = Some(s.clone());
                    }
                }
            }
        }
    }
    if toks.iter().any(|t| matches!(t, Tok::Cmd(c) if c == "relative")) {
        return Err(Error::unsupported("LilyPond \\relative mode is not supported by the reader; write absolute pitches"));
    }
    // staves: \new Staff / DrumStaff, voices: \new Voice / DrumVoice
    struct StaffIn {
        drums: bool,
        name: String,
        voices: Vec<LyVoice>,
        group: usize,
    }
    let mut staves: Vec<StaffIn> = Vec::new();
    let mut i = 0;
    let mut group = 0usize;
    let mut in_piano = false;
    while i < toks.len() {
        match &toks[i] {
            Tok::Cmd(c) if c == "new" => {
                let kind = match toks.get(i + 1) {
                    Some(Tok::Word(w)) => w.clone(),
                    _ => String::new(),
                };
                match kind.as_str() {
                    "PianoStaff" | "GrandStaff" => {
                        in_piano = true;
                        group += 1;
                        i += 2;
                        continue;
                    }
                    "Staff" | "DrumStaff" => {
                        let mut name = String::new();
                        let mut j = i + 2;
                        // optional = "id"
                        if matches!(toks.get(j), Some(Tok::Word(w)) if w == "=") {
                            j += 2;
                        }
                        // optional \with { instrumentName = "..." }
                        if matches!(toks.get(j), Some(Tok::Cmd(c)) if c == "with") {
                            j += 1;
                            let mut depth = 0;
                            while j < toks.len() {
                                match &toks[j] {
                                    Tok::Open => depth += 1,
                                    Tok::Close => {
                                        depth -= 1;
                                        if depth == 0 {
                                            j += 1;
                                            break;
                                        }
                                    }
                                    Tok::Word(w) if w == "instrumentName" => {
                                        if let Some(Tok::Str(s)) = toks.get(j + 2) {
                                            name = s.clone();
                                        }
                                    }
                                    _ => {}
                                }
                                j += 1;
                            }
                        }
                        if matches!(toks.get(j), Some(Tok::Cmd(c)) if c == "drummode") {
                            j += 1;
                        }
                        if !in_piano {
                            group += 1;
                        }
                        staves.push(StaffIn { drums: kind == "DrumStaff", name, voices: Vec::new(), group });
                        if toks.get(j) == Some(&Tok::Open) {
                            // music directly in the staff (a single voice)
                            let (v, end) = parse_music(&toks, j)?;
                            staves.last_mut().unwrap().voices.push(v);
                            i = end;
                        } else {
                            i = j;
                        }
                        continue;
                    }
                    "Voice" | "DrumVoice" => {
                        // find the music block
                        let mut j = i + 2;
                        while j < toks.len() && toks[j] != Tok::Open {
                            j += 1;
                        }
                        let (v, end) = parse_music(&toks, j)?;
                        if staves.is_empty() {
                            group += 1;
                            staves.push(StaffIn { drums: kind == "DrumVoice", name: String::new(), voices: Vec::new(), group });
                        }
                        staves.last_mut().unwrap().voices.push(v);
                        i = end;
                        continue;
                    }
                    "ChordNames" | "Lyrics" | "FretBoards" => {
                        // skip the block
                        let mut j = i + 2;
                        while j < toks.len() && toks[j] != Tok::Open {
                            j += 1;
                        }
                        let mut depth = 0;
                        while j < toks.len() {
                            match toks[j] {
                                Tok::Open => depth += 1,
                                Tok::Close => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            j += 1;
                        }
                        i = j + 1;
                        continue;
                    }
                    _ => {}
                }
                i += 1;
            }
            Tok::SimClose => {
                if in_piano && staves.last().map(|s| s.group) == Some(group) {
                    // closing a staff or the piano staff; heuristically end piano when two staves seen
                    let n = staves.iter().filter(|s| s.group == group).count();
                    if n >= 2 {
                        in_piano = false;
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    // a bare music block { ... } with no \new Staff
    if staves.is_empty() {
        if let Some(j) = toks.iter().position(|t| *t == Tok::Open) {
            let start = toks.iter().enumerate().skip(j).find(|(_, t)| matches!(t, Tok::Word(w) if parse_ly_pitch(w).is_some() || w.starts_with('r'))).map(|_| j);
            if let Some(j) = start {
                let (v, _) = parse_music(&toks, j)?;
                staves.push(StaffIn { drums: false, name: String::new(), voices: vec![v], group: 1 });
            }
        }
    }
    if staves.is_empty() {
        return Err(Error::input("no LilyPond music found"));
    }
    // rational → divisions
    let mut den = 4u64;
    for s in &staves {
        for v in &s.voices {
            for n in &v.notes {
                den = lcm(den, n.len.1);
            }
        }
    }
    let div = den as u32;
    let mut score = Score { title, composer, divisions: div, ..Score::default() };
    let mut groups: Vec<usize> = staves.iter().map(|s| s.group).collect();
    groups.dedup();
    for (pi, g) in groups.iter().enumerate() {
        let members: Vec<&StaffIn> = staves.iter().filter(|s| s.group == *g).collect();
        let drums = members.iter().any(|s| s.drums);
        let mut part = Part {
            id: format!("P{}", pi + 1),
            name: members.iter().map(|s| s.name.clone()).find(|n| !n.is_empty()).unwrap_or_else(|| if drums { "Drums".into() } else { format!("Part {}", pi + 1) }),
            abbreviation: None,
            program: if drums { None } else { Some(0) },
            channel: if drums { Some(10) } else { None },
            percussion: drums,
            staves: Vec::new(),
        };
        for (si, st) in members.iter().enumerate() {
            part.staves.push(build_staff(st.voices.as_slice(), div, si as u32 + 1, drums));
        }
        score.parts.push(part);
    }
    Ok(score)
}

fn build_staff(voices: &[LyVoice], div: u32, number: u32, drums: bool) -> Staff {
    // meta from the first voice
    let mut time = TimeSig::new(4, 4);
    let mut key = KeySig::major(0);
    let mut clef = if drums { Clef::percussion() } else { Clef::treble() };
    let mut partial: Option<u64> = None;
    let mut tempo: Option<f64> = None;
    let mut meta_at: Vec<(usize, String)> = Vec::new();
    if let Some(v) = voices.first() {
        meta_at = v.events.clone();
    }
    for (idx, m) in &meta_at {
        if *idx > 0 {
            continue;
        }
        if let Some(t) = m.strip_prefix("time ") {
            if let Some((a, b)) = t.split_once('/') {
                time = TimeSig::new(a.parse().unwrap_or(4), b.parse().unwrap_or(4));
            }
        } else if let Some(k) = m.strip_prefix("key ") {
            if let Some(kk) = crate::pitch::parse_key(k) {
                key = kk;
            }
        } else if let Some(c) = m.strip_prefix("clef ") {
            clef = match c {
                "bass" => Clef::bass(),
                "alto" => Clef { sign: "C".into(), line: 3, octave_change: 0 },
                _ => Clef::treble(),
            };
        } else if let Some(p) = m.strip_prefix("partialq ") {
            if let Some((a, b)) = p.split_once('/') {
                let (a, b): (u64, u64) = (a.parse().unwrap_or(1), b.parse().unwrap_or(1));
                partial = Some(a * div as u64 / b.max(1));
            }
        } else if let Some(t) = m.strip_prefix("tempo ") {
            tempo = t.parse().ok();
        }
    }
    let mlen = time.measure_divisions(div) as u64;
    let mut measures: Vec<Measure> = Vec::new();
    for (vi, v) in voices.iter().enumerate() {
        let mut mi = 0usize;
        let mut pos = 0u64;
        let cap = |mi: usize| -> u64 {
            if mi == 0 {
                partial.unwrap_or(mlen)
            } else {
                mlen
            }
        };
        let mut cur: Vec<Event> = Vec::new();
        let flush = |mi: usize, cur: &mut Vec<Event>, measures: &mut Vec<Measure>| {
            while measures.len() <= mi {
                let n = measures.len();
                measures.push(Measure {
                    number: if partial.is_some() { n as u32 } else { n as u32 + 1 },
                    implicit: n == 0 && partial.is_some(),
                    ..Measure::default()
                });
            }
            if cur.iter().all(|e| e.is_rest() && !e.visible) && vi > 0 {
                cur.clear();
                return;
            }
            measures[mi].voices.push(Voice { number: vi as u32 + 1, events: std::mem::take(cur) });
        };
        for (ni, n) in v.notes.iter().enumerate() {
            let _ = ni;
            let mut d = n.len.0 * div as u64 / n.len.1;
            let mut first = true;
            while d > 0 {
                let room = cap(mi) - pos;
                let take = d.min(room);
                let mut ev = if n.rest {
                    let mut e = Event::rest(take as u32);
                    e.visible = n.visible;
                    e.measure_rest = n.measure_rest && take == cap(mi);
                    e
                } else {
                    let notes = n
                        .pitches
                        .iter()
                        .map(|(p, tie, dk)| {
                            let mut nt = Note::new(*p);
                            if drums {
                                nt.midi = *dk;
                                nt.unpitched = true;
                                let sp = crate::drums::spec(*dk as u8);
                                nt.pitch = crate::drums::display_pitch(sp.key);
                                nt.notehead = sp.notehead.map(|h| h.to_string());
                                nt.instrument = Some(sp.name.to_string());
                            }
                            nt.tie_start = *tie || take < d;
                            nt.tie_stop = !first;
                            nt
                        })
                        .collect();
                    Event::notes(take as u32, notes)
                };
                if first {
                    ev.articulations = n.articulations.clone();
                    ev.dynamic = n.dynamic.clone();
                }
                if let Some((a, b)) = n.tuplet {
                    ev.tuplet = Some(Tuplet { actual: a, normal: b, start: false, stop: false });
                }
                cur.push(ev);
                pos += take;
                d -= take;
                first = false;
                if pos == cap(mi) {
                    flush(mi, &mut cur, &mut measures);
                    mi += 1;
                    pos = 0;
                }
            }
        }
        if !cur.is_empty() {
            flush(mi, &mut cur, &mut measures);
        }
    }
    // tie_stop from previous ties (across events)
    for m in measures.iter_mut() {
        for v in m.voices.iter_mut() {
            let n = v.events.len();
            for i in 0..n {
                if let Some(t) = v.events[i].tuplet {
                    let prev = i > 0 && v.events[i - 1].tuplet.is_some();
                    let next = i + 1 < n && v.events[i + 1].tuplet.is_some();
                    v.events[i].tuplet = Some(Tuplet { start: !prev, stop: !next, ..t });
                }
            }
        }
    }
    let mut open: Vec<i32> = Vec::new();
    for m in measures.iter_mut() {
        if let Some(v) = m.voices.first_mut() {
            for e in v.events.iter_mut() {
                for nt in e.notes.iter_mut() {
                    if open.contains(&nt.midi) {
                        nt.tie_stop = true;
                    }
                }
                if !e.is_rest() {
                    open = e.notes.iter().filter(|n| n.tie_start).map(|n| n.midi).collect();
                } else {
                    open.clear();
                }
            }
        }
    }
    // repeat commands and barlines from the first voice: (note index, command)
    if let Some(v) = voices.first() {
        let mut starts: Vec<u64> = Vec::new();
        let mut acc = 0u64;
        for n in &v.notes {
            starts.push(acc);
            acc += n.len.0 * div as u64 / n.len.1;
        }
        starts.push(acc);
        let measure_of = |pos: u64| -> usize {
            let p0 = partial.unwrap_or(mlen);
            if pos < p0 {
                0
            } else {
                1 + ((pos - p0) / mlen) as usize
            }
        };
        let mut open_end: Option<String> = None;
        for (idx, cmd) in &v.events {
            let pos = starts.get(*idx).copied().unwrap_or(acc);
            // commands after the last note of a measure apply to that measure
            let mi_next = measure_of(pos).min(measures.len().saturating_sub(1));
            let mi_prev = if pos > 0 { measure_of(pos - 1).min(measures.len().saturating_sub(1)) } else { 0 };
            if cmd == "start-repeat" {
                measures[mi_next].repeat_start = true;
            } else if cmd == "end-repeat" {
                measures[mi_prev].repeat_end = Some(2);
            } else if let Some(num) = cmd.strip_prefix("volta ") {
                if num == "#f" {
                    let has_rep = measures[mi_prev].repeat_end.is_some();
                    if let Some(e) = measures[mi_prev].ending.as_mut() {
                        let ty = if has_rep { "stop" } else { "discontinue" };
                        e.kind = if e.kind == "start" { format!("start-{ty}") } else { ty.to_string() };
                    }
                    open_end = None;
                } else {
                    open_end = Some(num.to_string());
                    measures[mi_next].ending = Some(Ending { number: num.to_string(), kind: "start".into() });
                }
            } else if let Some(b) = cmd.strip_prefix("bar ") {
                if b == "|." {
                    measures[mi_prev].barline = Some("light-heavy".into());
                } else if b == "||" {
                    measures[mi_prev].barline = Some("light-light".into());
                }
            }
            let _ = &open_end;
        }
        // fix end-repeat kinds set before the repeat flag was known
        for m in measures.iter_mut() {
            if let (Some(e), Some(_)) = (m.ending.as_mut(), m.repeat_end) {
                if e.kind == "discontinue" {
                    e.kind = "stop".into();
                } else if e.kind == "start-discontinue" {
                    e.kind = "start-stop".into();
                }
            }
        }
        // middle measures of multi-measure endings
        let mut open: Option<String> = None;
        for m in measures.iter_mut() {
            match &m.ending {
                Some(e) if e.starts() && !e.stops() => open = Some(e.number.clone()),
                Some(e) if e.stops() => open = None,
                None => {
                    if let Some(n) = &open {
                        m.ending = Some(Ending { number: n.clone(), kind: "continue".into() });
                    }
                }
                _ => {}
            }
        }
        // time/key changes after the first measure
        let mut last_t = time;
        let mut last_k = key;
        for (idx, cmd) in &v.events {
            if *idx == 0 {
                continue;
            }
            let pos = starts.get(*idx).copied().unwrap_or(acc);
            let mi = measure_of(pos).min(measures.len().saturating_sub(1));
            if let Some(t) = cmd.strip_prefix("time ") {
                if let Some((a, b)) = t.split_once('/') {
                    let ts = TimeSig::new(a.parse().unwrap_or(4), b.parse().unwrap_or(4));
                    if ts != last_t {
                        measures[mi].time = Some(ts);
                        last_t = ts;
                    }
                }
            } else if let Some(k) = cmd.strip_prefix("key ") {
                if let Some(kk) = crate::pitch::parse_key(k) {
                    if kk != last_k {
                        measures[mi].key = Some(kk);
                        last_k = kk;
                    }
                }
            }
        }
    }
    for m in measures.iter_mut() {
        if m.voices.is_empty() {
            let mut r = Event::rest(mlen as u32);
            r.measure_rest = true;
            m.voices.push(Voice { number: 1, events: vec![r] });
        }
        // renumber voices densely
        for (i, v) in m.voices.iter_mut().enumerate() {
            v.number = i as u32 + 1;
        }
    }
    if let Some(m0) = measures.first_mut() {
        m0.time = Some(time);
        if !drums {
            m0.key = Some(key);
        }
        m0.clef = Some(clef);
        if let Some(t) = tempo {
            m0.tempo.push(TempoMark { offset: 0, bpm: t, text: None });
        }
    }
    Staff { number, measures }
}

fn parse_music(toks: &[Tok], start: usize) -> Result<(LyVoice, usize)> {
    let mut v = LyVoice { notes: Vec::new(), events: Vec::new() };
    let mut i = start;
    let mut depth = 0i32;
    let mut last_len = (1u64, 1u64);
    let mut tuplets: Vec<(u32, u32, i32)> = Vec::new();
    while i < toks.len() {
        match &toks[i] {
            Tok::Open => {
                depth += 1;
                i += 1;
            }
            Tok::Close => {
                depth -= 1;
                if let Some(t) = tuplets.last() {
                    if t.2 == depth + 1 {
                        tuplets.pop();
                    }
                }
                i += 1;
                if depth <= 0 {
                    break;
                }
            }
            Tok::Cmd(c) => {
                let c = c.as_str();
                match c {
                    "time" => {
                        if let Some(Tok::Word(w)) = toks.get(i + 1) {
                            v.events.push((v.notes.len(), format!("time {w}")));
                        }
                        i += 2;
                    }
                    "key" => {
                        if let (Some(Tok::Word(w)), Some(Tok::Cmd(m))) = (toks.get(i + 1), toks.get(i + 2)) {
                            let (p, _) = parse_ly_pitch(w).unwrap_or((Pitch { step: 'C', alter: 0, octave: 3 }, 0));
                            let name = Spelled { step: p.step, alter: p.alter }.name();
                            v.events.push((v.notes.len(), format!("key {name}{}", if m == "minor" { "m" } else { "" })));
                        }
                        i += 3;
                    }
                    "clef" => {
                        let w = match toks.get(i + 1) {
                            Some(Tok::Word(w)) | Some(Tok::Str(w)) => w.clone(),
                            _ => "treble".into(),
                        };
                        v.events.push((v.notes.len(), format!("clef {w}")));
                        i += 2;
                    }
                    "partial" => {
                        if let Some(Tok::Word(w)) = toks.get(i + 1) {
                            if let Some((len, _)) = parse_dur(w) {
                                // in divisions later: store as quarters*1000/den is lossy; use (num/den)*48
                                v.events.push((v.notes.len(), format!("partialq {}/{}", len.0, len.1)));
                            }
                        }
                        i += 2;
                    }
                    "tempo" => {
                        // \tempo 4 = 120
                        let mut j = i + 1;
                        let mut bpm = None;
                        while j < toks.len() && j < i + 5 {
                            if let Tok::Word(w) = &toks[j] {
                                if let Some(b) = w.strip_prefix('=') {
                                    if !b.is_empty() {
                                        bpm = b.parse::<f64>().ok();
                                    }
                                } else if w != "4" && w != "=" {
                                    if let Ok(b) = w.parse::<f64>() {
                                        bpm = Some(b);
                                    }
                                }
                            }
                            j += 1;
                            if bpm.is_some() {
                                break;
                            }
                        }
                        if let Some(b) = bpm {
                            v.events.push((v.notes.len(), format!("tempo {b}")));
                        }
                        i = j;
                    }
                    "tuplet" | "times" => {
                        if let Some(Tok::Word(w)) = toks.get(i + 1) {
                            if let Some((a, b)) = w.split_once('/') {
                                let (a, b): (u32, u32) = (a.parse().unwrap_or(3), b.parse().unwrap_or(2));
                                let (a, b) = if c == "times" { (b, a) } else { (a, b) };
                                tuplets.push((a, b, depth + 1));
                            }
                        }
                        i += 2;
                    }
                    "set" => {
                        // \set Score.repeatCommands = #'(...)
                        let mut j = i + 1;
                        while j < toks.len() && j < i + 5 {
                            if let Tok::Word(w) = &toks[j] {
                                if w.starts_with("#'") {
                                    if w.contains("start-repeat") {
                                        v.events.push((v.notes.len(), "start-repeat".into()));
                                    } else if w.contains("end-repeat") {
                                        v.events.push((v.notes.len(), "end-repeat".into()));
                                    } else if w.contains("volta #f") {
                                        v.events.push((v.notes.len(), "volta #f".into()));
                                    }
                                    break;
                                }
                            }
                            if let Tok::Str(s) = &toks[j] {
                                v.events.push((v.notes.len(), format!("volta {s}")));
                                // skip the rest of the scheme list
                                j += 1;
                                break;
                            }
                            j += 1;
                        }
                        i = j + 1;
                    }
                    "bar" => {
                        if let Some(Tok::Str(s)) = toks.get(i + 1) {
                            v.events.push((v.notes.len(), format!("bar {s}")));
                        }
                        i += 2;
                    }
                    "voiceOne" | "voiceTwo" | "drummode" | "fermata" | "oneVoice" => {
                        if c == "fermata" {
                            if let Some(n) = v.notes.last_mut() {
                                n.articulations.push("fermata".into());
                            }
                        }
                        i += 1;
                    }
                    d if matches!(d, "p" | "pp" | "ppp" | "mp" | "mf" | "f" | "ff" | "fff" | "sfz" | "fp") => {
                        if let Some(n) = v.notes.last_mut() {
                            n.dynamic = Some(d.to_string());
                        }
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            Tok::ChordOpen => {
                let mut j = i + 1;
                let mut ps = Vec::new();
                while j < toks.len() && toks[j] != Tok::ChordClose {
                    if let Tok::Word(w) = &toks[j] {
                        let tie = w.ends_with('~');
                        let w2 = w.trim_end_matches('~');
                        if let Some((p, _)) = parse_ly_pitch(w2) {
                            ps.push((p, tie, p.midi()));
                        } else if let Some(k) = drum_key(w2) {
                            ps.push((crate::drums::display_pitch(k as u8), tie, k));
                        }
                    }
                    j += 1;
                }
                j += 1;
                let mut len = last_len;
                let mut arts = Vec::new();
                let mut all_tie = false;
                if let Some(Tok::Word(w)) = toks.get(j) {
                    if let Some((l, used)) = parse_dur(w) {
                        len = l;
                        let rest = &w[used..];
                        all_tie = rest.contains('~');
                        arts = parse_arts(rest);
                        j += 1;
                    } else if w.starts_with('~') || w.starts_with('-') {
                        all_tie = w.contains('~');
                        arts = parse_arts(w);
                        j += 1;
                    }
                }
                if all_tie {
                    for p in ps.iter_mut() {
                        p.1 = true;
                    }
                }
                last_len = len;
                let tup = tuplets.last().map(|t| (t.0, t.1));
                let len = match tup {
                    Some((a, b)) => mulq(len, b as u64, a as u64),
                    None => len,
                };
                v.notes.push(LyNote { pitches: ps, rest: false, visible: true, measure_rest: false, len, tuplet: tup, articulations: arts, dynamic: None });
                i = j;
            }
            Tok::Word(w) => {
                let w = w.as_str();
                let tup = tuplets.last().map(|t| (t.0, t.1));
                if let Some(rest) = w.strip_prefix(['r', 's', 'R']) {
                    let first = w.chars().next().unwrap();
                    if rest.is_empty() || rest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        let (len, _) = parse_dur(rest).unwrap_or((last_len, 0));
                        last_len = len;
                        let l = match tup {
                            Some((a, b)) if first != 'R' => mulq(len, b as u64, a as u64),
                            _ => len,
                        };
                        v.notes.push(LyNote {
                            pitches: vec![],
                            rest: true,
                            visible: first != 's',
                            measure_rest: first == 'R',
                            len: l,
                            tuplet: if first == 'R' { None } else { tup },
                            articulations: vec![],
                            dynamic: None,
                        });
                        i += 1;
                        continue;
                    }
                }
                let (pitch, used, dk) = if let Some((p, used)) = parse_ly_pitch(w) {
                    (Some(p), used, p.midi())
                } else {
                    // drum names
                    let name: String = w.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
                    match drum_key(&name) {
                        Some(k) => (Some(crate::drums::display_pitch(k as u8)), name.len(), k),
                        None => (None, 0, 0),
                    }
                };
                if let Some(p) = pitch {
                    let rest = &w[used..];
                    let (len, used2) = parse_dur(rest).unwrap_or((last_len, 0));
                    last_len = len;
                    let after = &rest[used2..];
                    let tie = after.contains('~');
                    let arts = parse_arts(after);
                    let l = match tup {
                        Some((a, b)) => mulq(len, b as u64, a as u64),
                        None => len,
                    };
                    v.notes.push(LyNote { pitches: vec![(p, tie, dk)], rest: false, visible: true, measure_rest: false, len: l, tuplet: tup, articulations: arts, dynamic: None });
                } else if w.starts_with('~') {
                    if let Some(n) = v.notes.last_mut() {
                        for p in n.pitches.iter_mut() {
                            p.1 = true;
                        }
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok((v, i))
}

fn mulq(l: (u64, u64), n: u64, d: u64) -> (u64, u64) {
    let a = l.0 * n;
    let b = l.1 * d;
    let g = crate::pitch::gcd(a, b);
    (a / g, b / g)
}

fn parse_arts(s: &str) -> Vec<String> {
    let mut v = Vec::new();
    if s.contains("-.") {
        v.push("staccato".into());
    }
    if s.contains("--") {
        v.push("tenuto".into());
    }
    if s.contains("->") {
        v.push("accent".into());
    }
    if s.contains("-^") {
        v.push("marcato".into());
    }
    if s.contains("-!") {
        v.push("staccatissimo".into());
    }
    v
}

#[allow(dead_code)]
fn _types() -> usize {
    TYPES.len() + chords::QUALITIES.len()
}
