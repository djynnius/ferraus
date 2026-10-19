//! MEI 5 (basic) writer and reader: staves, layers, notes, rests, chords, tuplets, ties,
//! key/meter/clef, harmony, dynamics, tempo, repeats and endings. Every duration also
//! carries `dur.ppq` so the reader round-trips exactly.

use crate::chords;
use crate::model::*;
use crate::musicxml::{esc, key_alter};
use crate::pitch::type_for;
use ferraus_core::{Error, Result};
use std::collections::HashMap;
use std::fmt::Write as _;

fn mei_dur(t: &str) -> &'static str {
    match t {
        "breve" => "breve",
        "whole" => "1",
        "half" => "2",
        "quarter" => "4",
        "eighth" => "8",
        "16th" => "16",
        "32nd" => "32",
        "64th" => "64",
        "128th" => "128",
        _ => "4",
    }
}

fn accid(alter: i8) -> &'static str {
    match alter {
        2 => "x",
        1 => "s",
        -1 => "f",
        -2 => "ff",
        _ => "n",
    }
}

fn key_sig(f: i8) -> String {
    if f == 0 {
        "0".into()
    } else if f > 0 {
        format!("{f}s")
    } else {
        format!("{}f", -f)
    }
}

fn dur_attrs(e: &Event, div: u32) -> String {
    let tup = e.tuplet.map(|t| (t.actual, t.normal));
    let mut s = String::new();
    let (t, dots) = match &e.note_type {
        Some(t) => (Some(t.as_str()), e.dots),
        None => match type_for(e.duration, div, tup) {
            Some((t, d)) => (Some(t), d),
            None => (None, 0),
        },
    };
    if let Some(t) = t {
        let _ = write!(s, " dur=\"{}\"", mei_dur(t));
        if dots > 0 {
            let _ = write!(s, " dots=\"{dots}\"");
        }
    }
    let _ = write!(s, " dur.ppq=\"{}\"", e.duration);
    s
}

