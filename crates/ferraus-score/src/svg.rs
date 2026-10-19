//! A minimal native SVG engraver: the fallback when neither the Verovio helper nor
//! LilyPond is available. It draws every staff of every part with clefs, key and time
//! signatures, noteheads, stems, flags, dots, accidentals, ledger lines, rests, ties,
//! tuplet numbers, chord symbols, lyrics, dynamics, repeats, and endings, with
//! proportional spacing and line breaks. Music symbols come from Unicode (rendered with
//! a music font when the viewer has one); noteheads, stems, and lines are drawn shapes.

use crate::model::*;
use crate::musicxml::{esc, key_alter};
use crate::pitch::{step_index, type_for};
use std::fmt::Write as _;

const SP: f64 = 10.0; // staff space
const PAGE_W: f64 = 1000.0;
const MARGIN: f64 = 40.0;
const FONT: &str = "Bravura, 'Noto Music', 'Segoe UI Symbol', 'Apple Symbols', serif";

fn bottom_line_index(clef: &Clef) -> i32 {
    // diatonic index (octave*7 + step) of the bottom staff line
    match clef.sign.as_str() {
        "F" => 2 * 7 + 4,                              // G2
        "C" => 3 * 7 + 3 + 2 * (3 - clef.line as i32), // F3 for alto (line 3)
        _ => 4 * 7 + 2,                                // E4 (treble and percussion)
    }
}

fn diatonic(p: Pitch) -> i32 {
    p.octave as i32 * 7 + step_index(p.step)
}

struct StaffCtx {
    clef: Clef,
    key: KeySig,
    time: TimeSig,
}

fn measure_len(score: &Score, staves: &[(&Part, &Staff)], mi: usize) -> u32 {
    let mut len = 0u32;
    for (_, st) in staves {
        if let Some(m) = st.measures.get(mi) {
            for v in &m.voices {
                len = len.max(v.events.iter().filter(|e| !e.grace).map(|e| e.duration).sum());
            }
        }
    }
    if len == 0 {
        if let Some((_, st)) = staves.first() {
            return Score::time_at(st, mi).measure_divisions(score.divisions);
        }
    }
    len.max(1)
}

fn natural_width(score: &Score, staves: &[(&Part, &Staff)], mi: usize) -> f64 {
    // the busiest voice decides
    let mut w: f64 = 60.0;
    for (_, st) in staves {
        if let Some(m) = st.measures.get(mi) {
            for v in &m.voices {
                let mut vw = 24.0;
                for e in v.events.iter().filter(|e| !e.grace) {
                    let q = e.duration as f64 / score.divisions as f64;
                    vw += 22.0 + 26.0 * q.sqrt();
                    if e.notes.iter().any(|n| n.pitch.alter != 0) {
                        vw += 6.0;
                    }
                }
                let lyr: f64 = v
                    .events
                    .iter()
                    .flat_map(|e| e.lyrics.first())
                    .map(|l| 8.0 * l.text.chars().count() as f64 + 10.0)
                    .sum();
                w = w.max(vw).max(lyr + 20.0);
            }
            let chords: f64 = m.harmonies.iter().map(|h| 9.0 * h.text.chars().count() as f64 + 12.0).sum();
            w = w.max(chords + 20.0);
        }
    }
    w
}

