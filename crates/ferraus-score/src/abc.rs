//! ABC notation writer and a basic reader (headers, notes, rests, chords, ties,
//! triplets, broken rhythm, bars and repeats, chord symbols, dynamics, lyrics, voices).

use crate::chords;
use crate::model::*;
use crate::musicxml::key_alter;
use crate::pitch::{gcd, key_tonic, lcm, spell_midi, spelled_lof};
use ferraus_core::{Error, Result};
use std::collections::HashMap;
use std::fmt::Write as _;

fn frac(num: u64, den: u64) -> (u64, u64) {
    let g = gcd(num, den).max(1);
    (num / g, den / g)
}

/// ABC length suffix for `num/den` unit lengths.
fn len_suffix(num: u64, den: u64) -> String {
    let (n, d) = frac(num, den);
    match (n, d) {
        (1, 1) => String::new(),
        (n, 1) => n.to_string(),
        (1, 2) => "/".into(),
        (1, d) => format!("/{d}"),
        (n, d) => format!("{n}/{d}"),
    }
}

fn abc_pitch(p: Pitch) -> String {
    let mut s = String::new();
    if p.octave >= 5 {
        s.push(p.step.to_ascii_lowercase());
        for _ in 5..p.octave {
            s.push('\'');
        }
    } else {
        s.push(p.step);
        for _ in p.octave..4 {
            s.push(',');
        }
    }
    s
}

fn abc_acc(alter: i8) -> &'static str {
    match alter {
        2 => "^^",
        1 => "^",
        -1 => "_",
        -2 => "__",
        _ => "=",
    }
}

fn key_field(k: KeySig) -> String {
    let t = key_tonic(k);
    format!("{}{}", t.name(), if k.mode == Mode::Minor { "m" } else { "" })
}

fn clef_name(c: &Clef) -> &'static str {
    match c.sign.as_str() {
        "F" => "bass",
        "C" => "alto",
        "percussion" => "perc",
        _ => "treble",
    }
}

/// Serialize as ABC 2.1.
pub fn write(score: &Score) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "X:1");
    let _ = writeln!(s, "T:{}", score.title.clone().unwrap_or_else(|| "Untitled".into()));
    if let Some(c) = &score.composer {
        let _ = writeln!(s, "C:{c}");
    }
    let first = score.parts.first().and_then(|p| p.staves.first());
    let time = first.map(|st| Score::time_at(st, 0)).unwrap_or(TimeSig::new(4, 4));
    let key = score
        .parts
        .iter()
        .find(|p| !p.percussion)
        .and_then(|p| p.staves.first())
        .map(|st| Score::key_at(st, 0))
        .unwrap_or(KeySig::major(0));
    let _ = writeln!(s, "M:{}/{}", time.beats, time.beat_type);
    let _ = writeln!(s, "L:1/8");
    if let Some(t) = first.and_then(|st| st.measures.first()).and_then(|m| m.tempo.first()) {
        let _ = writeln!(s, "Q:1/4={}", t.bpm.round() as i64);
    }
    // voices
    let mut voices: Vec<(String, &Part, &Staff)> = Vec::new();
    for p in &score.parts {
        for st in &p.staves {
            voices.push((format!("{}{}", p.id, if p.staves.len() > 1 { format!("S{}", st.number) } else { String::new() }), p, st));
        }
    }
    if voices.len() > 1 {
        let ids: Vec<String> = voices.iter().map(|v| v.0.clone()).collect();
        let _ = writeln!(s, "%%score {}", ids.join(" "));
    }
    for (id, p, st) in &voices {
        if voices.len() > 1 || p.percussion {
            let clef = st.measures.first().and_then(|m| m.clef.clone()).unwrap_or_else(Clef::treble);
            let _ = writeln!(s, "V:{id} clef={} name=\"{}\"", clef_name(&clef), p.name.replace('"', "'"));
        }
    }
    let _ = writeln!(s, "K:{}", key_field(key));
    for (id, p, st) in &voices {
        if voices.len() > 1 || p.percussion {
            let _ = writeln!(s, "V:{id}");
        }
        if p.percussion {
            s.push_str("%%MIDI channel 10\n");
        }
        write_staff(&mut s, score, st, p.percussion);
    }
    s
}

