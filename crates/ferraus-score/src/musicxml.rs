//! MusicXML 4.0 (partwise) writer and reader, plus compressed `.mxl` containers.

use crate::chords;
use crate::drums;
use crate::model::*;
use crate::pitch::{lcm, type_for};
use ferraus_core::{Error, Result};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Voice number written for a staff-local voice (MuseScore convention: 1-4 on staff 1,
/// 5-8 on staff 2).
fn xml_voice(staff: u32, local: u32) -> u32 {
    (staff - 1) * 4 + local
}

fn accidental_name(alter: i8) -> &'static str {
    match alter {
        -2 => "flat-flat",
        -1 => "flat",
        1 => "sharp",
        2 => "double-sharp",
        _ => "natural",
    }
}

/// Key-signature alteration of a step.
pub fn key_alter(fifths: i8, step: char) -> i8 {
    const SHARPS: [char; 7] = ['F', 'C', 'G', 'D', 'A', 'E', 'B'];
    let f = fifths as i32;
    let idx = SHARPS.iter().position(|s| *s == step).unwrap_or(0) as i32;
    if f > 0 && idx < f {
        1
    } else if f < 0 && (6 - idx) < -f {
        -1
    } else {
        0
    }
}

fn instrument_id(part: &Part, key: i32) -> String {
    format!("{}-I{}", part.id, key + 1)
}

/// Serialize a score as MusicXML 4.0 partwise.
pub fn write(score: &Score) -> String {
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n");
    x.push_str("<!DOCTYPE score-partwise PUBLIC \"-//Recordare//DTD MusicXML 4.0 Partwise//EN\" \"http://www.musicxml.org/dtds/partwise.dtd\">\n");
    x.push_str("<score-partwise version=\"4.0\">\n");
    if let Some(t) = &score.title {
        let _ = writeln!(x, "  <work>\n    <work-title>{}</work-title>\n  </work>", esc(t));
    }
    x.push_str("  <identification>\n");
    if let Some(c) = &score.composer {
        let _ = writeln!(x, "    <creator type=\"composer\">{}</creator>", esc(c));
    }
    let _ = writeln!(
        x,
        "    <encoding>\n      <software>ferraus {}</software>\n    </encoding>",
        ferraus_core::FERRAUS_VERSION
    );
    x.push_str("  </identification>\n");
    x.push_str("  <part-list>\n");
    for p in &score.parts {
        let _ = writeln!(x, "    <score-part id=\"{}\">", esc(&p.id));
        let _ = writeln!(x, "      <part-name>{}</part-name>", esc(&p.name));
        if let Some(a) = &p.abbreviation {
            let _ = writeln!(x, "      <part-abbreviation>{}</part-abbreviation>", esc(a));
        }
        if p.percussion {
            let mut keys: BTreeMap<i32, String> = BTreeMap::new();
            for st in &p.staves {
                for m in &st.measures {
                    for v in &m.voices {
                        for e in &v.events {
                            for n in &e.notes {
                                keys.entry(n.midi).or_insert_with(|| {
                                    n.instrument
                                        .clone()
                                        .unwrap_or_else(|| drums::gm_drum_name(n.midi as u8).to_string())
                                });
                            }
                        }
                    }
                }
            }
            for (k, name) in &keys {
                let _ = writeln!(
                    x,
                    "      <score-instrument id=\"{}\">\n        <instrument-name>{}</instrument-name>\n      </score-instrument>",
                    instrument_id(p, *k),
                    esc(name)
                );
            }
            for k in keys.keys() {
                let _ = writeln!(
                    x,
                    "      <midi-instrument id=\"{}\">\n        <midi-channel>10</midi-channel>\n        <midi-unpitched>{}</midi-unpitched>\n      </midi-instrument>",
                    instrument_id(p, *k),
                    k + 1
                );
            }
        } else {
            let _ = writeln!(
                x,
                "      <score-instrument id=\"{}-I1\">\n        <instrument-name>{}</instrument-name>\n      </score-instrument>",
                esc(&p.id),
                esc(&p.name)
            );
            if p.program.is_some() || p.channel.is_some() {
                let _ = writeln!(x, "      <midi-instrument id=\"{}-I1\">", esc(&p.id));
                if let Some(c) = p.channel {
                    let _ = writeln!(x, "        <midi-channel>{c}</midi-channel>");
                }
                if let Some(pr) = p.program {
                    let _ = writeln!(x, "        <midi-program>{}</midi-program>", pr as u32 + 1);
                }
                x.push_str("      </midi-instrument>\n");
            }
        }
        x.push_str("    </score-part>\n");
    }
    x.push_str("  </part-list>\n");
    for p in &score.parts {
        write_part(&mut x, score, p);
    }
    x.push_str("</score-partwise>\n");
    x
}