/// Render the score as one tall SVG page.
pub fn render(score: &Score) -> String {
    let staves: Vec<(&Part, &Staff)> = score.parts.iter().flat_map(|p| p.staves.iter().map(move |s| (p, s))).collect();
    let nmeas = score.measure_count();
    // staff heights: chords above, lyrics below
    let has_chords: Vec<bool> =
        staves.iter().map(|(_, s)| s.measures.iter().any(|m| !m.harmonies.is_empty())).collect();
    let has_lyrics: Vec<bool> = staves
        .iter()
        .map(|(_, s)| {
            s.measures
                .iter()
                .any(|m| m.voices.iter().any(|v| v.events.iter().any(|e| !e.lyrics.is_empty() || e.solfa.is_some())))
        })
        .collect();
    let staff_block: Vec<f64> = (0..staves.len())
        .map(|i| 4.0 * SP + 50.0 + if has_chords[i] { 22.0 } else { 0.0 } + if has_lyrics[i] { 38.0 } else { 0.0 })
        .collect();
    let system_h: f64 = staff_block.iter().sum::<f64>() + 30.0;
    // line breaking
    let widths: Vec<f64> = (0..nmeas).map(|mi| natural_width(score, &staves, mi)).collect();
    let mut systems: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut acc = 0.0;
    for (mi, wd) in widths.iter().enumerate() {
        if !cur.is_empty() && acc + wd > PAGE_W - 2.0 * MARGIN - 110.0 {
            systems.push(std::mem::take(&mut cur));
            acc = 0.0;
        }
        cur.push(mi);
        acc += wd;
    }
    if !cur.is_empty() {
        systems.push(cur);
    }
    let title_h = if score.title.is_some() || score.composer.is_some() { 90.0 } else { 50.0 };
    let height = title_h + systems.len() as f64 * system_h + MARGIN;
    let mut out = String::new();
    let _ = writeln!(out, "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{PAGE_W}\" height=\"{height:.0}\" viewBox=\"0 0 {PAGE_W} {height:.0}\" class=\"ferraus-native\">");
    let _ = writeln!(out, "<rect width=\"100%\" height=\"100%\" fill=\"white\"/>");
    out.push_str("<g fill=\"black\" stroke=\"black\" font-family=\"serif\">\n");
    if let Some(t) = &score.title {
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"40\" font-size=\"24\" text-anchor=\"middle\" stroke=\"none\">{}</text>",
            PAGE_W / 2.0,
            esc(t)
        );
    }
    if let Some(c) = &score.composer {
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"60\" font-size=\"13\" text-anchor=\"end\" stroke=\"none\">{}</text>",
            PAGE_W - MARGIN,
            esc(c)
        );
    }
    let mut ctxs: Vec<StaffCtx> = staves
        .iter()
        .map(|(p, s)| StaffCtx {
            clef: s.measures.first().and_then(|m| m.clef.clone()).unwrap_or_else(|| {
                if p.percussion {
                    Clef::percussion()
                } else {
                    Clef::treble()
                }
            }),
            key: Score::key_at(s, 0),
            time: Score::time_at(s, 0),
        })
        .collect();
    let mut y0 = title_h;
    for (si_sys, sys) in systems.iter().enumerate() {
        let avail = PAGE_W - 2.0 * MARGIN;
        let head_w = 30.0
            + 8.0 * ctxs.iter().map(|c| c.key.fifths.unsigned_abs() as f64).fold(0.0, f64::max)
            + if si_sys == 0 { 30.0 } else { 0.0 }
            + 20.0;
        let total_nat: f64 = sys.iter().map(|m| widths[*m]).sum();
        let last = si_sys + 1 == systems.len();
        let scale = if last && total_nat + head_w < avail * 0.7 { 1.0 } else { (avail - head_w) / total_nat };
        // staff tops
        let mut tops = Vec::new();
        let mut y = y0;
        for i in 0..staves.len() {
            y += if has_chords[i] { 22.0 } else { 0.0 } + 20.0;
            tops.push(y);
            y += 4.0 * SP + 30.0 + if has_lyrics[i] { 38.0 } else { 0.0 };
        }
        let sys_right = MARGIN + head_w + total_nat * scale;
        for (i, (_p, _st)) in staves.iter().enumerate() {
            let top = tops[i];
            for l in 0..5 {
                let yy = top + l as f64 * SP;
                let _ = writeln!(
                    out,
                    "<line x1=\"{MARGIN}\" y1=\"{yy}\" x2=\"{sys_right:.1}\" y2=\"{yy}\" stroke-width=\"1\"/>"
                );
            }
            // header: clef, key, time
            let c = &ctxs[i];
            let mut x = MARGIN + 4.0;
            draw_clef(&mut out, &c.clef, x, top);
            x += 30.0;
            if c.clef.sign != "percussion" {
                x = draw_key(&mut out, c.key, &c.clef, x, top);
            }
            if si_sys == 0 {
                draw_time(&mut out, c.time, x + 4.0, top);
            }
        }
        // system bracket line at the left
        if staves.len() > 1 {
            let _ = writeln!(
                out,
                "<line x1=\"{MARGIN}\" y1=\"{}\" x2=\"{MARGIN}\" y2=\"{}\" stroke-width=\"2\"/>",
                tops[0],
                tops[tops.len() - 1] + 4.0 * SP
            );
        }
        let mut mx = MARGIN + head_w;
        for &mi in sys {
            let w = widths[mi] * scale;
            let len = measure_len(score, &staves, mi);
            for (i, (p, st)) in staves.iter().enumerate() {
                let top = tops[i];
                let Some(m) = st.measures.get(mi) else { continue };
                if let Some(k) = m.key {
                    ctxs[i].key = k;
                }
                if let Some(t) = m.time {
                    ctxs[i].time = t;
                }
                if let Some(c) = &m.clef {
                    ctxs[i].clef = c.clone();
                }
                let ctx = &ctxs[i];
                let xpos = |pos: u32| mx + 18.0 + (pos as f64 / len as f64) * (w - 30.0);
                // time changes inside the system
                if mi > 0 && m.time.is_some() && Some(mi) != sys.first().copied() {
                    draw_time(&mut out, ctx.time, mx + 2.0, top);
                }
                draw_measure(&mut out, score, p, m, ctx, top, &xpos, mx, w);
                // bar line
                let right = mx + w;
                if m.repeat_end.is_some() || m.barline.as_deref() == Some("light-heavy") {
                    let _ = writeln!(
                        out,
                        "<line x1=\"{:.1}\" y1=\"{top}\" x2=\"{:.1}\" y2=\"{}\" stroke-width=\"1\"/>",
                        right - 5.0,
                        right - 5.0,
                        top + 4.0 * SP
                    );
                    let _ = writeln!(
                        out,
                        "<rect x=\"{:.1}\" y=\"{top}\" width=\"3\" height=\"{}\" stroke=\"none\"/>",
                        right - 3.0,
                        4.0 * SP
                    );
                    if m.repeat_end.is_some() {
                        for d in [1.5, 2.5] {
                            let _ = writeln!(
                                out,
                                "<circle cx=\"{:.1}\" cy=\"{}\" r=\"2\" stroke=\"none\"/>",
                                right - 10.0,
                                top + d * SP
                            );
                        }
                    }
                } else if m.barline.as_deref() == Some("light-light") {
                    for dx in [3.0, 0.0] {
                        let _ = writeln!(
                            out,
                            "<line x1=\"{:.1}\" y1=\"{top}\" x2=\"{:.1}\" y2=\"{}\" stroke-width=\"1\"/>",
                            right - dx,
                            right - dx,
                            top + 4.0 * SP
                        );
                    }
                } else {
                    let _ = writeln!(
                        out,
                        "<line x1=\"{right:.1}\" y1=\"{top}\" x2=\"{right:.1}\" y2=\"{}\" stroke-width=\"1\"/>",
                        top + 4.0 * SP
                    );
                }
                if m.repeat_start {
                    let _ = writeln!(
                        out,
                        "<rect x=\"{:.1}\" y=\"{top}\" width=\"3\" height=\"{}\" stroke=\"none\"/>",
                        mx,
                        4.0 * SP
                    );
                    for d in [1.5, 2.5] {
                        let _ = writeln!(
                            out,
                            "<circle cx=\"{:.1}\" cy=\"{}\" r=\"2\" stroke=\"none\"/>",
                            mx + 9.0,
                            top + d * SP
                        );
                    }
                }
                if i == 0 {
                    // measure number at system starts, endings above the first staff
                    if Some(mi) == sys.first().copied() && mi > 0 {
                        let _ = writeln!(
                            out,
                            "<text x=\"{:.1}\" y=\"{}\" font-size=\"10\" stroke=\"none\">{}</text>",
                            mx,
                            top - 6.0 - if has_chords[0] { 18.0 } else { 0.0 },
                            m.number
                        );
                    }
                    if let Some(e) = &m.ending {
                        let ey = top - 18.0 - if has_chords[0] { 18.0 } else { 0.0 };
                        let x1 = mx + 2.0;
                        let x2 = mx + w - 4.0;
                        let _ = writeln!(out, "<polyline points=\"{x1:.1},{:.1} {x1:.1},{ey:.1} {x2:.1},{ey:.1}{}\" fill=\"none\" stroke-width=\"1\"/>", ey + 10.0, if e.kind.ends_with("stop") { format!(" {x2:.1},{:.1}", ey + 10.0) } else { String::new() });
                        if e.starts() {
                            let _ = writeln!(
                                out,
                                "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" stroke=\"none\">{}.</text>",
                                x1 + 4.0,
                                ey + 11.0,
                                esc(&e.number)
                            );
                        }
                    }
                    if mi == 0 || !m.tempo.is_empty() {
                        if let Some(t) = m.tempo.first() {
                            let _ = writeln!(out, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"13\" font-weight=\"bold\" stroke=\"none\">\u{2669} = {}</text>", mx, top - 34.0 - if has_chords[0] { 16.0 } else { 0.0 }, t.bpm.round() as i64);
                        }
                    }
                }
            }
            mx += w;
        }
        y0 += system_h;
    }
    out.push_str("</g>\n</svg>\n");
    out
}