fn write_staff(s: &mut String, score: &Score, st: &Staff, perc: bool) {
    let div = score.divisions as u64;
    let unit = div / 2; // L:1/8 in divisions (may be fractional; handled by fractions)
    let _ = unit;
    let mut line = String::new();
    let mut lyr_line: Vec<String> = Vec::new();
    let mut bars_in_line = 0;
    let mut cur_time = Score::time_at(st, 0);
    let mut cur_key = Score::key_at(st, 0);
    let n = st.measures.len();
    for (mi, m) in st.measures.iter().enumerate() {
        if mi > 0 {
            if let Some(t) = m.time {
                if t != cur_time {
                    let _ = write!(line, "[M:{}/{}] ", t.beats, t.beat_type);
                    cur_time = t;
                }
            }
            if let Some(k) = m.key {
                if k != cur_key {
                    let _ = write!(line, "[K:{}] ", key_field(k));
                    cur_key = k;
                }
            }
        }
        if m.repeat_start {
            line.push_str("|: ");
        }
        if let Some(e) = &m.ending {
            if e.starts() {
                let _ = write!(line, "[{} ", e.number.replace(' ', ""));
            }
        }
        let fifths = if perc { 0 } else { cur_key.fifths };
        let mut voices_txt: Vec<String> = Vec::new();
        for (vi, v) in m.voices.iter().enumerate() {
            let mut t = String::new();
            let mut acc: HashMap<(char, i8), i8> = HashMap::new();
            let mut pos = 0u64;
            let mut harm: Vec<&Harmony> = if vi == 0 { m.harmonies.iter().collect() } else { vec![] };
            harm.sort_by_key(|h| h.offset);
            let mut hi = 0;
            for e in &v.events {
                let end = pos + e.duration as u64;
                while hi < harm.len() && (harm[hi].offset as u64) < end.max(pos + 1) {
                    let _ = write!(t, "\"{}\"", harm[hi].text);
                    hi += 1;
                }
                if let Some(d) = &e.dynamic {
                    let _ = write!(t, "!{d}!");
                }
                for a in &e.articulations {
                    match a.as_str() {
                        "staccato" => t.push('.'),
                        "fermata" => t.push_str("!fermata!"),
                        "accent" => t.push_str("!accent!"),
                        "tenuto" => t.push_str("!tenuto!"),
                        "marcato" => t.push_str("!marcato!"),
                        _ => {}
                    }
                }
                if let Some(tp) = e.tuplet {
                    if tp.start {
                        let _ = write!(t, "({}", tp.actual);
                    }
                }
                // written length in L units (1/8 = div/2 divisions)
                let (an, ad) = match e.tuplet {
                    Some(tp) => (e.duration as u64 * tp.actual as u64, tp.normal as u64),
                    None => (e.duration as u64, 1),
                };
                let lsuf = len_suffix(an * 2, ad * div);
                if e.is_rest() {
                    let _ = write!(t, "{}{}", if e.visible { "z" } else { "x" }, lsuf);
                } else {
                    let chord = e.notes.len() > 1;
                    if chord {
                        t.push('[');
                    }
                    for nt in &e.notes {
                        let p = nt.pitch;
                        if !perc {
                            let k = (p.step, p.octave);
                            let cur = acc.get(&k).copied().unwrap_or_else(|| key_alter(fifths, p.step));
                            if nt.tie_stop {
                                acc.insert(k, p.alter);
                            } else if cur != p.alter {
                                t.push_str(abc_acc(p.alter));
                                acc.insert(k, p.alter);
                            }
                        }
                        t.push_str(&abc_pitch(p));
                        if chord {
                            if nt.tie_start {
                                t.push('-');
                            }
                        }
                    }
                    if chord {
                        t.push(']');
                    }
                    t.push_str(&lsuf);
                    if !chord && e.notes.iter().any(|n| n.tie_start) {
                        t.push('-');
                    }
                    if vi == 0 && !e.notes.iter().all(|n| n.tie_stop) {
                        lyr_line.push(match e.lyrics.first() {
                            Some(l) => match l.syllabic.as_str() {
                                "begin" | "middle" => format!("{}-", l.text.replace(' ', "~")),
                                _ => l.text.replace(' ', "~"),
                            },
                            None => "*".into(),
                        });
                    }
                }
                // break beams at beat boundaries for readability
                let beat = cur_time.beat_divisions(score.divisions) as u64;
                if beat > 0 && end % beat == 0 {
                    t.push(' ');
                }
                pos = end;
            }
            voices_txt.push(t.trim_end().to_string());
        }
        line.push_str(&voices_txt.join(" & "));
        // bar line
        let bar = if let Some(_t) = m.repeat_end {
            " :|"
        } else if mi + 1 == n || m.barline.as_deref() == Some("light-heavy") {
            " |]"
        } else if m.barline.as_deref() == Some("light-light") {
            " ||"
        } else {
            " |"
        };
        line.push_str(bar);
        line.push(' ');
        bars_in_line += 1;
        if bars_in_line == 4 || mi + 1 == n {
            s.push_str(line.trim_end());
            s.push('\n');
            if lyr_line.iter().any(|l| l != "*") {
                let _ = writeln!(s, "w:{}", lyr_line.join(" "));
            }
            line.clear();
            lyr_line.clear();
            bars_in_line = 0;
        }
    }
}