/// Serialize as MEI 5.
pub fn write(score: &Score) -> String {
    let div = score.divisions;
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<mei xmlns=\"http://www.music-encoding.org/ns/mei\" meiversion=\"5.0\">\n");
    x.push_str("  <meiHead>\n    <fileDesc>\n      <titleStmt>\n");
    let _ = writeln!(x, "        <title>{}</title>", esc(score.title.as_deref().unwrap_or("Untitled")));
    if let Some(c) = &score.composer {
        let _ = writeln!(
            x,
            "        <respStmt>\n          <persName role=\"composer\">{}</persName>\n        </respStmt>",
            esc(c)
        );
    }
    x.push_str("      </titleStmt>\n      <pubStmt/>\n    </fileDesc>\n  </meiHead>\n");
    x.push_str("  <music>\n    <body>\n      <mdiv>\n        <score>\n");
    // staff numbering across parts
    let mut staff_map: Vec<(usize, usize, u32)> = Vec::new(); // (part, staff idx, global n)
    let mut n = 1u32;
    for (pi, p) in score.parts.iter().enumerate() {
        for si in 0..p.staves.len() {
            staff_map.push((pi, si, n));
            n += 1;
        }
    }
    let first = score.parts.first().and_then(|p| p.staves.first());
    let time = first.map(|s| Score::time_at(s, 0)).unwrap_or(TimeSig::new(4, 4));
    let key = score
        .parts
        .iter()
        .find(|p| !p.percussion)
        .and_then(|p| p.staves.first())
        .map(|s| Score::key_at(s, 0))
        .unwrap_or(KeySig::major(0));
    let _ = writeln!(x, "          <scoreDef ppq=\"{div}\">");
    let _ = writeln!(
        x,
        "            <keySig sig=\"{}\" mode=\"{}\"/>",
        key_sig(key.fifths),
        if key.mode == Mode::Minor { "minor" } else { "major" }
    );
    let _ = writeln!(x, "            <meterSig count=\"{}\" unit=\"{}\"/>", time.beats, time.beat_type);
    x.push_str("            <staffGrp>\n");
    for (pi, p) in score.parts.iter().enumerate() {
        let grouped = p.staves.len() > 1;
        if grouped {
            let _ = writeln!(
                x,
                "              <staffGrp symbol=\"brace\" bar.thru=\"true\">\n                <label>{}</label>",
                esc(&p.name)
            );
        }
        for (si, st) in p.staves.iter().enumerate() {
            let gn = staff_map.iter().find(|m| m.0 == pi && m.1 == si).unwrap().2;
            let clef = st.measures.first().and_then(|m| m.clef.clone()).unwrap_or_else(Clef::treble);
            let (shape, line) = match clef.sign.as_str() {
                "percussion" => ("perc".to_string(), 3),
                s => (s.to_string(), clef.line),
            };
            let label = if grouped { String::new() } else { format!(" label=\"{}\"", esc(&p.name)) };
            let _ = writeln!(x, "              <staffDef n=\"{gn}\" lines=\"5\"{label}>");
            let _ = writeln!(x, "                <clef shape=\"{shape}\" line=\"{line}\"/>");
            if let Some(pr) = p.program {
                let _ = writeln!(
                    x,
                    "                <instrDef midi.instrnum=\"{pr}\"{}/>",
                    if p.percussion { " midi.channel=\"9\"" } else { "" }
                );
            } else if p.percussion {
                x.push_str("                <instrDef midi.channel=\"9\"/>\n");
            }
            x.push_str("              </staffDef>\n");
        }
        if grouped {
            x.push_str("              </staffGrp>\n");
        }
    }
    x.push_str("            </staffGrp>\n          </scoreDef>\n          <section>\n");
    let nmeas = score.measure_count();
    let mut open_ending: Option<String> = None;
    let mut cur_time = time;
    let mut cur_key = key;
    for mi in 0..nmeas {
        let m0 = first.and_then(|s| s.measures.get(mi)).cloned().unwrap_or_default();
        if let Some(e) = &m0.ending {
            if e.starts() {
                let _ = writeln!(x, "            <ending n=\"{}\">", esc(&e.number));
                open_ending = Some(e.number.clone());
            }
        }
        // meter/key changes
        let t = first.map(|s| Score::time_at(s, mi)).unwrap_or(time);
        let k = score
            .parts
            .iter()
            .find(|p| !p.percussion)
            .and_then(|p| p.staves.first())
            .map(|s| Score::key_at(s, mi))
            .unwrap_or(key);
        if mi > 0 && (t != cur_time || k != cur_key) {
            let _ = writeln!(x, "            <scoreDef>");
            if k != cur_key {
                let _ = writeln!(
                    x,
                    "              <keySig sig=\"{}\" mode=\"{}\"/>",
                    key_sig(k.fifths),
                    if k.mode == Mode::Minor { "minor" } else { "major" }
                );
            }
            if t != cur_time {
                let _ = writeln!(x, "              <meterSig count=\"{}\" unit=\"{}\"/>", t.beats, t.beat_type);
            }
            x.push_str("            </scoreDef>\n");
            cur_time = t;
            cur_key = k;
        }
        let mut attrs = format!(" n=\"{}\"", m0.number);
        if m0.implicit {
            attrs.push_str(" metcon=\"false\"");
        }
        if m0.repeat_start {
            attrs.push_str(" left=\"rptstart\"");
        }
        if m0.repeat_end.is_some() {
            attrs.push_str(" right=\"rptend\"");
        } else if m0.barline.as_deref() == Some("light-heavy") {
            attrs.push_str(" right=\"end\"");
        } else if m0.barline.as_deref() == Some("light-light") {
            attrs.push_str(" right=\"dbl\"");
        }
        let _ = writeln!(x, "            <measure{attrs}>");
        let mut control = String::new();
        let beat_div = (div * 4 / cur_time.beat_type.max(1)) as f64;
        for (pi, p) in score.parts.iter().enumerate() {
            for (si, st) in p.staves.iter().enumerate() {
                let gn = staff_map.iter().find(|m| m.0 == pi && m.1 == si).unwrap().2;
                let Some(m) = st.measures.get(mi) else { continue };
                let fifths = Score::key_at(st, mi).fifths;
                let _ = writeln!(x, "              <staff n=\"{gn}\">");
                for v in &m.voices {
                    let _ = writeln!(x, "                <layer n=\"{}\">", v.number);
                    let mut acc: HashMap<(char, i8), i8> = HashMap::new();
                    let mut in_tuplet = false;
                    let mut pos = 0u32;
                    for e in &v.events {
                        if let Some(t) = e.tuplet {
                            if t.start && !in_tuplet {
                                let _ = writeln!(
                                    x,
                                    "                  <tuplet num=\"{}\" numbase=\"{}\">",
                                    t.actual, t.normal
                                );
                                in_tuplet = true;
                            }
                        }
                        let tstamp = pos as f64 / beat_div + 1.0;
                        if let Some(d) = &e.dynamic {
                            let _ = writeln!(
                                control,
                                "              <dynam staff=\"{gn}\" tstamp=\"{}\">{}</dynam>",
                                fmt(tstamp),
                                esc(d)
                            );
                        }
                        let ind = if in_tuplet { "                    " } else { "                  " };
                        if e.is_rest() {
                            if e.measure_rest {
                                let _ = writeln!(x, "{ind}<mRest dur.ppq=\"{}\"/>", e.duration);
                            } else if e.visible {
                                let _ = writeln!(x, "{ind}<rest{}/>", dur_attrs(e, div));
                            } else {
                                let _ = writeln!(x, "{ind}<space{}/>", dur_attrs(e, div));
                            }
                        } else {
                            let chord = e.notes.len() > 1;
                            let arts: Vec<&str> = e
                                .articulations
                                .iter()
                                .filter_map(|a| match a.as_str() {
                                    "staccato" => Some("stacc"),
                                    "tenuto" => Some("ten"),
                                    "accent" => Some("acc"),
                                    "marcato" => Some("marc"),
                                    "staccatissimo" => Some("stacciss"),
                                    _ => None,
                                })
                                .collect();
                            let artic =
                                if arts.is_empty() { String::new() } else { format!(" artic=\"{}\"", arts.join(" ")) };
                            let fermata =
                                if e.articulations.iter().any(|a| a == "fermata") { " fermata=\"above\"" } else { "" };
                            let ind2 = if chord {
                                let _ = writeln!(x, "{ind}<chord{}{artic}{fermata}>", dur_attrs(e, div));
                                format!("{ind}  ")
                            } else {
                                ind.to_string()
                            };
                            for nt in &e.notes {
                                let p = nt.pitch;
                                let mut a = String::new();
                                if !chord {
                                    a.push_str(&dur_attrs(e, div));
                                    a.push_str(&artic);
                                    a.push_str(fermata);
                                }
                                let _ = write!(a, " pname=\"{}\" oct=\"{}\"", p.step.to_ascii_lowercase(), p.octave);
                                if p.alter != 0 {
                                    let _ = write!(a, " accid.ges=\"{}\"", accid(p.alter));
                                }
                                if !nt.unpitched && !p_is_perc(p, nt) {
                                    let k = (p.step, p.octave);
                                    let cur = acc.get(&k).copied().unwrap_or_else(|| key_alter(fifths, p.step));
                                    if nt.tie_stop {
                                        acc.insert(k, p.alter);
                                    } else if cur != p.alter {
                                        let _ = write!(a, " accid=\"{}\"", accid(p.alter));
                                        acc.insert(k, p.alter);
                                    }
                                }
                                let tie = match (nt.tie_stop, nt.tie_start) {
                                    (false, true) => " tie=\"i\"",
                                    (true, true) => " tie=\"m\"",
                                    (true, false) => " tie=\"t\"",
                                    _ => "",
                                };
                                a.push_str(tie);
                                if let Some(h) = &nt.notehead {
                                    let shape = match h.as_str() {
                                        "x" => "x",
                                        "circle-x" => "circlex",
                                        "diamond" => "diamond",
                                        "triangle" => "isotriangle",
                                        "slash" => "slash",
                                        _ => "",
                                    };
                                    if !shape.is_empty() {
                                        let _ = write!(a, " head.shape=\"{shape}\"");
                                    }
                                }
                                if nt.unpitched {
                                    let _ = write!(a, " pnum=\"{}\"", nt.midi);
                                }
                                let lyr = if !chord && (!e.lyrics.is_empty() || e.solfa.is_some()) {
                                    lyrics_xml(e, &format!("{ind2}  "))
                                } else {
                                    String::new()
                                };
                                if lyr.is_empty() {
                                    let _ = writeln!(x, "{ind2}<note{a}/>");
                                } else {
                                    let _ = writeln!(x, "{ind2}<note{a}>");
                                    x.push_str(&lyr);
                                    let _ = writeln!(x, "{ind2}</note>");
                                }
                            }
                            if chord {
                                let _ = writeln!(x, "{ind}</chord>");
                            }
                        }
                        if let Some(t) = e.tuplet {
                            if t.stop && in_tuplet {
                                x.push_str("                  </tuplet>\n");
                                in_tuplet = false;
                            }
                        }
                        pos += e.duration;
                    }
                    if in_tuplet {
                        x.push_str("                  </tuplet>\n");
                    }
                    x.push_str("                </layer>\n");
                }
                x.push_str("              </staff>\n");
                for h in &m.harmonies {
                    let _ = writeln!(
                        control,
                        "              <harm staff=\"{gn}\" tstamp=\"{}\">{}</harm>",
                        fmt(h.offset as f64 / beat_div + 1.0),
                        esc(&h.text)
                    );
                }
                for d in &m.directions {
                    let ts = fmt(d.offset as f64 / beat_div + 1.0);
                    match d.kind.as_str() {
                        "dynamic" => {
                            let _ = writeln!(
                                control,
                                "              <dynam staff=\"{gn}\" tstamp=\"{ts}\">{}</dynam>",
                                esc(&d.value)
                            );
                        }
                        "words" => {
                            let _ = writeln!(
                                control,
                                "              <dir staff=\"{gn}\" tstamp=\"{ts}\">{}</dir>",
                                esc(&d.value)
                            );
                        }
                        _ => {}
                    }
                }
                if pi == 0 && si == 0 {
                    for t in &m.tempo {
                        let _ = writeln!(
                            control,
                            "              <tempo staff=\"{gn}\" tstamp=\"{}\" mm=\"{}\" mm.unit=\"4\">♩ = {}</tempo>",
                            fmt(t.offset as f64 / beat_div + 1.0),
                            fmt(t.bpm),
                            fmt(t.bpm)
                        );
                    }
                }
            }
        }
        x.push_str(&control);
        x.push_str("            </measure>\n");
        if let Some(e) = &m0.ending {
            if e.stops() && open_ending.is_some() {
                x.push_str("            </ending>\n");
                open_ending = None;
            }
        }
    }
    if open_ending.is_some() {
        x.push_str("            </ending>\n");
    }
    x.push_str("          </section>\n        </score>\n      </mdiv>\n    </body>\n  </music>\n</mei>\n");
    x
}