/// Rests drawn as shapes (no music font needed).
fn draw_rest(out: &mut String, kind: &str, x: f64, top: f64) {
    match kind {
        "whole" => {
            let _ = writeln!(
                out,
                "<rect x=\"{x:.1}\" y=\"{:.1}\" width=\"10\" height=\"5\" stroke=\"none\" class=\"rest\"/>",
                top + SP
            );
        }
        "half" => {
            let _ = writeln!(
                out,
                "<rect x=\"{x:.1}\" y=\"{:.1}\" width=\"10\" height=\"5\" stroke=\"none\" class=\"rest\"/>",
                top + 2.0 * SP - 5.0
            );
        }
        "quarter" => {
            let y = top + SP;
            let _ = writeln!(out, "<path d=\"M{:.1},{y:.1} l5,6 l-4,5 l5,6 q-6,-2 -4,4\" fill=\"none\" stroke-width=\"2\" class=\"rest\"/>", x + 2.0);
        }
        other => {
            let flags = match other {
                "eighth" => 1,
                "16th" => 2,
                "32nd" => 3,
                _ => 4,
            };
            let y = top + 1.5 * SP;
            let _ = writeln!(
                out,
                "<line x1=\"{:.1}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke-width=\"1.4\" class=\"rest\"/>",
                x + 9.0,
                x + 4.0,
                y + 8.0 * flags as f64 + 6.0
            );
            for f in 0..flags {
                let fy = y + 8.0 * f as f64;
                let fx = x + 9.0 - 1.2 * f as f64;
                let _ = writeln!(out, "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"2.4\" stroke=\"none\"/><path d=\"M{:.1},{:.1} q4,1 {:.1},-3\" fill=\"none\" stroke-width=\"1.3\"/>", fx - 6.0, fy + 1.5, fx - 6.0, fy + 3.0, 6.0);
            }
        }
    }
}