// ------------------------------------------------------------------------------------
// Reader
// ------------------------------------------------------------------------------------

/// Rational number of quarter notes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Q(u64, u64);

impl Q {
    fn add(self, o: Q) -> Q {
        let d = lcm(self.1, o.1);
        let (n, d) = frac(self.0 * (d / self.1) + o.0 * (d / o.1), d);
        Q(n, d)
    }
    fn mul(self, n: u64, d: u64) -> Q {
        let (a, b) = frac(self.0 * n, self.1 * d);
        Q(a, b)
    }
}

struct RNote {
    rest: bool,
    visible: bool,
    pitches: Vec<(Pitch, bool)>, // (pitch, tie_start)
    len: Q,
    tuplet: Option<(u32, u32)>,
    chord_symbol: Option<String>,
    dynamic: Option<String>,
    articulations: Vec<String>,
}

struct RMeasure {
    notes: Vec<RNote>,
    repeat_start: bool,
    repeat_end: bool,
    ending: Option<String>,
    final_bar: bool,
    time: Option<TimeSig>,
    key: Option<KeySig>,
    lyrics_start: usize,
}

fn parse_key_field(v: &str) -> Option<KeySig> {
    let v = v.split_whitespace().next().unwrap_or("");
    if v.is_empty() || v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("perc") {
        return Some(KeySig::major(0));
    }
    if let Some(k) = crate::pitch::parse_key(v) {
        return Some(k);
    }
    let lower = v.to_ascii_lowercase();
    let modes = [("ion", 0), ("dor", -2), ("phr", -4), ("lyd", 1), ("mix", -1), ("aeo", -3), ("loc", -5), ("maj", 0), ("min", -3)];
    for (m, off) in modes {
        if let Some(pos) = lower.find(m) {
            let root = &v[..pos];
            let mut chars = root.chars();
            let step = chars.next()?.to_ascii_uppercase();
            let mut alter = 0i8;
            for c in chars {
                match c {
                    '#' => alter += 1,
                    'b' => alter -= 1,
                    _ => {}
                }
            }
            let f = spelled_lof(Spelled { step, alter }) + off;
            if (-7..=7).contains(&f) {
                return Some(KeySig { fifths: f as i8, mode: if off == -3 { Mode::Minor } else { Mode::Major } });
            }
        }
    }
    None
}