fn write_part(x: &mut String, score: &Score, p: &Part) {
    let _ = writeln!(x, "  <part id=\"{}\">", esc(&p.id));
    let nmeas = p.staves.iter().map(|s| s.measures.len()).max().unwrap_or(0);
    let multi = p.staves.len() > 1;
    let div = score.divisions;
    for mi in 0..nmeas {
        let first = p.staves[0].measures.get(mi).cloned().unwrap_or_default();
        let _ = write!(x, "    <measure number=\"{}\"", first.number);
        if first.implicit {
            x.push_str(" implicit=\"yes\"");
        }
        x.push_str(">\n");
        // left barline
        let ending_start = first.ending.as_ref().filter(|e| e.starts());
        if first.repeat_start || ending_start.is_some() {
            x.push_str("      <barline location=\"left\">\n");
            if first.repeat_start {
                x.push_str("        <bar-style>heavy-light</bar-style>\n");
            }
            if let Some(e) = ending_start {
                let _ = writeln!(x, "        <ending number=\"{}\" type=\"start\"/>", esc(&e.number));
            }
            if first.repeat_start {
                x.push_str("        <repeat direction=\"forward\"/>\n");
            }
            x.push_str("      </barline>\n");
        }
        // attributes
        let clefs: Vec<(u32, &Clef)> = p
            .staves
            .iter()
            .filter_map(|s| s.measures.get(mi).and_then(|m| m.clef.as_ref()).map(|c| (s.number, c)))
            .collect();
        let need_div = mi == 0;
        if need_div || first.key.is_some() || first.time.is_some() || !clefs.is_empty() {
            x.push_str("      <attributes>\n");
            if need_div {
                let _ = writeln!(x, "        <divisions>{div}</divisions>");
            }
            let key = first.key.or(if mi == 0 { Some(KeySig::major(0)) } else { None });
            if let Some(k) = key {
                if !p.percussion || first.key.is_some() {
                    let _ = writeln!(
                        x,
                        "        <key>\n          <fifths>{}</fifths>\n          <mode>{}</mode>\n        </key>",
                        k.fifths,
                        if k.mode == Mode::Minor { "minor" } else { "major" }
                    );
                }
            }
            let time = first.time.or(if mi == 0 { Some(TimeSig::new(4, 4)) } else { None });
            if let Some(t) = time {
                let _ = writeln!(
                    x,
                    "        <time>\n          <beats>{}</beats>\n          <beat-type>{}</beat-type>\n        </time>",
                    t.beats, t.beat_type
                );
            }
            if mi == 0 && multi {
                let _ = writeln!(x, "        <staves>{}</staves>", p.staves.len());
            }
            for (sn, c) in &clefs {
                if multi {
                    let _ = writeln!(x, "        <clef number=\"{sn}\">");
                } else {
                    x.push_str("        <clef>\n");
                }
                let _ = writeln!(x, "          <sign>{}</sign>", esc(&c.sign));
                if c.sign != "percussion" || c.line != 3 {
                    let _ = writeln!(x, "          <line>{}</line>", c.line);
                }
                if c.octave_change != 0 {
                    let _ = writeln!(x, "          <clef-octave-change>{}</clef-octave-change>", c.octave_change);
                }
                x.push_str("        </clef>\n");
            }
            x.push_str("      </attributes>\n");
        }
        // tempo marks (from staff 1)
        for t in &first.tempo {
            x.push_str("      <direction placement=\"above\">\n");
            if let Some(txt) = &t.text {
                let _ = writeln!(
                    x,
                    "        <direction-type>\n          <words>{}</words>\n        </direction-type>",
                    esc(txt)
                );
            }
            let _ = writeln!(
                x,
                "        <direction-type>\n          <metronome>\n            <beat-unit>quarter</beat-unit>\n            <per-minute>{}</per-minute>\n          </metronome>\n        </direction-type>",
                fmt_num(t.bpm)
            );
            if t.offset > 0 {
                let _ = writeln!(x, "        <offset>{}</offset>", t.offset);
            }
            let _ = writeln!(x, "        <sound tempo=\"{}\"/>", fmt_num(t.bpm));
            x.push_str("      </direction>\n");
        }
        // voices of every staff
        let mut blocks: Vec<(u32, &Measure, &Voice)> = Vec::new();
        for st in &p.staves {
            if let Some(m) = st.measures.get(mi) {
                for v in &m.voices {
                    blocks.push((st.number, m, v));
                }
            }
        }
        let nblocks = blocks.len();
        for (bi, (sn, m, v)) in blocks.iter().enumerate() {
            let two_voices = m.voices.len() > 1;
            let stem = if two_voices { Some(if v.number % 2 == 1 { "up" } else { "down" }) } else { None };
            // harmonies and directions go with the first voice of each staff
            let first_voice = m.voices.first().map(|fv| fv.number) == Some(v.number);
            let mut harm: Vec<&Harmony> = if first_voice { m.harmonies.iter().collect() } else { vec![] };
            harm.sort_by_key(|h| h.offset);
            let mut dirs: Vec<&Direction> = if first_voice { m.directions.iter().collect() } else { vec![] };
            dirs.sort_by_key(|d| d.offset);
            let mut hi = 0;
            let mut di = 0;
            let fifths = Score::key_at(p.staves.iter().find(|s| s.number == *sn).unwrap_or(&p.staves[0]), mi).fifths;
            let mut acc_state: HashMap<(char, i8), i8> = HashMap::new();
            let staff_ref = p.staves.iter().find(|s| s.number == *sn).unwrap_or(&p.staves[0]);
            let beams = if p.percussion && v.number == 2 {
                vec![Vec::new(); v.events.len()]
            } else {
                beam_states(&v.events, score.divisions, Score::time_at(staff_ref, mi), m.implicit)
            };
            let mut pos = 0u32;
            for (ei, e) in v.events.iter().enumerate() {
                // emit harmonies/directions whose offset falls before the end of this event
                let end = pos + e.duration;
                while hi < harm.len() && (harm[hi].offset < end || (e.duration == 0 && harm[hi].offset <= pos)) {
                    write_harmony(x, harm[hi], harm[hi].offset.saturating_sub(pos), multi.then_some(*sn));
                    hi += 1;
                }
                while di < dirs.len() && (dirs[di].offset < end || (e.duration == 0 && dirs[di].offset <= pos)) {
                    write_direction(
                        x,
                        dirs[di],
                        dirs[di].offset.saturating_sub(pos),
                        multi.then_some(*sn),
                        xml_voice(*sn, v.number),
                    );
                    di += 1;
                }
                if let Some(dy) = &e.dynamic {
                    let d = Direction { offset: 0, kind: "dynamic".into(), value: dy.clone(), placement: None };
                    write_direction(x, &d, 0, multi.then_some(*sn), xml_voice(*sn, v.number));
                }
                write_event(x, score, p, e, *sn, v.number, multi, stem, fifths, &mut acc_state, &beams[ei]);
                pos = end;
            }
            while hi < harm.len() {
                write_harmony(x, harm[hi], harm[hi].offset.saturating_sub(pos), multi.then_some(*sn));
                hi += 1;
            }
            while di < dirs.len() {
                write_direction(
                    x,
                    dirs[di],
                    dirs[di].offset.saturating_sub(pos),
                    multi.then_some(*sn),
                    xml_voice(*sn, v.number),
                );
                di += 1;
            }
            if bi + 1 < nblocks && pos > 0 {
                let _ = writeln!(x, "      <backup>\n        <duration>{pos}</duration>\n      </backup>");
            }
        }
        // right barline
        let ending_stop = first.ending.as_ref().filter(|e| e.stops());
        if first.repeat_end.is_some() || ending_stop.is_some() || first.barline.is_some() {
            x.push_str("      <barline location=\"right\">\n");
            let style = first.barline.clone().or_else(|| first.repeat_end.map(|_| "light-heavy".to_string()));
            if let Some(s) = style {
                let _ = writeln!(x, "        <bar-style>{}</bar-style>", esc(&s));
            }
            if let Some(e) = ending_stop {
                let _ = writeln!(x, "        <ending number=\"{}\" type=\"{}\"/>", esc(&e.number), e.stop_type());
            }
            if let Some(times) = first.repeat_end {
                if times != 2 {
                    let _ = writeln!(x, "        <repeat direction=\"backward\" times=\"{times}\"/>");
                } else {
                    x.push_str("        <repeat direction=\"backward\"/>\n");
                }
            }
            x.push_str("      </barline>\n");
        }
        x.push_str("    </measure>\n");
    }
    x.push_str("  </part>\n");
}