fn draw_clef(out: &mut String, clef: &Clef, x: f64, top: f64) {
    if clef.sign == "percussion" {
        for dx in [6.0, 12.0] {
            let _ = writeln!(
                out,
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"3\" height=\"{}\" stroke=\"none\" class=\"clef\"/>",
                x + dx,
                top + SP,
                2.0 * SP
            );
        }
        return;
    }
    let (glyph, y, size) = match clef.sign.as_str() {
        "F" => ("\u{1D122}", top + 1.0 * SP + 2.0, 40.0),
        "C" => ("\u{1D121}", top + 4.0 * SP, 40.0),
        "percussion" => ("\u{1D125}", top + 3.5 * SP, 40.0),
        _ => ("\u{1D11E}", top + 3.0 * SP + 4.0, 44.0),
    };
    let _ = writeln!(out, "<text x=\"{x:.1}\" y=\"{y:.1}\" font-size=\"{size}\" font-family=\"{FONT}\" stroke=\"none\" class=\"clef\">{glyph}</text>");
}

fn draw_key(out: &mut String, key: KeySig, clef: &Clef, mut x: f64, top: f64) -> f64 {
    const SHARPS: [char; 7] = ['F', 'C', 'G', 'D', 'A', 'E', 'B'];
    // treble positions (diatonic index) for sharps and flats
    const SHARP_POS: [i32; 7] = [38, 35, 39, 36, 33, 37, 34]; // F5 C5 G5 D5 A4 E5 B4
    const FLAT_POS: [i32; 7] = [34, 37, 33, 36, 32, 35, 31]; // B4 E5 A4 D5 G4 C5 F4
    let shift = bottom_line_index(clef) - (4 * 7 + 2);
    let bottom = top + 4.0 * SP;
    let n = key.fifths.unsigned_abs() as usize;
    for i in 0..n.min(7) {
        let (d, glyph) = if key.fifths > 0 { (SHARP_POS[i], "\u{266F}") } else { (FLAT_POS[i], "\u{266D}") };
        let _ = SHARPS;
        let d = d + shift;
        let y = bottom - (d - bottom_line_index(clef)) as f64 * SP / 2.0;
        let _ = writeln!(out, "<text x=\"{x:.1}\" y=\"{:.1}\" font-size=\"18\" font-family=\"{FONT}\" stroke=\"none\" class=\"keysig\">{glyph}</text>", y + 5.0);
        x += 8.0;
    }
    x + 4.0
}