fn parse_frac(v: &str) -> Option<(u64, u64)> {
    let (a, b) = v.trim().split_once('/')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Parse ABC text (the first tune).
pub fn read(text: &str) -> Result<Score> {
    let mut title = None;
    let mut composer = None;
    let mut time = TimeSig::new(4, 4);
    let mut unit = (1u64, 8u64);
    let mut unit_set = false;
    let mut tempo: Option<f64> = None;
    let mut key = KeySig::major(0);
    let mut in_body = false;
    // voice id → (clef, name, measures, lyrics)
    let mut voice_order: Vec<String> = Vec::new();
    let mut voices: HashMap<String, (Clef, String, Vec<RMeasure>, Vec<String>, bool)> = HashMap::new();
    let mut cur_voice = String::from("1");
    let mut seen_x = false;
    let new_measure = || RMeasure { notes: Vec::new(), repeat_start: false, repeat_end: false, ending: None, final_bar: false, time: None, key: None, lyrics_start: 0 };
    for raw_line in text.lines() {
        let line = match raw_line.find('%') {
            Some(i) if !raw_line.starts_with("%%") => &raw_line[..i],
            _ => raw_line,
        };
        let line = line.trim_end();
        if line.starts_with("%%") {
            if line.contains("MIDI channel 10") {
                if let Some(v) = voices.get_mut(&cur_voice) {
                    v.4 = true;
                }
            }
            continue;
        }
        if line.trim().is_empty() {
            if in_body && seen_x {
                // blank line ends the tune
                break;
            }
            continue;
        }
        let bytes = line.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() && !(in_body && (bytes[0] == b'|' )) {
            let field = bytes[0] as char;
            let val = line[2..].trim();
            match field {
                'X' => {
                    if seen_x && in_body {
                        break;
                    }
                    seen_x = true;
                }
                'T' => {
                    if title.is_none() {
                        title = Some(val.to_string())
                    }
                }
                'C' => composer = Some(val.to_string()),
                'M' => {
                    let t = match val {
                        "C" => Some((4, 4)),
                        "C|" => Some((2, 2)),
                        v => parse_frac(v),
                    };
                    if let Some((b, bt)) = t {
                        time = TimeSig::new(b as u32, bt as u32);
                        if !unit_set && !in_body {
                            unit = if (b as f64 / bt as f64) < 0.75 { (1, 16) } else { (1, 8) };
                        }
                        if in_body {
                            if let Some(v) = voices.get_mut(&cur_voice) {
                                if let Some(m) = v.2.last_mut() {
                                    m.time = Some(time);
                                }
                            }
                        }
                    }
                }
                'L' => {
                    if let Some(u) = parse_frac(val) {
                        unit = u;
                        unit_set = true;
                    }
                }
                'Q' => {
                    // Q:1/4=120 or Q:120
                    let bpm = val.rsplit('=').next().and_then(|x| x.trim().split_whitespace().next()?.parse::<f64>().ok());
                    let beat = val.split_once('=').and_then(|(b, _)| parse_frac(b.trim()));
                    if let Some(b) = bpm {
                        tempo = Some(match beat {
                            Some((n, d)) => b * (n as f64 * 4.0 / d as f64),
                            None => b,
                        });
                    }
                }
                'K' => {
                    if let Some(k) = parse_key_field(val) {
                        key = k;
                    }
                    if !in_body {
                        in_body = true;
                        if voice_order.is_empty() {
                            voice_order.push(cur_voice.clone());
                            voices.insert(cur_voice.clone(), (Clef::treble(), String::new(), vec![new_measure()], Vec::new(), false));
                        }
                    } else if let Some(v) = voices.get_mut(&cur_voice) {
                        if let Some(m) = v.2.last_mut() {
                            m.key = Some(key);
                        }
                    }
                    if val.contains("clef=bass") {
                        if let Some(v) = voices.get_mut(&cur_voice) {
                            v.0 = Clef::bass();
                        }
                    }
                }
                'V' => {
                    let id = val.split_whitespace().next().unwrap_or("1").to_string();
                    let clef = if val.contains("clef=bass") {
                        Some(Clef::bass())
                    } else if val.contains("clef=perc") {
                        Some(Clef::percussion())
                    } else if val.contains("clef=treble") {
                        Some(Clef::treble())
                    } else {
                        None
                    };
                    let name = val.split("name=\"").nth(1).and_then(|r| r.split('"').next()).map(|s| s.to_string());
                    if !voices.contains_key(&id) {
                        // the default voice created at K: is replaced when empty
                        if voice_order.len() == 1 && voice_order[0] == "1" && id != "1" {
                            let empty = voices.get("1").map(|v| v.2.iter().all(|m| m.notes.is_empty())).unwrap_or(true);
                            if empty {
                                voices.remove("1");
                                voice_order.clear();
                            }
                        }
                        voice_order.push(id.clone());
                        voices.insert(id.clone(), (Clef::treble(), String::new(), vec![new_measure()], Vec::new(), false));
                    }
                    let v = voices.get_mut(&id).unwrap();
                    if let Some(c) = clef {
                        if c.sign == "percussion" {
                            v.4 = true;
                        }
                        v.0 = c;
                    }
                    if let Some(n) = name {
                        v.1 = n;
                    }
                    cur_voice = id;
                }
                'w' => {
                    if let Some(v) = voices.get_mut(&cur_voice) {
                        for tok in val.split_whitespace() {
                            v.3.push(tok.to_string());
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if !in_body {
            continue;
        }
        let v = voices
            .entry(cur_voice.clone())
            .or_insert_with(|| (Clef::treble(), String::new(), vec![new_measure()], Vec::new(), false));
        parse_body_line(line, &mut v.2, unit, key, &mut time)?;
    }
    if voice_order.is_empty() {
        return Err(Error::input("no ABC tune found (missing K: field)"));
    }
    // build score
    let mut den: u64 = 2;
    for v in voices.values() {
        for m in &v.2 {
            for n in &m.notes {
                den = lcm(den, n.len.1);
            }
        }
    }
    let div = den as u32;
    let mut score = Score { title, composer, divisions: div, ..Score::default() };
    for (vi, id) in voice_order.iter().enumerate() {
        let (clef, name, ms, lyrics, perc) = voices.remove(id).unwrap();
        let mut ms: Vec<RMeasure> = ms;
        if ms.last().map(|m| m.notes.is_empty()).unwrap_or(false) && ms.len() > 1 {
            ms.pop();
        }
        let mut measures = Vec::new();
        let mut syl = lyrics.into_iter();
        let mut cur_time = None;
        let mut first_time = None;
        let mut number = 1u32;
        let mut tie_open: Vec<i32> = Vec::new();
        let mut open_ending: Option<String> = None;
        let mut ctime = TimeSig::new(4, 4);
        for (mi, rm) in ms.iter().enumerate() {
            if let Some(t) = rm.time {
                cur_time = Some(t);
            }
            let t = cur_time.unwrap_or(time);
            if mi == 0 {
                first_time = Some(t);
                ctime = t;
            }
            let mut events = Vec::new();
            let mut total = 0u64;
            for (ni, n) in rm.notes.iter().enumerate() {
                let d = (n.len.0 * div as u64 / n.len.1) as u32;
                total += d as u64;
                let mut ev = if n.rest || n.pitches.is_empty() {
                    let mut e = Event::rest(d);
                    e.visible = n.visible;
                    e
                } else {
                    let notes: Vec<Note> = n
                        .pitches
                        .iter()
                        .map(|(p, tie)| {
                            let mut nt = Note::new(*p);
                            nt.tie_start = *tie;
                            nt.tie_stop = tie_open.contains(&nt.midi);
                            if perc {
                                nt.unpitched = true;
                                nt.midi = crate::drums::key_for_display(*p, None) as i32;
                            }
                            nt
                        })
                        .collect();
                    Event::notes(d, notes)
                };
                if !ev.is_rest() {
                    tie_open = ev.notes.iter().filter(|nt| nt.tie_start).map(|nt| nt.midi).collect();
                    if !ev.notes.iter().all(|nt| nt.tie_stop) {
                        loop {
                            match syl.next() {
                                Some(s) if s == "*" => break,
                                Some(s) if s == "|" => continue,
                                Some(s) if s == "_" => break,
                                Some(s) => {
                                    let (txt, syllabic) = if let Some(t) = s.strip_suffix('-') {
                                        (t.to_string(), "begin")
                                    } else {
                                        (s.clone(), "single")
                                    };
                                    ev.lyrics.push(Lyric { text: txt.replace('~', " "), syllabic: syllabic.into(), verse: 1 });
                                    break;
                                }
                                None => break,
                            }
                        }
                    }
                } else {
                    tie_open.clear();
                }
                if let Some((a, nn)) = n.tuplet {
                    ev.tuplet = Some(Tuplet { actual: a, normal: nn, start: false, stop: false });
                }
                ev.dynamic = n.dynamic.clone();
                ev.articulations = n.articulations.clone();
                events.push(ev);
                let _ = ni;
            }
            // tuplet bracket flags
            let ne = events.len();
            for i in 0..ne {
                if events[i].tuplet.is_some() {
                    let prev = i > 0 && events[i - 1].tuplet.is_some();
                    let next = i + 1 < ne && events[i + 1].tuplet.is_some();
                    let t = events[i].tuplet.as_mut().unwrap();
                    t.start = !prev;
                    t.stop = !next;
                }
            }
            // harmonies
            let mut harmonies = Vec::new();
            let mut pos = 0u32;
            for (ni, n) in rm.notes.iter().enumerate() {
                if let Some(cs) = &n.chord_symbol {
                    if let Some((root, q, bass)) = chords::parse_symbol(cs) {
                        harmonies.push(Harmony { offset: pos, text: chords::chord_text(root, &q, bass), root, quality: q, bass, frame: None, confidence: None });
                    }
                }
                pos += events[ni].duration;
            }
            let full = t.measure_divisions(div) as u64;
            let implicit = mi == 0 && total > 0 && total < full;
            if events.is_empty() {
                let mut r = Event::rest(full as u32);
                r.measure_rest = true;
                events.push(r);
            }
            let ending = if let Some(num) = &rm.ending {
                open_ending = Some(num.clone());
                Some(Ending { number: num.clone(), kind: "start".into() })
            } else {
                open_ending.as_ref().map(|num| Ending { number: num.clone(), kind: "continue".into() })
            };
            let mut ending = ending;
            if let Some(e) = ending.as_mut() {
                let closes = rm.repeat_end || rm.final_bar || ms.get(mi + 1).map(|n| n.ending.is_some() || n.repeat_start).unwrap_or(true);
                if closes {
                    let ty = if rm.repeat_end { "stop" } else { "discontinue" };
                    e.kind = if e.kind == "start" { format!("start-{ty}") } else { ty.to_string() };
                    open_ending = None;
                }
            }
            let m = Measure {
                number: if implicit { 0 } else { number },
                implicit,
                key: if mi == 0 { Some(key_at_start(&ms, key)) } else { rm.key },
                time: if mi == 0 { first_time } else if rm.time.is_some() && rm.time != Some(ctime) { rm.time } else { None },
                clef: if mi == 0 { Some(clef.clone()) } else { None },
                tempo: if mi == 0 && vi == 0 { tempo.map(|b| vec![TempoMark { offset: 0, bpm: b, text: None }]).unwrap_or_default() } else { vec![] },
                harmonies,
                voices: vec![Voice { number: 1, events }],
                repeat_start: rm.repeat_start,
                repeat_end: if rm.repeat_end { Some(2) } else { None },
                ending,
                barline: if rm.final_bar && !rm.repeat_end { Some("light-heavy".into()) } else { None },
                ..Measure::default()
            };
            if let Some(tt) = rm.time {
                ctime = tt;
            }
            if !implicit {
                number += 1;
            }
            measures.push(m);
        }
        let _ = rm_lyrics_unused(&ms);
        score.parts.push(Part {
            id: format!("P{}", vi + 1),
            name: if name.is_empty() { if perc { "Drums".into() } else { format!("Voice {}", vi + 1) } } else { name },
            abbreviation: None,
            program: if perc { None } else { Some(0) },
            channel: if perc { Some(10) } else { None },
            percussion: perc,
            staves: vec![Staff { number: 1, measures }],
        });
    }
    Ok(score)
}

fn rm_lyrics_unused(ms: &[RMeasure]) -> usize {
    ms.iter().map(|m| m.lyrics_start).sum()
}

fn key_at_start(ms: &[RMeasure], key: KeySig) -> KeySig {
    ms.first().and_then(|m| m.key).unwrap_or(key)
}

fn parse_body_line(line: &str, measures: &mut Vec<RMeasure>, unit: (u64, u64), key: KeySig, time: &mut TimeSig) -> Result<()> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut acc: HashMap<(char, i8), i8> = HashMap::new();
    let mut pending_symbol: Option<String> = None;
    let mut pending_dyn: Option<String> = None;
    let mut pending_art: Vec<String> = Vec::new();
    let mut tuplet: Option<(u32, u32, u32)> = None; // (p, q, remaining)
    let mut broken: Option<(u64, u64)> = None; // multiplier for the next note
    let mut cur_key = key;
    let new_m = |_: ()| RMeasure { notes: Vec::new(), repeat_start: false, repeat_end: false, ending: None, final_bar: false, time: None, key: None, lyrics_start: 0 };
    let unit_q = Q(unit.0 * 4, unit.1);
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '`' => {
                i += 1;
            }
            '"' => {
                let end = chars[i + 1..].iter().position(|x| *x == '"').map(|p| i + 1 + p).unwrap_or(chars.len());
                let sym: String = chars[i + 1..end].iter().collect();
                if !sym.starts_with(['^', '_', '<', '>', '@']) {
                    pending_symbol = Some(sym);
                }
                i = end + 1;
            }
            '!' | '+' => {
                let end = chars[i + 1..].iter().position(|x| *x == c).map(|p| i + 1 + p).unwrap_or(chars.len());
                let deco: String = chars[i + 1..end].iter().collect();
                match deco.as_str() {
                    "p" | "pp" | "ppp" | "mp" | "mf" | "f" | "ff" | "fff" | "sfz" => pending_dyn = Some(deco),
                    "fermata" | "accent" | "tenuto" | "marcato" | "staccato" => pending_art.push(deco),
                    ">" => pending_art.push("accent".into()),
                    _ => {}
                }
                i = end + 1;
            }
            '.' => {
                pending_art.push("staccato".into());
                i += 1;
            }
            'H' => {
                pending_art.push("fermata".into());
                i += 1;
            }
            'L' => {
                pending_art.push("accent".into());
                i += 1;
            }
            '~' | 'T' | 'u' | 'v' | 'M' | 'P' | 'S' | 'O' | 'J' | 'R' => {
                i += 1;
            }
            '{' => {
                // grace notes: skipped
                let end = chars[i..].iter().position(|x| *x == '}').map(|p| i + p).unwrap_or(chars.len());
                i = end + 1;
            }
            '(' => {
                if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                    let mut j = i + 1;
                    let mut nums = Vec::new();
                    let mut cur = String::new();
                    while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ':') {
                        if chars[j] == ':' {
                            nums.push(cur.clone());
                            cur.clear();
                        } else {
                            cur.push(chars[j]);
                        }
                        j += 1;
                    }
                    nums.push(cur);
                    let p: u32 = nums[0].parse().unwrap_or(3);
                    let compound = time.is_compound();
                    let default_q = match p {
                        2 | 4 | 8 => 3,
                        3 | 6 => 2,
                        _ => {
                            if compound {
                                3
                            } else {
                                2
                            }
                        }
                    };
                    let q: u32 = nums.get(1).and_then(|s| s.parse().ok()).unwrap_or(default_q);
                    let r: u32 = nums.get(2).and_then(|s| s.parse().ok()).unwrap_or(p);
                    tuplet = Some((p, q, r));
                    i = j;
                } else {
                    // slur start
                    i += 1;
                }
            }
            ')' => {
                i += 1;
            }
            '[' if i + 2 < chars.len() && chars[i + 2] == ':' && chars[i + 1].is_ascii_alphabetic() => {
                let end = chars[i..].iter().position(|x| *x == ']').map(|p| i + p).unwrap_or(chars.len());
                let field: String = chars[i + 1..end].iter().collect();
                let (f, v) = field.split_at(1);
                let v = v.trim_start_matches(':').trim();
                match f {
                    "M" => {
                        if let Some((b, bt)) = parse_frac(v) {
                            *time = TimeSig::new(b as u32, bt as u32);
                            measures.last_mut().unwrap().time = Some(*time);
                        }
                    }
                    "K" => {
                        if let Some(k) = parse_key_field(v) {
                            cur_key = k;
                            measures.last_mut().unwrap().key = Some(k);
                        }
                    }
                    _ => {}
                }
                i = end + 1;
            }
            '[' if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() => {
                let mut j = i + 1;
                let mut num = String::new();
                while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ',' || chars[j] == '-') {
                    num.push(chars[j]);
                    j += 1;
                }
                measures.last_mut().unwrap().ending = Some(num.replace(',', ", "));
                i = j;
            }
            '[' if i + 1 < chars.len() && chars[i + 1] == '|' => {
                i += 1;
            }
            '|' | ':' => {
                // bar lines: | || |] [| |: :| :: |1 |2
                let mut j = i;
                let mut tok = String::new();
                while j < chars.len() && matches!(chars[j], '|' | ':' | ']' | '[') {
                    if chars[j] == '[' && j + 1 < chars.len() && chars[j + 1].is_ascii_alphabetic() {
                        break;
                    }
                    if chars[j] == '[' && j + 1 < chars.len() && chars[j + 1].is_ascii_digit() && j > i {
                        break;
                    }
                    tok.push(chars[j]);
                    j += 1;
                }
                let ends_repeat = tok.starts_with(':');
                let starts_repeat = tok.ends_with(':') && tok.len() > 1 || tok == "::";
                let final_bar = tok.contains("|]");
                {
                    let m = measures.last_mut().unwrap();
                    if ends_repeat {
                        m.repeat_end = true;
                    }
                    if final_bar {
                        m.final_bar = true;
                    }
                }
                let has_notes = !measures.last().unwrap().notes.is_empty();
                if has_notes {
                    measures.push(new_m(()));
                }
                acc.clear();
                if starts_repeat {
                    measures.last_mut().unwrap().repeat_start = true;
                }
                // |1 or :|2 style endings
                let mut k = j;
                let mut num = String::new();
                while k < chars.len() && chars[k].is_ascii_digit() {
                    num.push(chars[k]);
                    k += 1;
                }
                if !num.is_empty() && tok.ends_with('|') {
                    measures.last_mut().unwrap().ending = Some(num);
                    j = k;
                }
                i = j.max(i + 1);
            }
            'z' | 'x' | 'Z' | 'X' | 'A'..='G' | 'a'..='g' | '^' | '_' | '=' | '[' => {
                // note, rest or chord
                let mut pitches: Vec<(Pitch, bool)> = Vec::new();
                let mut rest = false;
                let mut visible = true;
                let mut measure_rest = false;
                let mut j = i;
                if matches!(c, 'z' | 'x' | 'Z' | 'X') {
                    rest = true;
                    visible = c == 'z' || c == 'Z';
                    measure_rest = c == 'Z' || c == 'X';
                    j += 1;
                } else if c == '[' {
                    j += 1;
                    while j < chars.len() && chars[j] != ']' {
                        let (p, nj) = parse_pitch(&chars, j, cur_key, &mut acc);
                        match p {
                            Some(p) => {
                                let mut nj = nj;
                                // inner lengths are ignored; take the chord length after ]
                                while nj < chars.len() && (chars[nj].is_ascii_digit() || chars[nj] == '/') {
                                    nj += 1;
                                }
                                let tie = nj < chars.len() && chars[nj] == '-';
                                if tie {
                                    nj += 1;
                                }
                                pitches.push((p, tie));
                                j = nj;
                            }
                            None => j = nj.max(j + 1),
                        }
                    }
                    j += 1;
                } else {
                    let (p, nj) = parse_pitch(&chars, j, cur_key, &mut acc);
                    if let Some(p) = p {
                        pitches.push((p, false));
                    }
                    j = nj;
                }
                // length
                let (mut ln, mut ld) = (1u64, 1u64);
                let mut digits = String::new();
                while j < chars.len() && chars[j].is_ascii_digit() {
                    digits.push(chars[j]);
                    j += 1;
                }
                if !digits.is_empty() {
                    ln = digits.parse().unwrap_or(1);
                }
                while j < chars.len() && chars[j] == '/' {
                    j += 1;
                    let mut d2 = String::new();
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        d2.push(chars[j]);
                        j += 1;
                    }
                    ld *= if d2.is_empty() { 2 } else { d2.parse().unwrap_or(2) };
                }
                // tie after a single note or chord
                if j < chars.len() && chars[j] == '-' {
                    for p in pitches.iter_mut() {
                        p.1 = true;
                    }
                    j += 1;
                }
                let mut len = unit_q.mul(ln, ld);
                if measure_rest {
                    len = Q(time.beats as u64 * 4, time.beat_type as u64).mul(ln, 1);
                }
                if let Some((n, d)) = broken.take() {
                    len = len.mul(n, d);
                }
                // broken rhythm after this note
                let mut k = j;
                while k < chars.len() && chars[k] == ' ' {
                    k += 1;
                }
                if k < chars.len() && (chars[k] == '>' || chars[k] == '<') {
                    let mut cnt = 0;
                    let sym = chars[k];
                    while k < chars.len() && chars[k] == sym {
                        cnt += 1;
                        k += 1;
                    }
                    let big = (1u64 << (cnt + 1)) - 1; // 3, 7
                    let den = 1u64 << cnt; // 2, 4
                    if sym == '>' {
                        len = len.mul(big, den);
                        broken = Some((1, den));
                    } else {
                        len = len.mul(1, den);
                        broken = Some((big, den));
                    }
                    j = k;
                }
                let mut tup = None;
                if let Some((p, q, r)) = tuplet {
                    len = len.mul(q as u64, p as u64);
                    tup = Some((p, q));
                    tuplet = if r > 1 { Some((p, q, r - 1)) } else { None };
                }
                measures.last_mut().unwrap().notes.push(RNote {
                    rest,
                    visible,
                    pitches,
                    len,
                    tuplet: tup,
                    chord_symbol: pending_symbol.take(),
                    dynamic: pending_dyn.take(),
                    articulations: std::mem::take(&mut pending_art),
                });
                i = j.max(i + 1);
            }
            '&' => {
                return Err(Error::unsupported("ABC voice overlay (&) is not supported by the ABC reader; use separate V: voices"));
            }
            '\\' => {
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    let _ = Q(1, 1).add(Q(1, 1));
    Ok(())
}

fn parse_pitch(chars: &[char], mut j: usize, key: KeySig, acc: &mut HashMap<(char, i8), i8>) -> (Option<Pitch>, usize) {
    let mut alter: Option<i8> = None;
    let mut a = 0i8;
    while j < chars.len() && matches!(chars[j], '^' | '_' | '=') {
        match chars[j] {
            '^' => a += 1,
            '_' => a -= 1,
            _ => {
                alter = Some(0);
            }
        }
        j += 1;
    }
    if a != 0 {
        alter = Some(a);
    }
    if j >= chars.len() || !chars[j].is_ascii_alphabetic() || !matches!(chars[j].to_ascii_uppercase(), 'A'..='G') {
        return (None, j + 1);
    }
    let c = chars[j];
    let step = c.to_ascii_uppercase();
    let mut octave: i8 = if c.is_ascii_lowercase() { 5 } else { 4 };
    j += 1;
    while j < chars.len() && (chars[j] == '\'' || chars[j] == ',') {
        octave += if chars[j] == '\'' { 1 } else { -1 };
        j += 1;
    }
    let k = (step, octave);
    let alter = match alter {
        Some(al) => {
            acc.insert(k, al);
            al
        }
        None => acc.get(&k).copied().unwrap_or_else(|| key_alter(key.fifths, step)),
    };
    let _ = spell_midi;
    (Some(Pitch { step, alter, octave }), j)
}