/// Beam groups: consecutive eighth-or-shorter notes inside one beat (a dotted quarter in
/// compound meters); a second beam level joins sixteenths.
fn beam_states(events: &[Event], div: u32, time: TimeSig, implicit: bool) -> Vec<Vec<(u8, &'static str)>> {
    let n = events.len();
    let mut out: Vec<Vec<(u8, &'static str)>> = vec![Vec::new(); n];
    let beat = time.beat_divisions(div).max(1);
    let total: u32 = events.iter().filter(|e| !e.grace).map(|e| e.duration).sum();
    let full = time.measure_divisions(div);
    // pickups are aligned to the end of the bar
    let shift = if implicit && total < full { full - total } else { 0 };
    let level_of = |e: &Event| -> u8 {
        if e.is_rest() || e.grace || e.duration == 0 {
            return 0;
        }
        let tup = e.tuplet.map(|t| (t.actual, t.normal));
        let t = match &e.note_type {
            Some(t) => Some(t.as_str()),
            None => type_for(e.duration, div, tup).map(|x| x.0),
        };
        match t {
            Some("eighth") => 1,
            Some("16th") => 2,
            Some("32nd") => 3,
            Some("64th") => 4,
            _ => 0,
        }
    };
    let mut pos = shift;
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_beat = u32::MAX;
    for (i, e) in events.iter().enumerate() {
        if e.grace {
            continue;
        }
        let b = pos / beat;
        let lv = level_of(e);
        if lv == 0 || b != cur_beat || (pos + e.duration).div_ceil(beat) - 1 != b {
            if cur.len() > 1 {
                groups.push(std::mem::take(&mut cur));
            }
            cur.clear();
        }
        if lv > 0 && (pos + e.duration).div_ceil(beat) - 1 == b {
            cur.push(i);
            cur_beat = b;
        } else {
            cur_beat = u32::MAX;
        }
        pos += e.duration;
    }
    if cur.len() > 1 {
        groups.push(cur);
    }
    for g in groups {
        let k = g.len();
        for (j, &i) in g.iter().enumerate() {
            let kind = if j == 0 {
                "begin"
            } else if j + 1 == k {
                "end"
            } else {
                "continue"
            };
            out[i].push((1, kind));
        }
        // deeper levels
        for level in 2..=4u8 {
            let mut j = 0;
            while j < k {
                if level_of(&events[g[j]]) >= level {
                    let start = j;
                    while j < k && level_of(&events[g[j]]) >= level {
                        j += 1;
                    }
                    let run = &g[start..j];
                    if run.len() == 1 {
                        let hook = if start == 0 { "forward hook" } else { "backward hook" };
                        out[run[0]].push((level, hook));
                    } else {
                        for (r, &i) in run.iter().enumerate() {
                            let kind = if r == 0 {
                                "begin"
                            } else if r + 1 == run.len() {
                                "end"
                            } else {
                                "continue"
                            };
                            out[i].push((level, kind));
                        }
                    }
                } else {
                    j += 1;
                }
            }
        }
    }
    out
}

fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn write_harmony(x: &mut String, h: &Harmony, offset: u32, staff: Option<u32>) {
    x.push_str("      <harmony>\n");
    let _ = writeln!(x, "        <root>\n          <root-step>{}</root-step>", h.root.step);
    if h.root.alter != 0 {
        let _ = writeln!(x, "          <root-alter>{}</root-alter>", h.root.alter);
    }
    x.push_str("        </root>\n");
    let _ = writeln!(
        x,
        "        <kind text=\"{}\">{}</kind>",
        esc(chords::suffix(&h.quality)),
        chords::musicxml_kind(&h.quality)
    );
    if let Some(b) = &h.bass {
        let _ = writeln!(x, "        <bass>\n          <bass-step>{}</bass-step>", b.step);
        if b.alter != 0 {
            let _ = writeln!(x, "          <bass-alter>{}</bass-alter>", b.alter);
        }
        x.push_str("        </bass>\n");
    }
    if h.quality == "add9" {
        x.push_str("        <degree>\n          <degree-value>9</degree-value>\n          <degree-alter>0</degree-alter>\n          <degree-type>add</degree-type>\n        </degree>\n");
    }
    if let Some(f) = &h.frame {
        x.push_str("        <frame>\n");
        let _ = writeln!(x, "          <frame-strings>{}</frame-strings>", f.strings);
        let _ = writeln!(x, "          <frame-frets>{}</frame-frets>", f.frets);
        if f.first_fret > 1 {
            let _ = writeln!(x, "          <first-fret>{}</first-fret>", f.first_fret);
        }
        let n = f.fret_by_string.len();
        for (i, fr) in f.fret_by_string.iter().enumerate() {
            if let Some(fr) = fr {
                // MusicXML string 1 is the highest string
                let _ = writeln!(
                    x,
                    "          <frame-note>\n            <string>{}</string>\n            <fret>{}</fret>\n          </frame-note>",
                    n - i,
                    fr
                );
            }
        }
        x.push_str("        </frame>\n");
    }
    if offset > 0 {
        let _ = writeln!(x, "        <offset>{offset}</offset>");
    }
    if let Some(s) = staff {
        let _ = writeln!(x, "        <staff>{s}</staff>");
    }
    x.push_str("      </harmony>\n");
}

fn write_direction(x: &mut String, d: &Direction, offset: u32, staff: Option<u32>, voice: u32) {
    let placement = d.placement.clone().unwrap_or_else(|| {
        if d.kind == "dynamic" || d.kind == "wedge" {
            "below".into()
        } else {
            "above".into()
        }
    });
    let _ = writeln!(x, "      <direction placement=\"{placement}\">");
    x.push_str("        <direction-type>\n");
    match d.kind.as_str() {
        "dynamic" => {
            let _ =
                writeln!(x, "          <dynamics>\n            <{}/>\n          </dynamics>", dynamic_tag(&d.value));
        }
        "wedge" => {
            let _ = writeln!(x, "          <wedge type=\"{}\"/>", esc(&d.value));
        }
        _ => {
            let _ = writeln!(x, "          <words>{}</words>", esc(&d.value));
        }
    }
    x.push_str("        </direction-type>\n");
    if offset > 0 {
        let _ = writeln!(x, "        <offset>{offset}</offset>");
    }
    let _ = writeln!(x, "        <voice>{voice}</voice>");
    if let Some(s) = staff {
        let _ = writeln!(x, "        <staff>{s}</staff>");
    }
    if d.kind == "dynamic" {
        if let Some(v) = dynamic_velocity(&d.value) {
            // MusicXML dynamics attribute: percentage of 90 (forte)
            let _ = writeln!(x, "        <sound dynamics=\"{}\"/>", fmt_num(v as f64 / 90.0 * 100.0));
        }
    }
    x.push_str("      </direction>\n");
}

fn dynamic_tag(v: &str) -> String {
    const KNOWN: &[&str] = &[
        "pppppp", "ppppp", "pppp", "ppp", "pp", "p", "mp", "mf", "f", "ff", "fff", "ffff", "fffff", "ffffff", "sf",
        "sfz", "sffz", "sfp", "fp", "rf", "rfz", "fz", "pf", "sfpp", "n",
    ];
    if KNOWN.contains(&v) {
        v.to_string()
    } else {
        "mf".to_string()
    }
}

/// MIDI velocity for a dynamic marking.
pub fn dynamic_velocity(v: &str) -> Option<u8> {
    Some(match v {
        "pppppp" | "ppppp" | "pppp" => 10,
        "ppp" => 16,
        "pp" => 33,
        "p" => 49,
        "mp" => 64,
        "mf" => 80,
        "f" => 96,
        "ff" => 112,
        "fff" | "ffff" | "fffff" | "ffffff" => 126,
        "sf" | "sfz" | "fz" | "sffz" | "rf" | "rfz" => 112,
        "fp" | "sfp" => 96,
        _ => return None,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_event(
    x: &mut String,
    score: &Score,
    part: &Part,
    e: &Event,
    staff: u32,
    voice: u32,
    multi: bool,
    stem: Option<&str>,
    fifths: i8,
    acc_state: &mut HashMap<(char, i8), i8>,
    beams: &[(u8, &'static str)],
) {
    let div = score.divisions;
    let tup = e.tuplet.map(|t| (t.actual, t.normal));
    let (ty, dots) = match &e.note_type {
        Some(t) => (Some(t.clone()), e.dots),
        None => match type_for(e.duration, div, tup) {
            Some((t, d)) => (Some(t.to_string()), d),
            None => (None, 0),
        },
    };
    let notes: Vec<Option<&Note>> = if e.is_rest() { vec![None] } else { e.notes.iter().map(Some).collect() };
    for (ni, n) in notes.iter().enumerate() {
        let print = if e.visible { "" } else { " print-object=\"no\"" };
        let _ = writeln!(x, "      <note{print}>");
        if e.grace {
            x.push_str("        <grace/>\n");
        }
        if ni > 0 {
            x.push_str("        <chord/>\n");
        }
        match n {
            None => {
                if e.measure_rest {
                    x.push_str("        <rest measure=\"yes\"/>\n");
                } else {
                    x.push_str("        <rest/>\n");
                }
            }
            Some(n) if n.unpitched || part.percussion => {
                let _ = writeln!(
                    x,
                    "        <unpitched>\n          <display-step>{}</display-step>\n          <display-octave>{}</display-octave>\n        </unpitched>",
                    n.pitch.step, n.pitch.octave
                );
            }
            Some(n) => {
                let _ = writeln!(x, "        <pitch>\n          <step>{}</step>", n.pitch.step);
                if n.pitch.alter != 0 {
                    let _ = writeln!(x, "          <alter>{}</alter>", n.pitch.alter);
                }
                let _ = writeln!(x, "          <octave>{}</octave>\n        </pitch>", n.pitch.octave);
            }
        }
        if !e.grace {
            let _ = writeln!(x, "        <duration>{}</duration>", e.duration);
        }
        if let Some(n) = n {
            if n.tie_stop {
                x.push_str("        <tie type=\"stop\"/>\n");
            }
            if n.tie_start {
                x.push_str("        <tie type=\"start\"/>\n");
            }
            if n.unpitched || part.percussion {
                let _ = writeln!(x, "        <instrument id=\"{}\"/>", instrument_id(part, n.midi));
            }
        }
        let _ = writeln!(x, "        <voice>{}</voice>", xml_voice(staff, voice));
        if let Some(t) = &ty {
            if !(e.measure_rest && n.is_none()) {
                let _ = writeln!(x, "        <type>{t}</type>");
                for _ in 0..dots {
                    x.push_str("        <dot/>\n");
                }
            }
        }
        // accidentals
        if let Some(n) = n {
            if !(n.unpitched || part.percussion) {
                let k = (n.pitch.step, n.pitch.octave);
                let current = acc_state.get(&k).copied().unwrap_or_else(|| key_alter(fifths, n.pitch.step));
                if n.tie_stop {
                    acc_state.insert(k, n.pitch.alter);
                } else if current != n.pitch.alter {
                    let _ = writeln!(x, "        <accidental>{}</accidental>", accidental_name(n.pitch.alter));
                    acc_state.insert(k, n.pitch.alter);
                }
            }
        }
        if let Some(t) = e.tuplet {
            let _ = writeln!(
                x,
                "        <time-modification>\n          <actual-notes>{}</actual-notes>\n          <normal-notes>{}</normal-notes>\n        </time-modification>",
                t.actual, t.normal
            );
        }
        if let (Some(s), Some(_)) = (stem, n) {
            let _ = writeln!(x, "        <stem>{s}</stem>");
        }
        if let Some(n) = n {
            if let Some(h) = &n.notehead {
                let _ = writeln!(x, "        <notehead>{}</notehead>", esc(h));
            }
        }
        if multi {
            let _ = writeln!(x, "        <staff>{staff}</staff>");
        }
        if ni == 0 {
            for (level, kind) in beams {
                let _ = writeln!(x, "        <beam number=\"{level}\">{kind}</beam>");
            }
        }
        // notations
        let mut nots = String::new();
        if let Some(n) = n {
            if n.tie_stop {
                nots.push_str("          <tied type=\"stop\"/>\n");
            }
            if n.tie_start {
                nots.push_str("          <tied type=\"start\"/>\n");
            }
        }
        if ni == 0 {
            for s in &e.slurs {
                let _ = writeln!(nots, "          <slur type=\"{}\" number=\"1\"/>", esc(s));
            }
            if let Some(t) = e.tuplet {
                if t.start {
                    nots.push_str("          <tuplet type=\"start\" bracket=\"yes\"/>\n");
                }
                if t.stop {
                    nots.push_str("          <tuplet type=\"stop\"/>\n");
                }
            }
            let arts: Vec<&String> = e.articulations.iter().filter(|a| a.as_str() != "fermata").collect();
            if !arts.is_empty() {
                nots.push_str("          <articulations>\n");
                for a in arts {
                    let tag = match a.as_str() {
                        "marcato" => "strong-accent",
                        "legato" => "detached-legato",
                        other => other,
                    };
                    let _ = writeln!(nots, "            <{}/>", esc(tag));
                }
                nots.push_str("          </articulations>\n");
            }
            if e.articulations.iter().any(|a| a == "fermata") {
                nots.push_str("          <fermata/>\n");
            }
        }
        if !nots.is_empty() {
            x.push_str("        <notations>\n");
            x.push_str(&nots);
            x.push_str("        </notations>\n");
        }
        if ni == 0 {
            let max_verse = e.lyrics.iter().map(|l| l.verse).max().unwrap_or(0);
            for l in &e.lyrics {
                let _ = writeln!(
                    x,
                    "        <lyric number=\"{}\">\n          <syllabic>{}</syllabic>\n          <text>{}</text>\n        </lyric>",
                    l.verse,
                    esc(&l.syllabic),
                    esc(&l.text)
                );
            }
            if let Some(s) = &e.solfa {
                let _ = writeln!(
                    x,
                    "        <lyric number=\"{}\" name=\"solfa\">\n          <syllabic>single</syllabic>\n          <text>{}</text>\n        </lyric>",
                    max_verse.max(1) + 1,
                    esc(s)
                );
            }
        }
        x.push_str("      </note>\n");
    }
}

// ------------------------------------------------------------------------------------
// Reader
// ------------------------------------------------------------------------------------

/// Read MusicXML text (partwise) or `.mxl` bytes.
pub fn read_bytes(bytes: &[u8]) -> Result<(Score, Vec<String>)> {
    if bytes.starts_with(b"PK") {
        let text = read_mxl(bytes)?;
        return read(&text);
    }
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    read(text)
}

/// Extract the root MusicXML document from a compressed `.mxl` container.
pub fn read_mxl(bytes: &[u8]) -> Result<String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| Error::input(format!("not a valid .mxl container: {e}")))?;
    let mut root: Option<String> = None;
    if let Ok(mut c) = zip.by_name("META-INF/container.xml") {
        let mut s = String::new();
        c.read_to_string(&mut s)?;
        if let Ok(doc) = roxmltree::Document::parse(&s) {
            root = doc
                .descendants()
                .find(|n| n.has_tag_name("rootfile"))
                .and_then(|n| n.attribute("full-path"))
                .map(|s| s.to_string());
        }
    }
    let name = match root {
        Some(r) => r,
        None => (0..zip.len())
            .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
            .find(|n| (n.ends_with(".xml") || n.ends_with(".musicxml")) && !n.starts_with("META-INF"))
            .ok_or_else(|| Error::input("the .mxl container has no MusicXML document"))?,
    };
    let mut f = zip.by_name(&name).map_err(|e| Error::input(format!("cannot open {name} in .mxl: {e}")))?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}

/// Write a compressed `.mxl` container.
pub fn write_mxl(score: &Score) -> Result<Vec<u8>> {
    use std::io::Write;
    let xml = write(score);
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let zerr = |e: zip::result::ZipError| Error::processing(format!("cannot write .mxl: {e}"));
        z.start_file("mimetype", stored).map_err(zerr)?;
        z.write_all(b"application/vnd.recordare.musicxml")?;
        z.start_file("META-INF/container.xml", deflated).map_err(zerr)?;
        z.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container>\n  <rootfiles>\n    <rootfile full-path=\"score.musicxml\" media-type=\"application/vnd.recordare.musicxml+xml\"/>\n  </rootfiles>\n</container>\n")?;
        z.start_file("score.musicxml", deflated).map_err(zerr)?;
        z.write_all(xml.as_bytes())?;
        z.finish().map_err(zerr)?;
    }
    Ok(buf.into_inner())
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(name))
}

fn child_text<'a>(n: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(n, name).and_then(|c| c.text()).map(|t| t.trim())
}