fn draw_time(out: &mut String, t: TimeSig, x: f64, top: f64) {
    let _ = writeln!(out, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"20\" font-weight=\"bold\" text-anchor=\"middle\" stroke=\"none\" class=\"timesig\">{}</text>", x + 8.0, top + 1.9 * SP, t.beats);
    let _ = writeln!(out, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"20\" font-weight=\"bold\" text-anchor=\"middle\" stroke=\"none\" class=\"timesig\">{}</text>", x + 8.0, top + 3.9 * SP, t.beat_type);
}

#[allow(clippy::too_many_arguments)]
fn draw_measure(
    out: &mut String,
    score: &Score,
    part: &Part,
    m: &Measure,
    ctx: &StaffCtx,
    top: f64,
    xpos: &dyn Fn(u32) -> f64,
    mx: f64,
    w: f64,
) {
    let div = score.divisions;
    let bottom = top + 4.0 * SP;
    let bl = bottom_line_index(&ctx.clef);
    let y_of = |d: i32| bottom - (d - bl) as f64 * SP / 2.0;
    let two = m.voices.len() > 1;
    // chord symbols
    for h in &m.harmonies {
        let _ = writeln!(out, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"14\" font-weight=\"bold\" stroke=\"none\" class=\"harm\">{}</text>", xpos(h.offset) - 4.0, top - 14.0, esc(&h.text));
    }
    for d in &m.directions {
        if d.kind == "dynamic" {
            let _ = writeln!(out, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"14\" font-style=\"italic\" font-weight=\"bold\" stroke=\"none\">{}</text>", xpos(d.offset), bottom + 26.0, esc(&d.value));
        } else if d.kind == "words" {
            let _ = writeln!(
                out,
                "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"12\" font-style=\"italic\" stroke=\"none\">{}</text>",
                xpos(d.offset),
                top - 30.0,
                esc(&d.value)
            );
        }
    }
    let mut acc_state: std::collections::HashMap<(char, i8), i8> = std::collections::HashMap::new();
    for v in &m.voices {
        let mut pos = 0u32;
        let mut tuplet_start_x: Option<f64> = None;
        for e in &v.events {
            if e.grace {
                continue;
            }
            let x = if e.measure_rest { mx + w / 2.0 - 6.0 } else { xpos(pos) };
            let tup = e.tuplet.map(|t| (t.actual, t.normal));
            let (ty, dots) = match &e.note_type {
                Some(t) => (t.clone(), e.dots),
                None => {
                    type_for(e.duration, div, tup).map(|(t, d)| (t.to_string(), d)).unwrap_or(("quarter".into(), 0))
                }
            };
            if let Some(t) = e.tuplet {
                if t.start {
                    tuplet_start_x = Some(x);
                }
                if t.stop {
                    if let Some(sx) = tuplet_start_x.take() {
                        let ty_y = if v.number % 2 == 1 { top - 6.0 } else { bottom + 16.0 };
                        let _ = writeln!(out, "<text x=\"{:.1}\" y=\"{ty_y:.1}\" font-size=\"11\" font-style=\"italic\" text-anchor=\"middle\" stroke=\"none\">{}</text>", (sx + x) / 2.0 + 3.0, t.actual);
                    }
                }
            }
            if e.is_rest() {
                if e.visible {
                    let shift = if two {
                        if v.number % 2 == 1 {
                            -SP
                        } else {
                            SP
                        }
                    } else {
                        0.0
                    };
                    let kind = if e.measure_rest { "whole" } else { ty.as_str() };
                    draw_rest(out, kind, x, top + shift);
                    for k in 0..dots {
                        let _ = writeln!(
                            out,
                            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"1.6\" stroke=\"none\"/>",
                            x + 14.0 + 5.0 * k as f64,
                            top + 1.5 * SP
                        );
                    }
                }
                pos += e.duration;
                continue;
            }
            let filled = !matches!(ty.as_str(), "whole" | "half");
            let ds: Vec<i32> = e.notes.iter().map(|n| diatonic(n.pitch)).collect();
            let mid = bl + 4;
            let up =
                if two { v.number % 2 == 1 } else { (ds.iter().sum::<i32>() as f64 / ds.len() as f64) < mid as f64 };
            // ledger lines
            for d in ds.iter() {
                let mut l = bl - 2;
                while l >= *d {
                    let yy = y_of(l);
                    let _ = writeln!(
                        out,
                        "<line x1=\"{:.1}\" y1=\"{yy:.1}\" x2=\"{:.1}\" y2=\"{yy:.1}\" stroke-width=\"1\"/>",
                        x - 4.0,
                        x + 13.0
                    );
                    l -= 2;
                }
                let mut l = bl + 10;
                while l <= *d {
                    let yy = y_of(l);
                    let _ = writeln!(
                        out,
                        "<line x1=\"{:.1}\" y1=\"{yy:.1}\" x2=\"{:.1}\" y2=\"{yy:.1}\" stroke-width=\"1\"/>",
                        x - 4.0,
                        x + 13.0
                    );
                    l += 2;
                }
            }
            for (ni, n) in e.notes.iter().enumerate() {
                let d = ds[ni];
                let y = y_of(d);
                // seconds in a chord shift to the other side of the stem
                let clash = ds.iter().any(|o| (o - d).abs() == 1 && *o < d) && ni % 2 == 1;
                let nx = if clash {
                    if up {
                        x + 9.0
                    } else {
                        x - 9.0
                    }
                } else {
                    x
                };
                let x_head = n.notehead.as_deref() == Some("x") || n.notehead.as_deref() == Some("circle-x");
                if x_head {
                    let _ = writeln!(out, "<path d=\"M{:.1},{:.1} l9,8 M{:.1},{:.1} l9,-8\" stroke-width=\"1.6\" fill=\"none\" class=\"notehead\"/>", nx, y - 4.0, nx, y + 4.0);
                } else {
                    let fill = if filled { "black" } else { "none" };
                    let _ = writeln!(out, "<ellipse cx=\"{:.1}\" cy=\"{y:.1}\" rx=\"5.6\" ry=\"4.2\" transform=\"rotate(-20 {:.1} {y:.1})\" fill=\"{fill}\" stroke-width=\"1.4\" class=\"notehead\"/>", nx + 4.5, nx + 4.5);
                }
                // accidental display
                if !(n.unpitched || part.percussion) {
                    let k = (n.pitch.step, n.pitch.octave);
                    let cur = acc_state.get(&k).copied().unwrap_or_else(|| key_alter(ctx.key.fifths, n.pitch.step));
                    if n.tie_stop {
                        acc_state.insert(k, n.pitch.alter);
                    } else if cur != n.pitch.alter {
                        let glyph = match n.pitch.alter {
                            1 => "\u{266F}",
                            -1 => "\u{266D}",
                            2 => "\u{1D12A}",
                            -2 => "\u{1D12B}",
                            _ => "\u{266E}",
                        };
                        let _ = writeln!(out, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"16\" font-family=\"{FONT}\" stroke=\"none\" class=\"accid\">{glyph}</text>", nx - 11.0, y + 5.0);
                        acc_state.insert(k, n.pitch.alter);
                    }
                }
                for k in 0..dots {
                    let dy = if d % 2 == bl % 2 { -SP / 2.0 } else { 0.0 };
                    let _ = writeln!(
                        out,
                        "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"1.6\" stroke=\"none\"/>",
                        nx + 14.0 + 5.0 * k as f64,
                        y + dy
                    );
                }
                if n.tie_start {
                    let x2 = (nx + 40.0).min(mx + w + 8.0);
                    let (cy, sgn) = if up { (y + 6.0, 1.0) } else { (y - 6.0, -1.0) };
                    let _ = writeln!(out, "<path d=\"M{:.1},{cy:.1} Q{:.1},{:.1} {x2:.1},{cy:.1}\" fill=\"none\" stroke-width=\"1.2\" class=\"tie\"/>", nx + 9.0, (nx + x2) / 2.0 + 4.0, cy + 6.0 * sgn);
                }
            }
            // stem and flags
            if ty != "whole" {
                let (dmin, dmax) = (ds.iter().copied().min().unwrap_or(mid), ds.iter().copied().max().unwrap_or(mid));
                let (sx, y1, y2) = if up {
                    (x + 9.6, y_of(dmin), y_of(dmax) - 3.5 * SP)
                } else {
                    (x - 0.6, y_of(dmax), y_of(dmin) + 3.5 * SP)
                };
                let _ = writeln!(out, "<line x1=\"{sx:.1}\" y1=\"{y1:.1}\" x2=\"{sx:.1}\" y2=\"{y2:.1}\" stroke-width=\"1.3\" class=\"stem\"/>");
                let flags = match ty.as_str() {
                    "eighth" => 1,
                    "16th" => 2,
                    "32nd" => 3,
                    "64th" => 4,
                    _ => 0,
                };
                for f in 0..flags {
                    let fy = if up { y2 + f as f64 * 7.0 } else { y2 - f as f64 * 7.0 };
                    let dy = if up { 12.0 } else { -12.0 };
                    let _ = writeln!(out, "<path d=\"M{sx:.1},{fy:.1} q7,{:.1} 6,{dy:.1}\" fill=\"none\" stroke-width=\"1.6\" class=\"flag\"/>", dy / 2.0);
                }
            }
            // articulations
            for a in &e.articulations {
                let ay = if up {
                    y_of(ds.iter().copied().min().unwrap_or(mid)) + 12.0
                } else {
                    y_of(ds.iter().copied().max().unwrap_or(mid)) - 12.0
                };
                match a.as_str() {
                    "staccato" => {
                        let _ =
                            writeln!(out, "<circle cx=\"{:.1}\" cy=\"{ay:.1}\" r=\"1.6\" stroke=\"none\"/>", x + 4.5);
                    }
                    "tenuto" => {
                        let _ = writeln!(
                            out,
                            "<line x1=\"{:.1}\" y1=\"{ay:.1}\" x2=\"{:.1}\" y2=\"{ay:.1}\" stroke-width=\"1.4\"/>",
                            x,
                            x + 9.0
                        );
                    }
                    "accent" | "marcato" => {
                        let _ = writeln!(
                            out,
                            "<path d=\"M{:.1},{:.1} l9,3 l-9,3\" fill=\"none\" stroke-width=\"1.2\"/>",
                            x,
                            ay - 3.0
                        );
                    }
                    "fermata" => {
                        let _ = writeln!(out, "<text x=\"{x:.1}\" y=\"{:.1}\" font-size=\"22\" font-family=\"{FONT}\" stroke=\"none\">\u{1D110}</text>", top - 4.0);
                    }
                    _ => {}
                }
            }
            if let Some(dy) = &e.dynamic {
                let _ = writeln!(out, "<text x=\"{x:.1}\" y=\"{:.1}\" font-size=\"14\" font-style=\"italic\" font-weight=\"bold\" stroke=\"none\" class=\"dynam\">{}</text>", bottom + 26.0, esc(dy));
            }
            if v.number == m.voices[0].number {
                let mut ly = bottom + 40.0;
                for l in &e.lyrics {
                    let txt = if l.syllabic == "begin" || l.syllabic == "middle" {
                        format!("{} -", l.text)
                    } else {
                        l.text.clone()
                    };
                    let _ = writeln!(
                        out,
                        "<text x=\"{x:.1}\" y=\"{ly:.1}\" font-size=\"13\" stroke=\"none\" class=\"lyric\">{}</text>",
                        esc(&txt)
                    );
                    ly += 15.0;
                }
                if let Some(s) = &e.solfa {
                    let _ = writeln!(out, "<text x=\"{x:.1}\" y=\"{ly:.1}\" font-size=\"13\" font-style=\"italic\" stroke=\"none\" class=\"solfa\">{}</text>", esc(s));
                }
            }
            pos += e.duration;
        }
    }
}