fn p_is_perc(_p: Pitch, n: &Note) -> bool {
    n.unpitched
}

fn lyrics_xml(e: &Event, ind: &str) -> String {
    let mut s = String::new();
    for l in &e.lyrics {
        let wordpos = match l.syllabic.as_str() {
            "begin" => " wordpos=\"i\" con=\"d\"",
            "middle" => " wordpos=\"m\" con=\"d\"",
            "end" => " wordpos=\"t\"",
            _ => "",
        };
        let _ =
            writeln!(s, "{ind}<verse n=\"{}\">\n{ind}  <syl{wordpos}>{}</syl>\n{ind}</verse>", l.verse, esc(&l.text));
    }
    if let Some(so) = &e.solfa {
        let _ = writeln!(
            s,
            "{ind}<verse n=\"{}\" type=\"solfa\">\n{ind}  <syl>{}</syl>\n{ind}</verse>",
            e.lyrics.len() + 1,
            esc(so)
        );
    }
    s
}

fn fmt(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

// ------------------------------------------------------------------------------------
// Reader
// ------------------------------------------------------------------------------------

fn parse_key_sig(s: &str) -> i8 {
    if s == "0" {
        return 0;
    }
    let n: i8 = s[..s.len().saturating_sub(1)].parse().unwrap_or(0);
    if s.ends_with('f') {
        -n
    } else {
        n
    }
}

fn alter_of(s: &str) -> i8 {
    match s {
        "s" => 1,
        "x" | "ss" => 2,
        "f" => -1,
        "ff" => -2,
        _ => 0,
    }
}

/// Read MEI (as written by [`write`], and simple MEI 4/5 files).
pub fn read(text: &str) -> Result<Score> {
    let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc =
        roxmltree::Document::parse_with_options(text, opts).map_err(|e| Error::input(format!("not valid MEI: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "mei" {
        return Err(Error::input("not an MEI document"));
    }
    let title =
        root.descendants().find(|n| n.has_tag_name("title")).and_then(|n| n.text()).map(|s| s.trim().to_string());
    let composer = root
        .descendants()
        .find(|n| n.has_tag_name("persName") && n.attribute("role") == Some("composer"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string());
    let score_el =
        root.descendants().find(|n| n.has_tag_name("score")).ok_or_else(|| Error::input("MEI has no <score>"))?;
    let sd = score_el
        .descendants()
        .find(|n| n.has_tag_name("scoreDef"))
        .ok_or_else(|| Error::input("MEI has no <scoreDef>"))?;
    let ppq: Option<u32> = sd.attribute("ppq").and_then(|v| v.parse().ok());
    let div = ppq.unwrap_or(48);
    let mut key = KeySig::major(0);
    let mut time = TimeSig::new(4, 4);
    if let Some(k) = sd.children().find(|n| n.has_tag_name("keySig")) {
        key.fifths = parse_key_sig(k.attribute("sig").unwrap_or("0"));
        if k.attribute("mode") == Some("minor") {
            key.mode = Mode::Minor;
        }
    } else if let Some(k) = sd.attribute("key.sig") {
        key.fifths = parse_key_sig(k);
        if sd.attribute("key.mode") == Some("minor") {
            key.mode = Mode::Minor;
        }
    }
    if let Some(m) = sd.children().find(|n| n.has_tag_name("meterSig")) {
        time = TimeSig::new(
            m.attribute("count").and_then(|v| v.parse().ok()).unwrap_or(4),
            m.attribute("unit").and_then(|v| v.parse().ok()).unwrap_or(4),
        );
    } else if let (Some(c), Some(u)) = (sd.attribute("meter.count"), sd.attribute("meter.unit")) {
        time = TimeSig::new(c.parse().unwrap_or(4), u.parse().unwrap_or(4));
    }
    // staff definitions grouped into parts by brace staffGrps
    struct SDef {
        n: String,
        clef: Clef,
        label: String,
        part: usize,
        program: Option<u8>,
        perc: bool,
    }
    let mut defs: Vec<SDef> = Vec::new();
    let mut part_idx = 0usize;
    if let Some(top) = sd.children().find(|n| n.has_tag_name("staffGrp")) {
        for c in top.children().filter(|c| c.is_element()) {
            let (members, label): (Vec<roxmltree::Node>, String) = if c.has_tag_name("staffGrp") {
                (
                    c.children().filter(|x| x.has_tag_name("staffDef")).collect(),
                    c.children().find(|x| x.has_tag_name("label")).and_then(|l| l.text()).unwrap_or("").to_string(),
                )
            } else if c.has_tag_name("staffDef") {
                (vec![c], c.attribute("label").unwrap_or("").to_string())
            } else {
                continue;
            };
            for s in members {
                let clef_el = s.children().find(|x| x.has_tag_name("clef"));
                let shape = clef_el.and_then(|c| c.attribute("shape")).or(s.attribute("clef.shape")).unwrap_or("G");
                let line: u8 = clef_el
                    .and_then(|c| c.attribute("line"))
                    .or(s.attribute("clef.line"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(2);
                let perc = shape == "perc";
                let clef =
                    if perc { Clef::percussion() } else { Clef { sign: shape.to_string(), line, octave_change: 0 } };
                let instr = s.children().find(|x| x.has_tag_name("instrDef"));
                defs.push(SDef {
                    n: s.attribute("n").unwrap_or("1").to_string(),
                    clef,
                    label: if label.is_empty() {
                        s.attribute("label").unwrap_or("").to_string()
                    } else {
                        label.clone()
                    },
                    part: part_idx,
                    program: instr.and_then(|i| i.attribute("midi.instrnum")).and_then(|v| v.parse().ok()),
                    perc: perc || instr.and_then(|i| i.attribute("midi.channel")) == Some("9"),
                });
            }
            part_idx += 1;
        }
    }
    if defs.is_empty() {
        return Err(Error::input("MEI has no staff definitions"));
    }
    let nparts = part_idx.max(1);
    let mut score = Score { title, composer, divisions: div, ..Score::default() };
    for pi in 0..nparts {
        let members: Vec<&SDef> = defs.iter().filter(|d| d.part == pi).collect();
        let perc = members.iter().any(|d| d.perc);
        score.parts.push(Part {
            id: format!("P{}", pi + 1),
            name: members
                .first()
                .map(|d| d.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| format!("Part {}", pi + 1)),
            abbreviation: None,
            program: members.first().and_then(|d| d.program),
            channel: if perc { Some(10) } else { None },
            percussion: perc,
            staves: members
                .iter()
                .enumerate()
                .map(|(i, _)| Staff { number: i as u32 + 1, measures: Vec::new() })
                .collect(),
        });
    }
    let section = score_el
        .descendants()
        .find(|n| n.has_tag_name("section"))
        .ok_or_else(|| Error::input("MEI has no <section>"))?;
    let mut measures: Vec<(roxmltree::Node, Option<String>)> = Vec::new();
    let mut changes: HashMap<usize, (Option<KeySig>, Option<TimeSig>)> = HashMap::new();
    fn collect<'a, 'i>(
        n: roxmltree::Node<'a, 'i>,
        ending: Option<String>,
        out: &mut Vec<(roxmltree::Node<'a, 'i>, Option<String>)>,
        changes: &mut HashMap<usize, (Option<KeySig>, Option<TimeSig>)>,
    ) {
        for c in n.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "measure" => out.push((c, ending.clone())),
                "ending" => collect(c, c.attribute("n").map(|s| s.to_string()), out, changes),
                "section" => collect(c, ending.clone(), out, changes),
                "scoreDef" => {
                    let k = c.children().find(|x| x.has_tag_name("keySig")).map(|k| KeySig {
                        fifths: parse_key_sig(k.attribute("sig").unwrap_or("0")),
                        mode: if k.attribute("mode") == Some("minor") { Mode::Minor } else { Mode::Major },
                    });
                    let t = c.children().find(|x| x.has_tag_name("meterSig")).map(|m| {
                        TimeSig::new(
                            m.attribute("count").and_then(|v| v.parse().ok()).unwrap_or(4),
                            m.attribute("unit").and_then(|v| v.parse().ok()).unwrap_or(4),
                        )
                    });
                    changes.insert(out.len(), (k, t));
                }
                _ => {}
            }
        }
    }
    collect(section, None, &mut measures, &mut changes);
    let mut cur_time = time;
    let nm = measures.len();
    for (mi, (mnode, ending)) in measures.iter().enumerate() {
        let ch = changes.get(&mi).cloned().unwrap_or((None, None));
        if let Some(t) = ch.1 {
            cur_time = t;
        }
        let beat_div = (div * 4 / cur_time.beat_type.max(1)) as f64;
        let number: u32 = mnode.attribute("n").and_then(|v| v.parse().ok()).unwrap_or(mi as u32 + 1);
        let implicit = mnode.attribute("metcon") == Some("false");
        let ending = ending.as_ref().map(|num| {
            let prev_same = mi > 0 && measures[mi - 1].1.as_deref() == Some(num.as_str());
            let next_same = mi + 1 < nm && measures[mi + 1].1.as_deref() == Some(num.as_str());
            let closed = mnode.attribute("right") == Some("rptend");
            let stop = if closed { "stop" } else { "discontinue" };
            let kind = match (prev_same, next_same) {
                (false, false) => format!("start-{stop}"),
                (false, true) => "start".to_string(),
                (true, true) => "continue".to_string(),
                (true, false) => stop.to_string(),
            };
            Ending { number: num.clone(), kind }
        });
        for d in &defs {
            let part = &mut score.parts[d.part];
            let si = part.staves.iter().position(|_| true).unwrap_or(0);
            let staff_index = defs.iter().filter(|x| x.part == d.part).position(|x| x.n == d.n).unwrap_or(si);
            let staff_node =
                mnode.children().find(|c| c.has_tag_name("staff") && c.attribute("n") == Some(d.n.as_str()));
            let mut m = Measure { number, implicit, ending: ending.clone(), ..Measure::default() };
            if mi == 0 {
                m.clef = Some(d.clef.clone());
                m.time = Some(cur_time);
                if !d.perc {
                    m.key = Some(ch.0.unwrap_or(key));
                }
            } else {
                m.time = ch.1;
                if !d.perc {
                    m.key = ch.0;
                }
            }
            m.repeat_start = mnode.attribute("left") == Some("rptstart");
            m.repeat_end = (mnode.attribute("right") == Some("rptend")).then_some(2);
            m.barline = match mnode.attribute("right") {
                Some("end") => Some("light-heavy".into()),
                Some("dbl") => Some("light-light".into()),
                _ => None,
            };
            if let Some(sn) = staff_node {
                for (li, layer) in sn.children().filter(|c| c.has_tag_name("layer")).enumerate() {
                    let mut events = Vec::new();
                    read_layer(layer, div, cur_time, None, d.perc, &mut events);
                    m.voices.push(Voice { number: li as u32 + 1, events });
                }
            }
            if m.voices.is_empty() {
                let mut r = Event::rest(cur_time.measure_divisions(div));
                r.measure_rest = true;
                m.voices.push(Voice { number: 1, events: vec![r] });
            }
            // control events
            for c in mnode.children().filter(|c| c.is_element() && c.attribute("staff") == Some(d.n.as_str())) {
                let ts: f64 = c.attribute("tstamp").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                let off = ((ts - 1.0) * beat_div).round().max(0.0) as u32;
                let text: String =
                    c.descendants().filter_map(|t| t.text()).collect::<Vec<_>>().join("").trim().to_string();
                match c.tag_name().name() {
                    "harm" => {
                        if let Some((root, q, bass)) = chords::parse_symbol(&text) {
                            m.harmonies.push(Harmony {
                                offset: off,
                                text: chords::chord_text(root, &q, bass),
                                root,
                                quality: q,
                                bass,
                                frame: None,
                                confidence: None,
                            });
                        }
                    }
                    "dynam" => {
                        // attach to the event at the offset in layer 1
                        let mut placed = false;
                        if let Some(v) = m.voices.first_mut() {
                            let mut pos = 0u32;
                            for e in v.events.iter_mut() {
                                if pos == off && e.dynamic.is_none() {
                                    e.dynamic = Some(text.clone());
                                    placed = true;
                                    break;
                                }
                                pos += e.duration;
                            }
                        }
                        if !placed {
                            m.directions.push(Direction {
                                offset: off,
                                kind: "dynamic".into(),
                                value: text,
                                placement: None,
                            });
                        }
                    }
                    "dir" => {
                        m.directions.push(Direction { offset: off, kind: "words".into(), value: text, placement: None })
                    }
                    "tempo" => {
                        if let Some(mm) = c.attribute("mm").and_then(|v| v.parse::<f64>().ok()) {
                            m.tempo.push(TempoMark { offset: off, bpm: mm, text: None });
                        }
                    }
                    _ => {}
                }
            }
            part.staves[staff_index].measures.push(m);
        }
    }
    Ok(score)
}

fn read_layer(
    n: roxmltree::Node,
    div: u32,
    time: TimeSig,
    tuplet: Option<(u32, u32)>,
    perc: bool,
    out: &mut Vec<Event>,
) {
    for c in n.children().filter(|c| c.is_element()) {
        match c.tag_name().name() {
            "tuplet" => {
                let a: u32 = c.attribute("num").and_then(|v| v.parse().ok()).unwrap_or(3);
                let b: u32 = c.attribute("numbase").and_then(|v| v.parse().ok()).unwrap_or(2);
                let start = out.len();
                read_layer(c, div, time, Some((a, b)), perc, out);
                let end = out.len();
                for (i, e) in out[start..end].iter_mut().enumerate() {
                    e.tuplet = Some(Tuplet { actual: a, normal: b, start: i == 0, stop: start + i + 1 == end });
                }
            }
            "beam" => read_layer(c, div, time, tuplet, perc, out),
            "rest" | "space" | "mRest" => {
                let d = dur_of(c, div, tuplet).unwrap_or_else(|| time.measure_divisions(div));
                let mut e = Event::rest(d);
                e.visible = c.tag_name().name() != "space";
                e.measure_rest = c.tag_name().name() == "mRest";
                out.push(e);
            }
            "note" | "chord" => {
                let d = dur_of(c, div, tuplet).unwrap_or(div);
                let note_nodes: Vec<roxmltree::Node> = if c.tag_name().name() == "chord" {
                    c.children().filter(|x| x.has_tag_name("note")).collect()
                } else {
                    vec![c]
                };
                let mut notes = Vec::new();
                let mut lyrics = Vec::new();
                let mut solfa = None;
                for nn in note_nodes {
                    let step = nn.attribute("pname").and_then(|p| p.chars().next()).unwrap_or('c').to_ascii_uppercase();
                    let oct: i8 = nn.attribute("oct").and_then(|v| v.parse().ok()).unwrap_or(4);
                    let alter = alter_of(nn.attribute("accid.ges").or(nn.attribute("accid")).unwrap_or(""));
                    let mut nt = Note::new(Pitch { step, alter, octave: oct });
                    match nn.attribute("tie") {
                        Some("i") => nt.tie_start = true,
                        Some("m") => {
                            nt.tie_start = true;
                            nt.tie_stop = true
                        }
                        Some("t") => nt.tie_stop = true,
                        _ => {}
                    }
                    if let Some(h) = nn.attribute("head.shape") {
                        nt.notehead = Some(match h {
                            "circlex" => "circle-x".into(),
                            "isotriangle" => "triangle".into(),
                            other => other.to_string(),
                        });
                    }
                    if perc {
                        nt.unpitched = true;
                        nt.midi = nn
                            .attribute("pnum")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or_else(|| crate::drums::key_for_display(nt.pitch, nt.notehead.as_deref()) as i32);
                        nt.instrument = Some(crate::drums::gm_drum_name(nt.midi as u8).to_string());
                    }
                    for v in nn.children().filter(|x| x.has_tag_name("verse")) {
                        let syl = v.children().find(|x| x.has_tag_name("syl"));
                        let text = syl.and_then(|s| s.text()).unwrap_or("").to_string();
                        if v.attribute("type") == Some("solfa") {
                            solfa = Some(text);
                            continue;
                        }
                        let syllabic = match syl.and_then(|s| s.attribute("wordpos")) {
                            Some("i") => "begin",
                            Some("m") => "middle",
                            Some("t") => "end",
                            _ => "single",
                        };
                        lyrics.push(Lyric {
                            text,
                            syllabic: syllabic.into(),
                            verse: v.attribute("n").and_then(|x| x.parse().ok()).unwrap_or(1),
                        });
                    }
                    notes.push(nt);
                }
                let mut e = Event::notes(d, notes);
                e.lyrics = lyrics;
                e.solfa = solfa;
                if let Some(a) = c.attribute("artic") {
                    for t in a.split_whitespace() {
                        e.articulations.push(
                            match t {
                                "stacc" => "staccato",
                                "ten" => "tenuto",
                                "acc" => "accent",
                                "marc" => "marcato",
                                "stacciss" => "staccatissimo",
                                other => other,
                            }
                            .to_string(),
                        );
                    }
                }
                if c.attribute("fermata").is_some() {
                    e.articulations.push("fermata".into());
                }
                out.push(e);
            }
            _ => {}
        }
    }
}

fn dur_of(c: roxmltree::Node, div: u32, tuplet: Option<(u32, u32)>) -> Option<u32> {
    if let Some(p) = c.attribute("dur.ppq").and_then(|v| v.parse::<u32>().ok()) {
        return Some(p);
    }
    let d = c.attribute("dur")?;
    let dots: u8 = c.attribute("dots").and_then(|v| v.parse().ok()).unwrap_or(0);
    let t = match d {
        "breve" => "breve",
        "1" => "whole",
        "2" => "half",
        "4" => "quarter",
        "8" => "eighth",
        "16" => "16th",
        "32" => "32nd",
        "64" => "64th",
        "128" => "128th",
        _ => return None,
    };
    crate::pitch::duration_of(t, dots, div, tuplet)
}