fn num<T: std::str::FromStr>(s: Option<&str>) -> Option<T> {
    s.and_then(|v| v.trim().parse::<T>().ok())
}

fn num_f(s: Option<&str>) -> Option<f64> {
    s.and_then(|v| v.trim().parse::<f64>().ok())
}

struct RawEvent {
    staff: u32,
    voice: u32,
    start: u64,
    event: Event,
}

/// Parse MusicXML text. Returns the score and a list of unsupported constructs seen.
pub fn read(text: &str) -> Result<(Score, Vec<String>)> {
    let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, opts)
        .map_err(|e| Error::input(format!("not valid MusicXML: {e}")))?;
    let root = doc.root_element();
    if root.has_tag_name("score-timewise") {
        return Err(Error::unsupported("timewise MusicXML is not supported; convert it to partwise first"));
    }
    if !root.has_tag_name("score-partwise") {
        return Err(Error::input(format!("not a MusicXML score (root element <{}>)", root.tag_name().name())));
    }
    let mut unsupported: Vec<String> = Vec::new();
    // global divisions = lcm of all divisions values
    let mut g: u64 = 1;
    for d in root.descendants().filter(|n| n.has_tag_name("divisions")) {
        if let Some(v) = num::<f64>(d.text()) {
            if v >= 1.0 && v.fract() == 0.0 {
                g = lcm(g, v as u64);
            }
        }
    }
    if g > 1_000_000 {
        g = 960;
    }
    let mut score = Score { divisions: g as u32, ..Score::default() };
    score.title = root
        .descendants()
        .find(|n| n.has_tag_name("work-title"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            root.descendants()
                .find(|n| n.has_tag_name("movement-title"))
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string())
        });
    score.composer = root
        .descendants()
        .find(|n| n.has_tag_name("creator") && n.attribute("type") == Some("composer"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string());

    // part-list info
    struct PartInfo {
        name: String,
        abbr: Option<String>,
        program: Option<u8>,
        channel: Option<u8>,
        unpitched: HashMap<String, i32>,
        inst_names: HashMap<String, String>,
    }
    let mut infos: HashMap<String, PartInfo> = HashMap::new();
    if let Some(pl) = child(root, "part-list") {
        for sp in pl.children().filter(|c| c.has_tag_name("score-part")) {
            let id = sp.attribute("id").unwrap_or("P1").to_string();
            let mut info = PartInfo {
                name: child_text(sp, "part-name").unwrap_or("").to_string(),
                abbr: child_text(sp, "part-abbreviation").map(|s| s.to_string()),
                program: None,
                channel: None,
                unpitched: HashMap::new(),
                inst_names: HashMap::new(),
            };
            for si in sp.children().filter(|c| c.has_tag_name("score-instrument")) {
                if let (Some(iid), Some(nm)) = (si.attribute("id"), child_text(si, "instrument-name")) {
                    info.inst_names.insert(iid.to_string(), nm.to_string());
                }
            }
            for mi in sp.children().filter(|c| c.has_tag_name("midi-instrument")) {
                let iid = mi.attribute("id").unwrap_or("").to_string();
                if let Some(u) = num::<i32>(child_text(mi, "midi-unpitched")) {
                    info.unpitched.insert(iid.clone(), u - 1);
                }
                if info.program.is_none() {
                    info.program = num::<u32>(child_text(mi, "midi-program")).map(|p| (p.clamp(1, 128) - 1) as u8);
                }
                if info.channel.is_none() {
                    info.channel = num::<u8>(child_text(mi, "midi-channel"));
                }
            }
            infos.insert(id, info);
        }
    }

    for (pi, part_node) in root.children().filter(|c| c.has_tag_name("part")).enumerate() {
        let id = part_node.attribute("id").map(|s| s.to_string()).unwrap_or_else(|| format!("P{}", pi + 1));
        let info = infos.remove(&id);
        let mut part = Part {
            id: id.clone(),
            name: info.as_ref().map(|i| i.name.clone()).unwrap_or_default(),
            abbreviation: info.as_ref().and_then(|i| i.abbr.clone()),
            program: info.as_ref().and_then(|i| i.program),
            channel: info.as_ref().and_then(|i| i.channel),
            percussion: false,
            staves: Vec::new(),
        };
        let unpitched_map = info.as_ref().map(|i| i.unpitched.clone()).unwrap_or_default();
        let inst_names = info.as_ref().map(|i| i.inst_names.clone()).unwrap_or_default();
        let mut nstaves: u32 = 1;
        for s in part_node.descendants().filter(|n| n.has_tag_name("staves")) {
            nstaves = nstaves.max(num::<u32>(s.text()).unwrap_or(1));
        }
        let mut staves: Vec<Staff> = (1..=nstaves).map(|n| Staff { number: n, measures: Vec::new() }).collect();
        let mut cur_div: u64 = g;
        let mut time = TimeSig::new(4, 4);
        let mut open_ending: Option<String> = None;
        for (mi, mnode) in part_node.children().filter(|c| c.has_tag_name("measure")).enumerate() {
            let number = mnode.attribute("number").and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(mi as u32 + 1);
            let implicit = mnode.attribute("implicit") == Some("yes");
            let mut ms: Vec<Measure> =
                (0..nstaves).map(|_| Measure { number, implicit, ..Measure::default() }).collect();
            let mut cursor: i64 = 0;
            let mut raw: Vec<RawEvent> = Vec::new();
            let mut last_start: i64 = 0;
            let mut dyn_dirs: Vec<(u32, u64, String)> = Vec::new();
            let scale = |d: u64, cur: u64| -> u64 { d * g / cur.max(1) };
            for c in mnode.children().filter(|c| c.is_element()) {
                match c.tag_name().name() {
                    "attributes" => {
                        if let Some(d) = num::<u64>(child_text(c, "divisions")) {
                            cur_div = d.max(1);
                        }
                        for k in c.children().filter(|k| k.has_tag_name("key")) {
                            if let Some(f) = num::<i8>(child_text(k, "fifths")) {
                                let mode =
                                    if child_text(k, "mode") == Some("minor") { Mode::Minor } else { Mode::Major };
                                let ks = KeySig { fifths: f.clamp(-7, 7), mode };
                                match k.attribute("number").and_then(|n| n.parse::<usize>().ok()) {
                                    Some(n) if n >= 1 && n <= ms.len() => ms[n - 1].key = Some(ks),
                                    _ => ms.iter_mut().for_each(|m| m.key = Some(ks)),
                                }
                            } else {
                                unsupported.push(format!("measure {number}: non-traditional key signature"));
                            }
                        }
                        for t in c.children().filter(|k| k.has_tag_name("time")) {
                            let beats: Option<u32> = child_text(t, "beats")
                                .and_then(|b| b.split('+').map(|x| x.trim().parse::<u32>().ok()).sum::<Option<u32>>());
                            let bt = num::<u32>(child_text(t, "beat-type"));
                            if let (Some(b), Some(bt)) = (beats, bt) {
                                time = TimeSig::new(b, bt);
                                ms.iter_mut().for_each(|m| m.time = Some(time));
                            } else if child(t, "senza-misura").is_some() {
                                unsupported.push(format!("measure {number}: senza-misura time"));
                            }
                        }
                        for cl in c.children().filter(|k| k.has_tag_name("clef")) {
                            let n = cl.attribute("number").and_then(|n| n.parse::<usize>().ok()).unwrap_or(1);
                            let sign = child_text(cl, "sign").unwrap_or("G").to_string();
                            let line = num::<u8>(child_text(cl, "line")).unwrap_or(match sign.as_str() {
                                "F" => 4,
                                "C" => 3,
                                "percussion" => 3,
                                "TAB" => 5,
                                _ => 2,
                            });
                            let oc = num::<i8>(child_text(cl, "clef-octave-change")).unwrap_or(0);
                            if sign == "percussion" {
                                part.percussion = true;
                            }
                            if n >= 1 && n <= ms.len() {
                                ms[n - 1].clef = Some(Clef { sign, line, octave_change: oc });
                            }
                        }
                        for tag in ["transpose", "measure-style", "staff-details"] {
                            if child(c, tag).is_some() {
                                unsupported.push(format!("measure {number}: <{tag}> ignored"));
                            }
                        }
                    }
                    "backup" => {
                        let d = scale(num::<u64>(child_text(c, "duration")).unwrap_or(0), cur_div) as i64;
                        cursor = (cursor - d).max(0);
                    }
                    "forward" => {
                        let d = scale(num::<u64>(child_text(c, "duration")).unwrap_or(0), cur_div) as i64;
                        cursor += d;
                    }
                    "harmony" => {
                        let staff = num::<u32>(child_text(c, "staff")).unwrap_or(1).clamp(1, nstaves);
                        let off = num::<i64>(child_text(c, "offset"))
                            .map(|o| scale(o.max(0) as u64, cur_div) as i64)
                            .unwrap_or(0);
                        if let Some(h) = read_harmony(c, (cursor + off).max(0) as u32) {
                            ms[(staff - 1) as usize].harmonies.push(h);
                        }
                    }
                    "direction" => {
                        let staff = num::<u32>(child_text(c, "staff")).unwrap_or(1).clamp(1, nstaves);
                        let off = num::<i64>(child_text(c, "offset"))
                            .map(|o| scale(o.max(0) as u64, cur_div) as i64)
                            .unwrap_or(0);
                        let at = (cursor + off).max(0) as u32;
                        let placement = c.attribute("placement").map(|s| s.to_string());
                        let mut tempo_words: Option<String> = None;
                        let mut tempo: Option<f64> = None;
                        for dt in c.children().filter(|k| k.has_tag_name("direction-type")) {
                            for item in dt.children().filter(|k| k.is_element()) {
                                match item.tag_name().name() {
                                    "dynamics" => {
                                        for dy in item.children().filter(|k| k.is_element()) {
                                            let v = dy.tag_name().name();
                                            let v = if v == "other-dynamics" {
                                                dy.text().unwrap_or("mf").trim()
                                            } else {
                                                v
                                            };
                                            dyn_dirs.push((staff, at as u64, v.to_string()));
                                        }
                                    }
                                    "metronome" => {
                                        let unit = child_text(item, "beat-unit").unwrap_or("quarter");
                                        let dotted = child(item, "beat-unit-dot").is_some();
                                        if let Some(pm) = num_f(child_text(item, "per-minute")) {
                                            let q = crate::pitch::duration_of(unit, dotted as u8, 2, None).unwrap_or(2)
                                                as f64
                                                / 2.0;
                                            tempo = Some(pm * q);
                                        }
                                    }
                                    "words" => {
                                        let w = item.text().unwrap_or("").trim().to_string();
                                        if !w.is_empty() {
                                            tempo_words = Some(match tempo_words {
                                                Some(prev) => format!("{prev} {w}"),
                                                None => w,
                                            });
                                        }
                                    }
                                    "wedge" => {
                                        ms[(staff - 1) as usize].directions.push(Direction {
                                            offset: at,
                                            kind: "wedge".into(),
                                            value: item.attribute("type").unwrap_or("stop").to_string(),
                                            placement: placement.clone(),
                                        });
                                    }
                                    other => {
                                        if !matches!(
                                            other,
                                            "segno"
                                                | "coda"
                                                | "rehearsal"
                                                | "pedal"
                                                | "octave-shift"
                                                | "dashes"
                                                | "bracket"
                                        ) {
                                            unsupported.push(format!("measure {number}: direction <{other}> ignored"));
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(sound) = child(c, "sound") {
                            if tempo.is_none() {
                                tempo = num_f(sound.attribute("tempo"));
                            }
                        }
                        if let Some(bpm) = tempo {
                            ms[0].tempo.push(TempoMark { offset: at, bpm, text: tempo_words.take() });
                        }
                        if let Some(w) = tempo_words {
                            ms[(staff - 1) as usize].directions.push(Direction {
                                offset: at,
                                kind: "words".into(),
                                value: w,
                                placement,
                            });
                        }
                    }
                    "note" => {
                        let is_chord = child(c, "chord").is_some();
                        let grace = child(c, "grace").is_some();
                        let staff = num::<u32>(child_text(c, "staff")).unwrap_or(1).clamp(1, nstaves);
                        let voice = num::<u32>(child_text(c, "voice")).unwrap_or(1);
                        let dur =
                            if grace { 0 } else { scale(num::<u64>(child_text(c, "duration")).unwrap_or(0), cur_div) };
                        let (ev, note) =
                            read_note(c, g as u32, &unpitched_map, &inst_names, dur as u32, &mut part.percussion);
                        if is_chord {
                            if let Some(n) = note {
                                if let Some(last) =
                                    raw.iter_mut().rev().find(|r| r.staff == staff && r.start as i64 == last_start)
                                {
                                    last.event.notes.push(n);
                                    last.event.rest = false;
                                    continue;
                                }
                                // stray chord flag: treat as a new event at the last start
                                let mut ev = ev;
                                ev.notes = vec![n];
                                raw.push(RawEvent { staff, voice, start: last_start.max(0) as u64, event: ev });
                            }
                            continue;
                        }
                        let mut ev = ev;
                        if let Some(n) = note {
                            ev.notes = vec![n];
                        }
                        last_start = cursor;
                        raw.push(RawEvent { staff, voice, start: cursor.max(0) as u64, event: ev });
                        cursor += dur as i64;
                    }
                    "barline" => {
                        let loc = c.attribute("location").unwrap_or("right");
                        let m0 = &mut ms[0];
                        if let Some(r) = child(c, "repeat") {
                            match r.attribute("direction") {
                                Some("forward") => m0.repeat_start = true,
                                Some("backward") => {
                                    m0.repeat_end = Some(r.attribute("times").and_then(|t| t.parse().ok()).unwrap_or(2))
                                }
                                _ => {}
                            }
                        }
                        if let Some(e) = child(c, "ending") {
                            let num = e.attribute("number").unwrap_or("1").to_string();
                            match e.attribute("type").unwrap_or("start") {
                                "start" => {
                                    open_ending = Some(num.clone());
                                    m0.ending = Some(Ending { number: num, kind: "start".into() });
                                }
                                t @ ("stop" | "discontinue") => {
                                    let started_here = m0.ending.as_ref().map(|e| e.kind == "start").unwrap_or(false);
                                    let kind = if started_here { format!("start-{t}") } else { t.to_string() };
                                    m0.ending = Some(Ending { number: num, kind });
                                    open_ending = None;
                                }
                                _ => {}
                            }
                        }
                        if loc == "right" {
                            if let Some(bs) = child_text(c, "bar-style") {
                                let implied = m0.repeat_end.is_some() && bs == "light-heavy";
                                if !implied && bs != "regular" {
                                    m0.barline = Some(bs.to_string());
                                }
                            }
                        }
                    }
                    "print" | "sound" | "listening" | "bookmark" | "grouping" | "link" => {}
                    "figured-bass" => unsupported.push(format!("measure {number}: figured bass ignored")),
                    other => unsupported.push(format!("measure {number}: <{other}> ignored")),
                }
            }
            if ms[0].ending.is_none() {
                if let Some(num) = &open_ending {
                    ms[0].ending = Some(Ending { number: num.clone(), kind: "continue".into() });
                }
            }
            // build voices per staff
            let mlen = time.measure_divisions(g as u32) as u64;
            for (si, m) in ms.iter_mut().enumerate() {
                let sn = si as u32 + 1;
                let mut vnums: Vec<u32> = raw.iter().filter(|r| r.staff == sn).map(|r| r.voice).collect();
                vnums.sort();
                vnums.dedup();
                for (li, vn) in vnums.iter().enumerate() {
                    let mut evs: Vec<&RawEvent> = raw.iter().filter(|r| r.staff == sn && r.voice == *vn).collect();
                    evs.sort_by_key(|r| r.start);
                    let mut out: Vec<Event> = Vec::new();
                    let mut pos = 0u64;
                    for r in evs {
                        if r.start > pos {
                            let mut gap = Event::rest((r.start - pos) as u32);
                            gap.visible = false;
                            out.push(gap);
                            pos = r.start;
                        }
                        out.push(r.event.clone());
                        pos = pos.max(r.start + r.event.duration as u64);
                    }
                    m.voices.push(Voice { number: li as u32 + 1, events: out });
                }
                if m.voices.is_empty() {
                    let mut r = Event::rest(mlen as u32);
                    r.measure_rest = true;
                    r.visible = !raw.is_empty() || si == 0;
                    m.voices.push(Voice { number: 1, events: vec![r] });
                }
                // attach dynamics to the event starting at their offset
                for (st, at, v) in dyn_dirs.iter().filter(|d| d.0 == sn) {
                    let _ = st;
                    let mut placed = false;
                    for voice in m.voices.iter_mut() {
                        let mut pos = 0u64;
                        for e in voice.events.iter_mut() {
                            if pos == *at && e.visible && e.dynamic.is_none() && !e.grace {
                                e.dynamic = Some(v.clone());
                                placed = true;
                                break;
                            }
                            pos += e.duration as u64;
                            if pos > *at {
                                break;
                            }
                        }
                        if placed {
                            break;
                        }
                    }
                    if !placed {
                        m.directions.push(Direction {
                            offset: *at as u32,
                            kind: "dynamic".into(),
                            value: v.clone(),
                            placement: None,
                        });
                    }
                }
            }
            for (si, m) in ms.into_iter().enumerate() {
                staves[si].measures.push(m);
            }
        }
        part.staves = staves;
        if part.name.is_empty() {
            part.name = if part.percussion { "Drums".into() } else { format!("Part {}", pi + 1) };
        }
        score.parts.push(part);
    }
    unsupported.dedup();
    Ok((score, unsupported))
}

fn read_harmony(c: roxmltree::Node, offset: u32) -> Option<Harmony> {
    let root = child(c, "root")?;
    let step = child_text(root, "root-step")?.chars().next()?.to_ascii_uppercase();
    let alter = num::<f64>(child_text(root, "root-alter")).unwrap_or(0.0).round() as i8;
    let kind_node = child(c, "kind");
    let kind = kind_node.and_then(|k| k.text()).unwrap_or("major").trim();
    let add9 = c
        .children()
        .filter(|d| d.has_tag_name("degree"))
        .any(|d| child_text(d, "degree-value") == Some("9") && child_text(d, "degree-type") == Some("add"));
    let quality = chords::quality_from_kind(kind, add9);
    let bass = child(c, "bass").and_then(|b| {
        Some(Spelled {
            step: child_text(b, "bass-step")?.chars().next()?.to_ascii_uppercase(),
            alter: num::<f64>(child_text(b, "bass-alter")).unwrap_or(0.0).round() as i8,
        })
    });
    let root_s = Spelled { step, alter };
    let frame = child(c, "frame").map(|f| {
        let strings = num::<u8>(child_text(f, "frame-strings")).unwrap_or(6);
        let frets = num::<u8>(child_text(f, "frame-frets")).unwrap_or(4);
        let first_fret = num::<u8>(child_text(f, "first-fret")).unwrap_or(1);
        let mut fret_by_string = vec![None; strings as usize];
        for fnode in f.children().filter(|k| k.has_tag_name("frame-note")) {
            if let (Some(s), Some(fr)) =
                (num::<usize>(child_text(fnode, "string")), num::<u8>(child_text(fnode, "fret")))
            {
                if s >= 1 && s <= strings as usize {
                    fret_by_string[strings as usize - s] = Some(fr);
                }
            }
        }
        GuitarFrame { strings, frets, first_fret, fret_by_string, shape: None, capo: None }
    });
    Some(Harmony {
        offset,
        text: chords::chord_text(root_s, &quality, bass),
        root: root_s,
        quality,
        bass,
        frame,
        confidence: None,
    })
}

fn read_note(
    c: roxmltree::Node,
    divisions: u32,
    unpitched_map: &HashMap<String, i32>,
    inst_names: &HashMap<String, String>,
    dur: u32,
    percussion: &mut bool,
) -> (Event, Option<Note>) {
    let mut ev = Event::empty();
    ev.duration = dur;
    ev.grace = child(c, "grace").is_some();
    ev.visible = c.attribute("print-object") != Some("no");
    if let Some(t) = child_text(c, "type") {
        ev.note_type = Some(t.to_string());
    }
    ev.dots = c.children().filter(|k| k.has_tag_name("dot")).count() as u8;
    if let Some(tm) = child(c, "time-modification") {
        let a = num::<u32>(child_text(tm, "actual-notes")).unwrap_or(1);
        let n = num::<u32>(child_text(tm, "normal-notes")).unwrap_or(1);
        ev.tuplet = Some(Tuplet { actual: a, normal: n, start: false, stop: false });
    }
    let mut note: Option<Note> = None;
    if let Some(r) = child(c, "rest") {
        ev.rest = true;
        ev.measure_rest = r.attribute("measure") == Some("yes");
    } else if let Some(p) = child(c, "pitch") {
        let step = child_text(p, "step").and_then(|s| s.chars().next()).unwrap_or('C').to_ascii_uppercase();
        let alter = num::<f64>(child_text(p, "alter")).unwrap_or(0.0).round() as i8;
        let octave = num::<i8>(child_text(p, "octave")).unwrap_or(4);
        note = Some(Note::new(Pitch { step, alter, octave }));
    } else if let Some(u) = child(c, "unpitched") {
        *percussion = true;
        let step = child_text(u, "display-step").and_then(|s| s.chars().next()).unwrap_or('C').to_ascii_uppercase();
        let octave = num::<i8>(child_text(u, "display-octave")).unwrap_or(5);
        let pitch = Pitch { step, alter: 0, octave };
        let head = child_text(c, "notehead").map(|s| s.to_string());
        let inst = child(c, "instrument").and_then(|i| i.attribute("id"));
        let key = inst
            .and_then(|i| unpitched_map.get(i).copied())
            .unwrap_or_else(|| drums::key_for_display(pitch, head.as_deref()) as i32);
        let mut n = Note::new(pitch);
        n.midi = key;
        n.unpitched = true;
        n.instrument =
            inst.and_then(|i| inst_names.get(i).cloned()).or_else(|| Some(drums::gm_drum_name(key as u8).to_string()));
        note = Some(n);
    }
    if let Some(n) = note.as_mut() {
        if !n.unpitched {
            n.notehead = child_text(c, "notehead").filter(|h| *h != "normal").map(|s| s.to_string());
        } else {
            n.notehead = child_text(c, "notehead").filter(|h| *h != "normal").map(|s| s.to_string());
        }
        for t in c.children().filter(|k| k.has_tag_name("tie")) {
            match t.attribute("type") {
                Some("start") => n.tie_start = true,
                Some("stop") => n.tie_stop = true,
                _ => {}
            }
        }
    }
    if let Some(nots) = child(c, "notations") {
        for k in nots.children().filter(|k| k.is_element()) {
            match k.tag_name().name() {
                "tied" => {
                    if let Some(n) = note.as_mut() {
                        match k.attribute("type") {
                            Some("start") => n.tie_start = true,
                            Some("stop") => n.tie_stop = true,
                            _ => {}
                        }
                    }
                }
                "tuplet" => {
                    if let Some(t) = ev.tuplet.as_mut() {
                        match k.attribute("type") {
                            Some("start") => t.start = true,
                            Some("stop") => t.stop = true,
                            _ => {}
                        }
                    }
                }
                "slur" => {
                    if let Some(t) = k.attribute("type") {
                        if t == "start" || t == "stop" {
                            ev.slurs.push(t.to_string());
                        }
                    }
                }
                "articulations" => {
                    for a in k.children().filter(|a| a.is_element()) {
                        let name = match a.tag_name().name() {
                            "strong-accent" => "marcato",
                            "detached-legato" => "legato",
                            other => other,
                        };
                        ev.articulations.push(name.to_string());
                    }
                }
                "fermata" => ev.articulations.push("fermata".into()),
                "dynamics" => {
                    if let Some(d) = k.children().find(|d| d.is_element()) {
                        ev.dynamic = Some(d.tag_name().name().to_string());
                    }
                }
                _ => {}
            }
        }
    }
    for l in c.children().filter(|k| k.has_tag_name("lyric")) {
        let text: Vec<&str> = l.children().filter(|k| k.has_tag_name("text")).filter_map(|t| t.text()).collect();
        let text = text.join("");
        if l.attribute("name") == Some("solfa") {
            ev.solfa = Some(text);
            continue;
        }
        let verse = l.attribute("number").and_then(|n| n.parse::<u32>().ok()).unwrap_or(1);
        ev.lyrics.push(Lyric { text, syllabic: child_text(l, "syllabic").unwrap_or("single").to_string(), verse });
    }
    // drop types that disagree with the duration (keeps written values only when exact)
    if let Some(t) = &ev.note_type {
        let tup = ev.tuplet.map(|t| (t.actual, t.normal));
        if !ev.grace && crate::pitch::duration_of(t, ev.dots, divisions, tup) != Some(ev.duration) {
            // keep: some files write approximate types; but we record the exact type when derivable
            if type_for(ev.duration, divisions, tup).is_some() {
                ev.note_type = None;
                ev.dots = 0;
            }
        } else if !ev.grace
            && type_for(ev.duration, divisions, tup).map(|(tt, d)| tt == t && d == ev.dots).unwrap_or(false)
        {
            // derivable from the duration: store compactly
            ev.note_type = None;
            ev.dots = 0;
        }
    }
    if ev.measure_rest {
        ev.note_type = None;
        ev.dots = 0;
    }
    (ev, note)
}
