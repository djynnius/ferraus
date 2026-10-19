//! Reconstruction: from oemer's segmentation maps and the page's ink to staves, measures,
//! and timed events.
//!
//! oemer ships its notehead/clef/rest/accidental decisions as pickled scikit-learn
//! classifiers; ferraus does not load pickles, so those decisions are made here with small,
//! documented geometric features measured in staff spaces (`sp`), which is enough for clean
//! printed music:
//!
//! * **Staves**: horizontal runs of the network's staff class, grouped into five equally
//!   spaced lines. Staves joined by a vertical line at their left edge form a system.
//! * **Staff removal**: ink pixels on a staff line whose vertical run is no longer than the
//!   line is thick are erased, so symbols separate from the lines.
//! * **Noteheads**: connected regions of the notehead class; tall regions are split into
//!   stacked chord notes, wide ones into the two columns of a chord with a second. A head is
//!   *hollow* (half/whole) when the ink inside its centre is sparse.
//! * **Stems**: the longest vertical ink run leaving the head's right edge upwards or its
//!   left edge downwards, at least 2.2 sp long.
//! * **Beams and flags**: ink runs crossing a column half a staff space beside the stem's
//!   far end; their count gives eighths (1), sixteenths (2), thirty-seconds (3).
//! * **Dots**: small round blobs 0–1.6 sp right of a head or rest.
//! * **Rests**: the remaining blobs inside the staff, by size and position: a box hanging
//!   from the fourth line is a whole rest, one sitting on the middle line a half rest; a
//!   2.4–3.4 sp tall glyph is a quarter (narrow, one piece) or a sixteenth (two flags on its
//!   left), a 1.4–2.4 sp tall one an eighth.
//! * **Clef**: the first glyph of the staff: taller than 5.5 sp is a G clef, a 3.5–4.6 sp
//!   glyph spanning the staff is a C clef, a 2.5–3.7 sp glyph at the top an F clef.
//! * **Key signature**: the sharps/flats after the clef whose letters follow F C G D A E B
//!   (sharps) or B E A D G C F (flats). Sharps have two strokes at the top and bottom, flats
//!   one stroke at the top-left and a bowl at the bottom, naturals a stroke top-left and one
//!   bottom-right.
//! * **Time signature**: two stacked digits (or a C / cut C); digits are told apart by
//!   their holes and outline, then checked against how full the measures are.
//! * **Barlines**: thin vertical lines covering exactly the staff height, away from heads.
//!
//! Unsupported (reported, not guessed): lyrics and text, tuplets, multiple voices on one
//! staff, grace notes, ties and slurs, dynamics, articulations, repeats and endings, clef or
//! key changes inside a system.

use super::bitmap::{BBox, Component, Gray, Mask};
use super::segment::ClassMap;
use crate::model::{Clef, Pitch, TimeSig};

/// Quarter-note subdivisions used for every duration (32nds and their dots are exact).
pub const DIV: u32 = 48;

#[derive(Clone, Debug, PartialEq)]
pub struct StaffLines {
    /// y of the five lines, top to bottom.
    pub ys: [f64; 5],
    pub x0: i64,
    pub x1: i64,
    pub sp: f64,
    pub thickness: f64,
}

impl StaffLines {
    pub fn top(&self) -> f64 {
        self.ys[0]
    }
    pub fn bottom(&self) -> f64 {
        self.ys[4]
    }
    /// Staff position of a y coordinate: 0 = bottom line, 1 = first space, 8 = top line.
    pub fn position(&self, y: f64) -> i32 {
        ((self.bottom() - y) / (self.sp / 2.0)).round() as i32
    }
    pub fn y_of(&self, pos: i32) -> f64 {
        self.bottom() - pos as f64 * self.sp / 2.0
    }
}

/// What the page reader found.
#[derive(Clone, Debug, Default)]
pub struct PageResult {
    pub systems: Vec<SystemResult>,
    /// Page-level problems (no staves, inconsistent systems).
    pub issues: Vec<String>,
    pub staff_space: f64,
}

#[derive(Clone, Debug, Default)]
pub struct SystemResult {
    pub staves: Vec<StaffResult>,
}

#[derive(Clone, Debug, Default)]
pub struct StaffResult {
    pub clef: Option<Clef>,
    /// Key signature in fifths, when one was read.
    pub key: Option<i8>,
    pub time: Option<TimeSig>,
    pub measures: Vec<MeasureResult>,
    pub lines: Option<StaffLines>,
}

#[derive(Clone, Debug, Default)]
pub struct MeasureResult {
    pub events: Vec<Ev>,
    pub confidence: f32,
    pub reasons: Vec<String>,
    /// The measure ends with a final (thin-thick) barline.
    pub final_bar: bool,
}

/// One recognized event (note, chord, or rest) with its duration in [`DIV`]s.
#[derive(Clone, Debug, PartialEq)]
pub struct Ev {
    pub x: f64,
    pub rest: bool,
    pub pitches: Vec<Pitch>,
    /// Per pitch: its alteration comes from the key signature (respelled when the score's
    /// key is decided), not from an accidental in the measure.
    pub from_key: Vec<bool>,
    pub duration: u32,
    pub dots: u8,
    pub note_type: String,
    pub measure_rest: bool,
    pub confidence: f32,
    pub reason: Option<String>,
}

/// Accidentals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acc {
    Sharp,
    Flat,
    Natural,
    DoubleSharp,
}

impl Acc {
    fn alter(self) -> i8 {
        match self {
            Acc::Sharp => 1,
            Acc::Flat => -1,
            Acc::Natural => 0,
            Acc::DoubleSharp => 2,
        }
    }
}

/// Everything the reader needs from one page.
pub struct PageInput<'a> {
    pub gray: &'a Gray,
    /// `omr/oemer`: 0 background, 1 staff, 2 symbols.
    pub staff: &'a ClassMap,
    /// `omr/oemer-seg`: 0 background, 1 stems/rests, 2 noteheads, 3 clefs/keys.
    pub symbols: &'a ClassMap,
}

// ------------------------------------------------------------------------------------
// staff lines
// ------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct LineSeg {
    y0: i64,
    y1: i64,
    x0: i64,
    x1: i64,
}

/// Horizontal line segments: rows' long runs (gaps up to `max_gap` bridged), merged across
/// adjacent rows.
fn line_segments(m: &Mask, min_len: i64, max_gap: i64) -> Vec<LineSeg> {
    let mut rows: Vec<Vec<(i64, i64)>> = Vec::with_capacity(m.h);
    for y in 0..m.h {
        let mut runs = Vec::new();
        let mut start: Option<i64> = None;
        let mut last = -1i64;
        for x in 0..m.w as i64 {
            if m.get(x as usize, y) {
                if start.is_none() {
                    start = Some(x);
                } else if x - last > max_gap + 1 {
                    if last - start.unwrap() + 1 >= min_len {
                        runs.push((start.unwrap(), last + 1));
                    }
                    start = Some(x);
                }
                last = x;
            }
        }
        if let Some(s) = start {
            if last - s + 1 >= min_len {
                runs.push((s, last + 1));
            }
        }
        rows.push(runs);
    }
    // merge vertically adjacent overlapping runs into lines
    let mut done: Vec<LineSeg> = Vec::new();
    let mut open: Vec<LineSeg> = Vec::new();
    for (y, runs) in rows.iter().enumerate() {
        let y = y as i64;
        let mut next_open: Vec<LineSeg> = Vec::new();
        let mut used = vec![false; open.len()];
        for &(a, b) in runs {
            let mut merged = false;
            for (i, l) in open.iter_mut().enumerate() {
                if used[i] {
                    continue;
                }
                let ov = b.min(l.x1) - a.max(l.x0);
                if ov > (b - a).min(l.x1 - l.x0) / 2 {
                    let mut l2 = l.clone();
                    l2.y1 = y + 1;
                    l2.x0 = l2.x0.min(a);
                    l2.x1 = l2.x1.max(b);
                    used[i] = true;
                    next_open.push(l2);
                    merged = true;
                    break;
                }
            }
            if !merged {
                next_open.push(LineSeg { y0: y, y1: y + 1, x0: a, x1: b });
            }
        }
        for (i, l) in open.into_iter().enumerate() {
            if !used[i] {
                done.push(l);
            }
        }
        open = next_open;
    }
    done.extend(open);
    done
}

/// Group line segments into five-line staves.
pub fn find_staves(m: &Mask) -> Vec<StaffLines> {
    let min_len = ((m.w as i64) / 12).max(30);
    let mut lines = line_segments(m, min_len, (m.w as i64 / 150).max(4));
    // a staff line is thin: drop thick blobs (beams, text)
    lines.retain(|l| l.y1 - l.y0 <= 12);
    lines.sort_by(|a, b| (a.y0 + a.y1).cmp(&(b.y0 + b.y1)));
    let cy = |l: &LineSeg| (l.y0 + l.y1 - 1) as f64 / 2.0;
    let mut staves = Vec::new();
    let mut i = 0;
    while i + 5 <= lines.len() {
        // choose the 4 following lines that best continue an equally spaced staff
        let first = &lines[i];
        let mut chosen = vec![i];
        let mut j = i + 1;
        let mut gap: Option<f64> = None;
        while chosen.len() < 5 && j < lines.len() {
            let prev = &lines[*chosen.last().unwrap()];
            let d = cy(&lines[j]) - cy(prev);
            let ov = lines[j].x1.min(first.x1) - lines[j].x0.max(first.x0);
            let overlaps = ov as f64 > 0.5 * (first.x1 - first.x0).min(lines[j].x1 - lines[j].x0) as f64;
            if d < 3.0 || !overlaps {
                j += 1;
                continue;
            }
            match gap {
                None if d <= 60.0 => {
                    gap = Some(d);
                    chosen.push(j);
                }
                Some(g) if (d - g).abs() <= 0.25 * g + 1.0 => {
                    chosen.push(j);
                }
                _ => break,
            }
            j += 1;
        }
        if chosen.len() == 5 {
            let ls: Vec<&LineSeg> = chosen.iter().map(|&k| &lines[k]).collect();
            let mut ys = [0.0; 5];
            for (k, l) in ls.iter().enumerate() {
                ys[k] = cy(l);
            }
            let mut x0s: Vec<i64> = ls.iter().map(|l| l.x0).collect();
            let mut x1s: Vec<i64> = ls.iter().map(|l| l.x1).collect();
            x0s.sort();
            x1s.sort();
            let thickness = ls.iter().map(|l| (l.y1 - l.y0) as f64).sum::<f64>() / 5.0;
            staves.push(StaffLines { ys, x0: x0s[2], x1: x1s[2], sp: (ys[4] - ys[0]) / 4.0, thickness });
            i = *chosen.last().unwrap() + 1;
        } else {
            i += 1;
        }
    }
    staves
}

/// Measure the real line thickness and positions on the ink, and erase the lines.
fn remove_staff_lines(ink: &Mask, staves: &mut [StaffLines]) -> Mask {
    let mut clean = ink.clone();
    for st in staves.iter_mut() {
        // the network labels lines under clefs and barlines as symbols: extend the staff
        // along the ink while at least four of its lines continue
        let on_lines = |x: i64| {
            st.ys.iter().filter(|&&y| (-1..=1).any(|d| ink.get_i(x, y.round() as i64 + d))).count() >= 4
        };
        while st.x0 > 0 && on_lines(st.x0 - 1) {
            st.x0 -= 1;
        }
        while (st.x1 as usize) < ink.w && on_lines(st.x1) {
            st.x1 += 1;
        }
        // thickness from vertical ink runs through the lines
        let mut runs = Vec::new();
        for &ly in &st.ys {
            let y = ly.round() as i64;
            for x in (st.x0..st.x1).step_by(7) {
                if !ink.get_i(x, y) {
                    continue;
                }
                let up = ink.run_from(x, y, -1, 0);
                let down = ink.run_from(x, y, 1, 0);
                runs.push(up + down + 1);
            }
        }
        runs.sort();
        if !runs.is_empty() {
            st.thickness = runs[runs.len() / 3] as f64;
        }
        let t = st.thickness.round() as i64;
        for &ly in &st.ys {
            let yc = ly.round() as i64;
            for x in st.x0 - 2..st.x1 + 2 {
                // find the ink run through the line near yc
                let mut y = None;
                for dy in [0, -1, 1, -2, 2] {
                    if ink.get_i(x, yc + dy) {
                        y = Some(yc + dy);
                        break;
                    }
                }
                let Some(y) = y else { continue };
                let up = ink.run_from(x, y, -1, 0);
                let down = ink.run_from(x, y, 1, 0);
                if up + down < t + 2 {
                    for yy in y - up..=y + down {
                        if yy >= 0 && (yy as usize) < clean.h && x >= 0 && (x as usize) < clean.w {
                            clean.set(x as usize, yy as usize, false);
                        }
                    }
                }
            }
        }
    }
    clean
}

// ------------------------------------------------------------------------------------
// symbols
// ------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Head {
    bbox: BBox,
    pos: i32,
    hollow: bool,
    conf: f32,
    stem: Option<Stem>,
    dots: u8,
    acc: Option<Acc>,
}

#[derive(Clone, Copy, Debug)]
struct Stem {
    x: i64,
    /// -1 up, +1 down.
    dir: i64,
    end: i64,
    beams: u8,
}

#[derive(Clone, Debug)]
struct Rest {
    bbox: BBox,
    /// Duration in DIVs before dots.
    base: u32,
    kind: &'static str,
    dots: u8,
    whole_like: bool,
    conf: f32,
}

fn hollow_score(clean: &Mask, b: &BBox) -> f64 {
    let (w, h) = (b.w() as f64, b.h() as f64);
    let inner = BBox::new(
        (b.cx() - 0.18 * w).round() as i64,
        (b.cy() - 0.15 * h).round() as i64,
        (b.cx() + 0.18 * w).round() as i64 + 1,
        (b.cy() + 0.15 * h).round() as i64 + 1,
    );
    clean.density(&inner)
}

fn split_heads(c: &Component, sp: f64) -> Vec<BBox> {
    let b = c.bbox;
    let mut cols: Vec<BBox> = Vec::new();
    if b.w() as f64 > 2.0 * sp {
        // a chord with a second: split into left and right columns at the narrowest column
        let mid = b.x0 + b.w() / 2;
        for (lo, hi) in [(b.x0, mid), (mid, b.x1)] {
            let mut bb: Option<BBox> = None;
            for &(x, y) in &c.pixels {
                let (x, y) = (x as i64, y as i64);
                if x >= lo && x < hi {
                    match &mut bb {
                        Some(k) => k.include(x, y),
                        None => bb = Some(BBox::new(x, y, x + 1, y + 1)),
                    }
                }
            }
            if let Some(k) = bb {
                cols.push(k);
            }
        }
    } else {
        cols.push(b);
    }
    let mut out = Vec::new();
    for col in cols {
        let n = ((col.h() as f64) / (0.95 * sp)).round().max(1.0) as i64;
        if n <= 1 {
            out.push(col);
            continue;
        }
        let step = col.h() as f64 / n as f64;
        for k in 0..n {
            let y0 = col.y0 + (k as f64 * step).round() as i64;
            let y1 = col.y0 + ((k + 1) as f64 * step).round() as i64;
            out.push(BBox::new(col.x0, y0, col.x1, y1));
        }
    }
    out
}

fn mean_prob(map: &ClassMap, c: &Component) -> f32 {
    if c.pixels.is_empty() {
        return 0.0;
    }
    c.pixels.iter().map(|&(x, y)| map.prob[y as usize * map.w + x as usize]).sum::<f32>() / c.pixels.len() as f32
}

/// The longest vertical ink run leaving `y` along columns `xs` in direction `dir`.
fn best_run(ink: &Mask, xs: std::ops::Range<i64>, y: i64, dir: i64) -> (i64, i64) {
    let mut best = (0, xs.start);
    for x in xs {
        let r = ink.run_from(x, y, dir, 1);
        if r > best.0 {
            best = (r, x);
        }
    }
    best
}

fn find_stem(ink: &Mask, b: &BBox, sp: f64) -> Option<Stem> {
    let y = b.cy().round() as i64;
    let r = (0.35 * sp).round() as i64;
    let e = (0.2 * sp).round().max(1.0) as i64;
    let (up, ux) = best_run(ink, b.x1 - r..b.x1 + e, y, -1);
    let (down, dx) = best_run(ink, b.x0 - e..b.x0 + r, y, 1);
    let min = (2.2 * sp) as i64;
    if up < min && down < min {
        return None;
    }
    if up >= down {
        Some(Stem { x: ux, dir: -1, end: y - up, beams: 0 })
    } else {
        Some(Stem { x: dx, dir: 1, end: y + down, beams: 0 })
    }
}

/// Count beams or flags crossing a column beside the stem near its far end.
fn count_beams(clean: &Mask, stem: &Stem, sp: f64, heads: &[BBox]) -> u8 {
    let mut best = 0u8;
    let min_run = (0.2 * sp).max(2.0) as i64;
    for side in [-1i64, 1] {
        let x = stem.x + side * (0.55 * sp).round() as i64;
        // from just outside the stem end towards the head
        let start = stem.end + stem.dir * -((0.3 * sp) as i64);
        let len = (3.2 * sp) as i64;
        let mut runs = 0u8;
        let mut cur = 0i64;
        let mut first_at: Option<i64> = None;
        let mut gap = 0i64;
        for k in 0..len {
            let yy = if stem.dir < 0 { start + k } else { start - k };
            let in_head = heads.iter().any(|h| h.grow(1, 1).overlaps(&BBox::new(x, yy, x + 1, yy + 1)));
            let on = !in_head && clean.get_i(x, yy);
            if on {
                cur += 1;
                gap = 0;
            } else {
                if cur >= min_run {
                    runs += 1;
                    first_at.get_or_insert(k);
                } else if cur > 0 && first_at.is_none() && k > (0.9 * sp) as i64 {
                    break;
                }
                cur = 0;
                gap += 1;
                if first_at.is_some() && gap as f64 > 0.6 * sp {
                    break;
                }
                if first_at.is_none() && k as f64 > 1.0 * sp {
                    break;
                }
            }
        }
        if cur >= min_run {
            runs += 1;
        }
        best = best.max(runs.min(4));
    }
    best
}

/// Holes in a glyph: background regions inside its box that do not touch the border.
fn holes(m: &Mask) -> Vec<BBox> {
    let mut inv = Mask::new(m.w + 2, m.h + 2);
    for y in 0..m.h + 2 {
        for x in 0..m.w + 2 {
            let ink = x >= 1 && y >= 1 && x <= m.w && y <= m.h && m.get(x - 1, y - 1);
            inv.set(x, y, !ink);
        }
    }
    inv.components()
        .into_iter()
        .filter(|c| c.bbox.x0 > 0 && c.bbox.y0 > 0 && c.bbox.x1 < inv.w as i64 && c.bbox.y1 < inv.h as i64)
        .filter(|c| c.area >= 3)
        .map(|c| BBox::new(c.bbox.x0 - 1, c.bbox.y0 - 1, c.bbox.x1 - 1, c.bbox.y1 - 1))
        .collect()
}

/// Column groups with ink in a band of rows (fraction of height `a..b`).
fn band_runs(m: &Mask, a: f64, b: f64) -> Vec<(f64, f64)> {
    let y0 = (a * m.h as f64).floor() as usize;
    let y1 = ((b * m.h as f64).ceil() as usize).min(m.h).max(y0 + 1);
    let mut cols = vec![false; m.w];
    for y in y0..y1 {
        for (x, c) in cols.iter_mut().enumerate() {
            if m.get(x, y) {
                *c = true;
            }
        }
    }
    let mut out = Vec::new();
    let mut start = None;
    for x in 0..=m.w {
        let on = x < m.w && cols[x];
        match (on, start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                out.push((s as f64 / m.w as f64, x as f64 / m.w as f64));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// Sharp, flat, natural, or double sharp from a glyph mask; `None` when it looks like none.
pub fn classify_accidental(m: &Mask, sp: f64) -> Option<(Acc, f32)> {
    let (w, h) = (m.w as f64 / sp, m.h as f64 / sp);
    if (0.6..=1.3).contains(&w) && (0.6..=1.3).contains(&h) {
        let d = m.count() as f64 / (m.w * m.h).max(1) as f64;
        if d > 0.25 {
            return Some((Acc::DoubleSharp, 0.6));
        }
    }
    if !(1.5..=3.6).contains(&h) || !(0.4..=1.5).contains(&w) {
        return None;
    }
    let top = band_runs(m, 0.0, 0.18);
    let bot = band_runs(m, 0.82, 1.0);
    let mid = band_runs(m, 0.55, 0.75);
    let centre = |r: &[(f64, f64)]| r.iter().map(|(a, b)| (a + b) / 2.0).sum::<f64>() / r.len().max(1) as f64;
    if top.len() >= 2 && bot.len() >= 2 {
        return Some((Acc::Sharp, 0.9));
    }
    if top.len() == 1 && centre(&top) < 0.45 {
        // flat: the bowl fills the lower middle; natural: the right stroke runs to the bottom
        let bot_c = centre(&bot);
        let mid_w: f64 = mid.iter().map(|(a, b)| b - a).sum();
        if bot.len() == 1 && bot_c > 0.6 && top.len() == 1 {
            return Some((Acc::Natural, 0.8));
        }
        if mid.len() >= 2 || mid_w > 0.5 {
            return Some((Acc::Flat, 0.85));
        }
        return Some((Acc::Flat, 0.5));
    }
    if top.len() == 1 && bot.len() == 2 {
        return Some((Acc::Sharp, 0.6));
    }
    if top.len() == 2 && bot.len() == 1 {
        return Some((Acc::Sharp, 0.6));
    }
    None
}

/// The staff position an accidental glyph sits on (the bowl of a flat).
fn acc_position(st: &StaffLines, g: &Component, acc: Acc) -> i32 {
    let b = g.bbox;
    match acc {
        // the bowl of a flat is centred on its note, about 0.7 sp above the glyph's bottom
        // (SMuFL fonts put the flat's bottom at -0.7 sp); staff removal can eat the top
        // of the bowl, so measure from the bottom
        Acc::Flat => st.position(b.y1 as f64 - 0.7 * st.sp),
        _ => st.position(b.cy()),
    }
}

/// Clef from its glyph box.
pub fn classify_clef(st: &StaffLines, b: &BBox) -> Option<(Clef, f32)> {
    let sp = st.sp;
    let h = b.h() as f64 / sp;
    let above = (st.top() - b.y0 as f64) / sp;
    let below = (b.y1 as f64 - st.bottom()) / sp;
    if h >= 5.3 && above > 0.5 && below > 0.5 {
        return Some((Clef::treble(), 0.95));
    }
    if (3.5..=4.8).contains(&h) && above.abs() < 0.7 && below.abs() < 0.7 {
        // a C clef: the line it marks is its vertical centre
        let line = ((st.bottom() - b.cy()) / sp).round() as i64 + 1;
        return Some((Clef { sign: "C".into(), line: line.clamp(1, 5) as u8, octave_change: 0 }, 0.8));
    }
    if (2.3..=3.9).contains(&h) && above.abs() < 0.8 && b.y1 as f64 <= st.bottom() + 0.3 * sp {
        return Some((Clef::bass(), 0.9));
    }
    None
}

/// Diatonic index (octave × 7 + step) of the bottom line for a clef.
fn bottom_line_diatonic(clef: &Clef) -> i32 {
    let base = match (clef.sign.as_str(), clef.line) {
        ("G", 2) => 4 * 7 + 2,     // E4
        ("G", 1) => 4 * 7 + 4,     // G4 (French violin)
        ("F", 4) => 2 * 7 + 4,     // G2
        ("F", 3) => 2 * 7 + 6,     // B2
        ("C", 3) => 3 * 7 + 3,     // F3
        ("C", 4) => 3 * 7 + 1,     // D3
        ("C", 1) => 4 * 7,         // C4 on the bottom line
        ("C", 2) => 3 * 7 + 5,     // A3
        ("C", 5) => 2 * 7 + 6,     // B2
        _ => 4 * 7 + 2,
    };
    base + clef.octave_change as i32 * 7
}

const STEPS: [char; 7] = ['C', 'D', 'E', 'F', 'G', 'A', 'B'];
const SHARP_ORDER: [char; 7] = ['F', 'C', 'G', 'D', 'A', 'E', 'B'];
const FLAT_ORDER: [char; 7] = ['B', 'E', 'A', 'D', 'G', 'C', 'F'];

fn step_of(clef: &Clef, pos: i32) -> (char, i8) {
    let d = bottom_line_diatonic(clef) + pos;
    (STEPS[d.rem_euclid(7) as usize], d.div_euclid(7) as i8)
}

pub fn key_alter_for(fifths: i8, step: char) -> i8 {
    if fifths > 0 && SHARP_ORDER[..fifths as usize].contains(&step) {
        1
    } else if fifths < 0 && FLAT_ORDER[..(-fifths) as usize].contains(&step) {
        -1
    } else {
        0
    }
}

/// Note type name for a duration in DIVs before dots.
fn type_name(base: u32) -> &'static str {
    match base {
        x if x >= 4 * DIV => "whole",
        x if x >= 2 * DIV => "half",
        x if x >= DIV => "quarter",
        x if x >= DIV / 2 => "eighth",
        x if x >= DIV / 4 => "16th",
        _ => "32nd",
    }
}

fn dotted(base: u32, dots: u8) -> u32 {
    match dots {
        0 => base,
        1 => base * 3 / 2,
        _ => base * 7 / 4,
    }
}

/// Ink density of the rows in the fraction `a..b` of the height.
fn row_density(m: &Mask, a: f64, b: f64) -> f64 {
    let y0 = (a * m.h as f64).floor() as usize;
    let y1 = ((b * m.h as f64).ceil() as usize).min(m.h).max(y0 + 1);
    let n: usize = (y0..y1).map(|y| (0..m.w).filter(|&x| m.get(x, y)).count()).sum();
    n as f64 / ((y1 - y0) * m.w).max(1) as f64
}

/// A mask as text (for `FERRAUS_OMR_TRACE`).
fn ascii(m: &Mask) -> String {
    let mut s = String::new();
    for y in 0..m.h {
        for x in 0..m.w {
            s.push(if m.get(x, y) { '#' } else { '.' });
        }
        s.push('\n');
    }
    s
}

/// Time signature digit from its glyph.
pub fn classify_digit(m: &Mask) -> Option<u32> {
    let hs = holes(m);
    let ratio = m.w as f64 / m.h.max(1) as f64;
    if ratio < 0.42 {
        return Some(1);
    }
    let rel = |b: &BBox| b.cy() / m.h as f64;
    match hs.len() {
        2 => Some(8),
        1 => {
            let y = rel(&hs[0]);
            let bottom = band_runs(m, 0.85, 1.0);
            let bottom_w: f64 = bottom.iter().map(|(a, b)| b - a).sum();
            if y > 0.55 {
                Some(6)
            } else if bottom_w < 0.45 && bottom.iter().all(|(a, _)| *a > 0.3) {
                // a 4 stands on a short stem right of centre
                Some(4)
            } else if y < 0.45 {
                Some(9)
            } else {
                Some(0)
            }
        }
        _ => {
            let bottom = band_runs(m, 0.88, 1.0);
            let bottom_w: f64 = bottom.iter().map(|(a, b)| b - a).sum();
            let bottom_c = bottom.iter().map(|(a, b)| (a + b) / 2.0).sum::<f64>() / bottom.len().max(1) as f64;
            // an open 4: a full-width crossbar low down, standing on a stem right of centre
            let crossbar = (0..m.h)
                .filter(|&y| (0.5..0.88).contains(&(y as f64 / m.h as f64)))
                .any(|y| (0..m.w).filter(|&x| m.get(x, y)).count() as f64 >= 0.9 * m.w as f64);
            if crossbar && bottom_w < 0.6 && bottom_c > 0.5 {
                Some(4)
            } else if (0..m.h)
                .filter(|&y| (0.65..0.95).contains(&(y as f64 / m.h as f64)))
                .any(|y| (0..m.w).filter(|&x| m.get(x, y)).count() as f64 >= 0.88 * m.w as f64)
            {
                // the flat foot of a 2
                Some(2)
            } else if row_density(m, 0.0, 0.12) >= 0.75 {
                Some(if bottom_w < 0.45 { 7 } else { 5 })
            } else {
                Some(3)
            }
        }
    }
}

// ------------------------------------------------------------------------------------
// page reader
// ------------------------------------------------------------------------------------

/// `FERRAUS_OMR_TRACE=1` prints the reader's intermediate decisions to stderr.
fn trace() -> bool {
    std::env::var_os("FERRAUS_OMR_TRACE").is_some()
}

/// Threshold for thin strokes (stems, barlines): anti-aliased one-pixel lines are grey.
const SOFT_INK: u8 = 215;

/// Debug drawing collected while reading (boxes with an RGB colour).
#[derive(Default)]
pub struct Debug {
    pub boxes: Vec<(BBox, [u8; 3])>,
}

/// Read one page.
pub fn read_page(input: &PageInput, debug: &mut Debug) -> PageResult {
    let gray = input.gray;
    let ink = gray.binarize(gray.otsu());
    let soft = gray.binarize(SOFT_INK.max(gray.otsu()));
    let mut staves = find_staves(&input.staff.mask(1));
    if staves.is_empty() {
        // the network found no staff lines: fall back to the ink
        staves = find_staves(&ink);
    }
    let mut res = PageResult::default();
    if staves.is_empty() {
        res.issues.push("no staves found on the page".into());
        return res;
    }
    let clean = remove_staff_lines(&ink, &mut staves);
    let mut sps: Vec<f64> = staves.iter().map(|s| s.sp).collect();
    sps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    res.staff_space = sps[sps.len() / 2];

    // systems: consecutive staves joined by a vertical line at the left edge
    let mut groups: Vec<Vec<usize>> = vec![vec![0]];
    for k in 1..staves.len() {
        let (a, b) = (&staves[k - 1], &staves[k]);
        let joined = joined_at_left(&soft, a, b);
        if joined {
            groups.last_mut().unwrap().push(k);
        } else {
            groups.push(vec![k]);
        }
    }
    let notehead_mask = input.symbols.mask(2);
    for g in &groups {
        let mut sys = SystemResult::default();
        let n = g.len();
        for (gi, &si) in g.iter().enumerate() {
            let st = &staves[si];
            let above = if si > 0 { Some(&staves[si - 1]) } else { None };
            let below = staves.get(si + 1);
            let ctx = StaffCtx {
                st,
                ink: &ink,
                soft: &soft,
                clean: &clean,
                heads: &notehead_mask,
                symbols: input.symbols,
                above,
                below,
                joined_above: gi > 0,
                joined_below: gi + 1 < n,
            };
            sys.staves.push(read_staff(&ctx, debug));
        }
        align_system_barlines(&mut sys);
        res.systems.push(sys);
    }
    res
}

fn joined_at_left(ink: &Mask, a: &StaffLines, b: &StaffLines) -> bool {
    let x0 = a.x0.min(b.x0);
    let y0 = a.bottom().round() as i64 + 2;
    let y1 = b.top().round() as i64 - 2;
    if y1 <= y0 {
        return true;
    }
    let r = (a.sp * 0.8) as i64;
    (x0 - r..x0 + r).any(|x| {
        let n = (y0..y1).filter(|&y| ink.get_i(x, y)).count();
        n as f64 >= 0.9 * (y1 - y0) as f64
    })
}

struct StaffCtx<'a> {
    st: &'a StaffLines,
    ink: &'a Mask,
    soft: &'a Mask,
    clean: &'a Mask,
    heads: &'a Mask,
    symbols: &'a ClassMap,
    above: Option<&'a StaffLines>,
    below: Option<&'a StaffLines>,
    joined_above: bool,
    joined_below: bool,
}

/// One barline candidate.
#[derive(Clone, Copy, Debug)]
struct Bar {
    x: f64,
    thick: bool,
}

fn read_staff(c: &StaffCtx, debug: &mut Debug) -> StaffResult {
    let st = c.st;
    let sp = st.sp;
    // vertical zone of this staff
    let half_to = |o: Option<&StaffLines>, up: bool| -> f64 {
        match o {
            Some(o) if up => ((st.top() - o.bottom()) / 2.0).min(5.0 * sp),
            Some(o) => ((o.top() - st.bottom()) / 2.0).min(5.0 * sp),
            None => 5.0 * sp,
        }
    };
    let zone = BBox::new(
        st.x0 - (0.5 * sp) as i64,
        (st.top() - half_to(c.above, true)).round() as i64,
        st.x1 + (0.5 * sp) as i64,
        (st.bottom() + half_to(c.below, false)).round() as i64,
    );

    // ---- noteheads
    let mut heads: Vec<Head> = Vec::new();
    for comp in c.heads.components_in(&zone) {
        let b = comp.bbox;
        if (comp.area as f64) < 0.25 * sp * sp || (b.h() as f64) < 0.45 * sp || (b.w() as f64) < 0.6 * sp {
            continue;
        }
        let conf = mean_prob(c.symbols, &comp);
        for hb in split_heads(&comp, sp) {
            let pos = st.position(hb.cy());
            let hollow = hollow_score(c.clean, &hb) < 0.45;
            heads.push(Head { bbox: hb, pos, hollow, conf, stem: None, dots: 0, acc: None });
        }
    }
    heads.sort_by(|a, b| a.bbox.x0.cmp(&b.bbox.x0));
    let head_boxes: Vec<BBox> = heads.iter().map(|h| h.bbox).collect();
    for h in heads.iter_mut() {
        h.stem = find_stem(c.soft, &h.bbox, sp);
        if let Some(s) = h.stem.as_mut() {
            s.beams = count_beams(c.clean, s, sp, &head_boxes);
        }
    }
    // chords share one stem: use the longest stem's beams for every head on it
    for i in 0..heads.len() {
        let Some(si) = heads[i].stem else { continue };
        for j in 0..heads.len() {
            if let Some(sj) = heads[j].stem {
                if i != j && (si.x - sj.x).abs() as f64 <= 0.3 * sp && si.dir == sj.dir && sj.beams > si.beams {
                    heads[i].stem.as_mut().unwrap().beams = sj.beams;
                }
            }
        }
    }

    // ---- barlines
    let bars = find_barlines(c, &heads);

    // ---- other glyphs on the cleaned ink
    let band = BBox::new(
        st.x0,
        (st.top() - 3.5 * sp).round() as i64,
        st.x1 + 2,
        (st.bottom() + 3.5 * sp).round() as i64,
    );
    let band = BBox::new(band.x0, band.y0.max(zone.y0), band.x1, band.y1.min(zone.y1));
    let mut glyphs: Vec<Component> = c.clean.components_in(&band);
    // ignore stems, beams, heads, barlines: keep glyphs that do not overlap heads
    glyphs.retain(|g| {
        let b = g.bbox;
        !heads.iter().any(|h| h.bbox.overlaps(&b) && h.bbox.grow(1, 1).overlaps(&b))
            && !bars.iter().any(|bar| (b.cx() - bar.x).abs() < 0.4 * sp && b.h() as f64 > 3.0 * sp)
    });
    merge_split_flats(&mut glyphs, sp);
    let mut used = vec![false; glyphs.len()];

    // dots
    let is_dot = |g: &Component| {
        let (w, h) = (g.bbox.w() as f64 / sp, g.bbox.h() as f64 / sp);
        (0.2..=0.75).contains(&w) && (0.2..=0.75).contains(&h) && (w - h).abs() < 0.3 && g.area as f64 >= 0.5 * (g.bbox.w() * g.bbox.h()) as f64
    };

    // local accidentals: glyph right edge just left of a head, at its height
    for hi in 0..heads.len() {
        let hb = heads[hi].bbox;
        let mut best: Option<(usize, Acc)> = None;
        for (gi, g) in glyphs.iter().enumerate() {
            if used[gi] {
                continue;
            }
            let gb = g.bbox;
            let dx = hb.x0 as f64 - gb.x1 as f64;
            if !(-0.25 * sp..=1.2 * sp).contains(&dx) {
                continue;
            }
            let Some((acc, _)) = classify_accidental(&g.local_mask(), sp) else { continue };
            if acc_position(st, g, acc) != heads[hi].pos {
                continue;
            }
            best = Some((gi, acc));
            break;
        }
        if let Some((gi, acc)) = best {
            used[gi] = true;
            heads[hi].acc = Some(acc);
            debug.boxes.push((glyphs[gi].bbox, [255, 0, 255]));
        }
    }

    let first_event_x = heads.first().map(|h| h.bbox.x0).unwrap_or(st.x1);

    // ---- header: clef, key signature, time signature (glyphs left of the first head)
    let mut header: Vec<(BBox, Vec<usize>)> = Vec::new();
    for (gi, g) in glyphs.iter().enumerate() {
        if used[gi] || g.bbox.x1 > first_event_x {
            continue;
        }
        if (g.bbox.h() as f64) < 0.15 * sp {
            continue;
        }
        // measure numbers and text above or below the staff
        if (g.bbox.y1 as f64) < st.top() - 0.2 * sp || (g.bbox.y0 as f64) > st.bottom() + 0.2 * sp {
            continue;
        }
        // merge glyphs that overlap horizontally (clef pieces, stacked digits)
        if let Some(last) = header.iter_mut().find(|(b, _)| b.x_overlap(&g.bbox) as f64 > 0.25 * sp) {
            last.0 = last.0.union(&g.bbox);
            last.1.push(gi);
        } else {
            header.push((g.bbox, vec![gi]));
        }
    }
    header.sort_by(|a, b| a.0.x0.cmp(&b.0.x0));
    // drop the system's left barline and brace (thin full-height lines at the very start)
    header.retain(|(b, _)| !((b.w() as f64) < 0.5 * sp && (b.h() as f64) > 3.5 * sp && (b.x0 - st.x0) as f64 <= 1.0 * sp));
    if trace() {
        eprintln!(
            "staff top {:.1} sp {:.2} x {}..{} first_event_x {first_event_x}",
            st.top(),
            sp,
            st.x0,
            st.x1
        );
        for (b, idx) in &header {
            eprintln!("  header {:?} h {:.2}sp w {:.2}sp parts {}", b, b.h() as f64 / sp, b.w() as f64 / sp, idx.len());
        }
    }
    let mut res = StaffResult { lines: Some(st.clone()), ..Default::default() };
    let mut hi = 0;
    let mut clef_conf = 0.5;
    if let Some((b, idx)) = header.first() {
        if let Some((clef, conf)) = classify_clef(st, b) {
            res.clef = Some(clef);
            clef_conf = conf;
            for &i in idx {
                used[i] = true;
            }
            debug.boxes.push((*b, [255, 128, 0]));
            hi = 1;
        }
    }
    let clef = res.clef.clone().unwrap_or_else(Clef::treble);
    // key signature
    let mut sharps = 0i8;
    let mut flats = 0i8;
    while hi < header.len() {
        let (b, idx) = &header[hi];
        if idx.iter().all(|&i| (glyphs[i].bbox.h() as f64) < 0.8 * sp) {
            // the dots of an F clef, specks
            for &i in idx {
                used[i] = true;
            }
            hi += 1;
            continue;
        }
        if idx.len() != 1 {
            break;
        }
        let g = &glyphs[idx[0]];
        if is_dot(g) || (b.h() as f64) < 0.8 * sp {
            // F clef dots
            used[idx[0]] = true;
            hi += 1;
            continue;
        }
        let class = classify_accidental(&g.local_mask(), sp);
        if trace() {
            eprintln!("  key glyph {:?} -> {:?}\n{}", b, class, ascii(&g.local_mask()));
        }
        let Some((acc, _)) = class else { break };
        let (step, _) = step_of(&clef, acc_position(st, g, acc));
        let ok = match acc {
            Acc::Sharp if flats == 0 && sharps < 7 => SHARP_ORDER[sharps as usize] == step,
            Acc::Flat if sharps == 0 && flats < 7 => FLAT_ORDER[flats as usize] == step,
            _ => false,
        };
        if !ok {
            break;
        }
        if acc == Acc::Sharp {
            sharps += 1;
        } else {
            flats += 1;
        }
        used[idx[0]] = true;
        debug.boxes.push((*b, [0, 160, 255]));
        hi += 1;
    }
    res.key = Some(if sharps > 0 { sharps } else { -flats });
    // time signature
    if let Some((b, idx)) = header.get(hi) {
        if let Some(t) = read_time(c, b, idx, &glyphs) {
            res.time = Some(t);
            for &i in idx {
                used[i] = true;
            }
            debug.boxes.push((*b, [0, 200, 120]));
        }
    }
    let header_end = header
        .iter()
        .enumerate()
        .filter(|(_, (_, idx))| idx.iter().all(|&i| used[i]))
        .map(|(_, (b, _))| b.x1)
        .max()
        .unwrap_or(st.x0);

    // ---- dots after heads
    for h in heads.iter_mut() {
        let hb = h.bbox;
        let mut n = 0u8;
        let mut right = hb.x1 as f64;
        for (gi, g) in glyphs.iter().enumerate() {
            if !is_dot(g) {
                continue;
            }
            let dx = g.bbox.x0 as f64 - right;
            let dy = (g.bbox.cy() - hb.cy()).abs();
            if (-0.1 * sp..=if n == 0 { 1.4 * sp } else { 0.6 * sp }).contains(&dx) && dy <= 0.8 * sp {
                if h.stem.map(|s| s.dir < 0 && (g.bbox.x0 - s.x).abs() as f64 <= 0.2 * sp).unwrap_or(false) {
                    continue;
                }
                used[gi] = true;
                n += 1;
                right = g.bbox.x1 as f64;
                debug.boxes.push((g.bbox, [255, 255, 0]));
                if n == 2 {
                    break;
                }
            }
        }
        h.dots = n;
    }

    // ---- rests
    let mut rests: Vec<Rest> = Vec::new();
    for (gi, g) in glyphs.iter().enumerate() {
        if used[gi] || g.bbox.x0 < header_end {
            continue;
        }
        let r = classify_rest(st, g, c.symbols);
        if trace() && r.is_some() {
            eprintln!("  rest {:?} -> {:?}\n{}", g.bbox, r.as_ref().map(|r| r.kind), ascii(&g.local_mask()));
        }
        if let Some(r) = r {
            used[gi] = true;
            debug.boxes.push((g.bbox, [0, 200, 0]));
            rests.push(r);
        }
    }
    for r in rests.iter_mut() {
        let rb = r.bbox;
        let mut right = rb.x1 as f64;
        for (gi, g) in glyphs.iter().enumerate() {
            if !is_dot(g) || used[gi] {
                continue;
            }
            let dx = g.bbox.x0 as f64 - right;
            if (-0.1 * sp..=1.0 * sp).contains(&dx) && g.bbox.cy() > st.top() - 0.5 * sp && g.bbox.cy() < st.bottom() {
                used[gi] = true;
                r.dots += 1;
                right = g.bbox.x1 as f64;
                if r.dots == 2 {
                    break;
                }
            }
        }
    }

    for h in &heads {
        debug.boxes.push((h.bbox, if h.hollow { [255, 120, 120] } else { [220, 0, 0] }));
        if let Some(s) = h.stem {
            let (a, b) = if s.dir < 0 { (s.end, h.bbox.cy() as i64) } else { (h.bbox.cy() as i64, s.end) };
            debug.boxes.push((BBox::new(s.x, a, s.x + 1, b), [0, 0, 255]));
            for k in 0..s.beams {
                let y = s.end - s.dir * -((k as f64 * 0.75 * sp) as i64);
                debug.boxes.push((BBox::new(s.x - 3, y - 1, s.x + 3, y + 1), [0, 0, 255]));
            }
        }
    }
    for b in &bars {
        debug.boxes.push((
            BBox::new(b.x as i64 - 1, st.top() as i64, b.x as i64 + 2, st.bottom() as i64),
            [200, 0, 200],
        ));
    }

    // ---- events
    let mut events: Vec<Ev> = Vec::new();
    // staff positions and accidentals of each note event (spelled per measure below)
    let mut pos_of: Vec<Option<Vec<(i32, Option<Acc>)>>> = Vec::new();
    let mut taken = vec![false; heads.len()];
    for i in 0..heads.len() {
        if taken[i] {
            continue;
        }
        let mut members = vec![i];
        taken[i] = true;
        for j in i + 1..heads.len() {
            if taken[j] {
                continue;
            }
            let same = match (heads[i].stem, heads[j].stem) {
                (Some(a), Some(b)) => (a.x - b.x).abs() as f64 <= 0.35 * sp && a.dir == b.dir,
                (None, None) => (heads[i].bbox.cx() - heads[j].bbox.cx()).abs() <= 0.8 * sp,
                _ => false,
            };
            if same {
                members.push(j);
                taken[j] = true;
            }
        }
        let hs: Vec<&Head> = members.iter().map(|&k| &heads[k]).collect();
        let stem = hs.iter().find_map(|h| h.stem);
        let hollow_votes = hs.iter().filter(|h| h.hollow).count();
        let hollow = hollow_votes * 2 > hs.len();
        let beams = stem.map(|s| s.beams).unwrap_or(0);
        let mut conf = hs.iter().map(|h| h.conf).fold(1.0f32, f32::min);
        let mut reason = None;
        let base = match (hollow, stem.is_some()) {
            (true, false) => 4 * DIV,
            (true, true) => {
                if beams > 0 {
                    conf *= 0.6;
                    reason = Some("hollow notehead with beams".to_string());
                }
                2 * DIV
            }
            (false, true) => DIV >> beams.min(3),
            (false, false) => {
                conf *= 0.5;
                reason = Some("filled notehead without a stem".to_string());
                DIV
            }
        };
        let dots = hs.iter().map(|h| h.dots).max().unwrap_or(0);
        let x = hs.iter().map(|h| h.bbox.cx()).sum::<f64>() / hs.len() as f64;
        let mut ps: Vec<(i32, Option<Acc>)> = hs.iter().map(|h| (h.pos, h.acc)).collect();
        ps.sort_by_key(|p| p.0);
        events.push(Ev {
            x,
            rest: false,
            pitches: Vec::new(),
            from_key: Vec::new(),
            duration: dotted(base, dots),
            dots,
            note_type: type_name(base).into(),
            measure_rest: false,
            confidence: conf,
            reason,
        });
        pos_of.push(Some(ps));
    }
    for r in &rests {
        events.push(Ev {
            x: r.bbox.cx(),
            rest: true,
            pitches: Vec::new(),
            from_key: Vec::new(),
            duration: dotted(r.base, r.dots),
            dots: r.dots,
            note_type: r.kind.into(),
            measure_rest: r.whole_like,
            confidence: r.conf,
            reason: None,
        });
        pos_of.push(None);
    }
    let mut order: Vec<usize> = (0..events.len()).collect();
    order.sort_by(|&a, &b| events[a].x.partial_cmp(&events[b].x).unwrap());

    // ---- measures
    let mut edges: Vec<f64> = bars.iter().map(|b| b.x).collect();
    edges.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let last_bar = edges.last().copied().unwrap_or(st.x0 as f64);
    let tail_has_events = order.iter().any(|&i| events[i].x > last_bar);
    if edges.is_empty() || tail_has_events {
        edges.push(st.x1 as f64 + 1.0);
    }
    let key = res.key.unwrap_or(0);
    let mut start = st.x0 as f64;
    for (bi, &edge) in edges.iter().enumerate() {
        let mut m = MeasureResult { confidence: 1.0, ..Default::default() };
        m.final_bar = bars.iter().any(|b| (b.x - edge).abs() < 0.5 && b.thick);
        let mut local: Vec<((char, i8), i8)> = Vec::new();
        for &i in &order {
            let e = &events[i];
            if e.x <= start || e.x > edge {
                continue;
            }
            let mut e = e.clone();
            if let Some(ps) = &pos_of[i] {
                for &(pos, acc) in ps {
                    let (step, oct) = step_of(&clef, pos);
                    let (alter, from_key) = match acc {
                        Some(a) => {
                            local.retain(|(k, _)| *k != (step, oct));
                            local.push(((step, oct), a.alter()));
                            (a.alter(), false)
                        }
                        None => match local.iter().find(|(k, _)| *k == (step, oct)) {
                            Some((_, a)) => (*a, false),
                            None => (key_alter_for(key, step), true),
                        },
                    };
                    e.pitches.push(Pitch { step, alter, octave: oct });
                    e.from_key.push(from_key);
                }
            }
            m.events.push(e);
        }
        start = edge;
        if m.events.is_empty() && bi == 0 && edges.len() > 1 {
            // nothing before the first barline (a system that starts with a barline)
            continue;
        }
        res.measures.push(m);
    }
    for m in res.measures.iter_mut() {
        if clef_conf < 0.9 {
            m.confidence *= clef_conf;
        }
        for e in &m.events {
            m.confidence = m.confidence.min(e.confidence.max(0.05));
            if let Some(r) = &e.reason {
                if !m.reasons.contains(r) {
                    m.reasons.push(r.clone());
                }
            }
        }
    }
    if res.clef.is_none() {
        res.measures.iter_mut().take(1).for_each(|m| m.reasons.push("clef not recognized; treble assumed".into()));
    }
    res
}

fn read_time(c: &StaffCtx, b: &BBox, idx: &[usize], glyphs: &[Component]) -> Option<TimeSig> {
    let st = c.st;
    let sp = st.sp;
    let mid = st.ys[2];
    if (b.h() as f64) > 3.0 * sp && (b.h() as f64) < 4.8 * sp && (b.cy() - mid).abs() < 0.7 * sp {
        // stacked digits; they may touch across the middle line, so split the ink there
        let mut halves = [Mask::new(b.w() as usize, b.h() as usize), Mask::new(b.w() as usize, b.h() as usize)];
        for &i in idx {
            for &(x, y) in &glyphs[i].pixels {
                let half = (y as f64 >= mid) as usize;
                halves[half].set((x as i64 - b.x0) as usize, (y as i64 - b.y0) as usize, true);
            }
        }
        let num = |m: &Mask| -> Option<u32> {
            // pieces of one digit (cut by the staff lines) overlap horizontally: merge them
            let mut comps: Vec<Component> = Vec::new();
            let mut parts = m.components();
            parts.sort_by_key(|g| std::cmp::Reverse(g.area));
            for g in parts {
                match comps.iter_mut().find(|c| 2 * c.bbox.x_overlap(&g.bbox) > c.bbox.w().min(g.bbox.w())) {
                    Some(c) => {
                        c.bbox = c.bbox.union(&g.bbox);
                        c.area += g.area;
                        c.pixels.extend(g.pixels);
                    }
                    None => comps.push(g),
                }
            }
            comps.retain(|g| (g.bbox.h() as f64) >= 0.8 * sp);
            comps.sort_by_key(|g| g.bbox.x0);
            let mut n = 0u32;
            for g in &comps {
                let d = classify_digit(&g.local_mask())?;
                if trace() {
                    eprintln!("  time digit {:?} -> {d} (holes {:?})\n{}", g.bbox, holes(&g.local_mask()), ascii(&g.local_mask()));
                }
                n = n * 10 + d;
            }
            (!comps.is_empty()).then_some(n)
        };
        let beats = num(&halves[0])?;
        let beat_type = num(&halves[1])?;
        let beat_type = match beat_type {
            2 | 4 | 8 | 16 => beat_type,
            0 | 9 | 6 => 8,
            _ => 4,
        };
        if beats == 0 || beats > 16 {
            return None;
        }
        return Some(TimeSig::new(beats, beat_type));
    }
    if idx.len() == 1 {
        // common time (C) or cut time (C with a vertical stroke)
        let h = b.h() as f64 / sp;
        let w = b.w() as f64 / sp;
        if (1.6..=2.6).contains(&h) && (1.0..=2.2).contains(&w) && (b.cy() - mid).abs() < 0.5 * sp {
            return Some(TimeSig::new(4, 4));
        }
        if (2.6..=3.8).contains(&h) && (1.0..=2.2).contains(&w) && (b.cy() - mid).abs() < 0.6 * sp {
            return Some(TimeSig::new(2, 2));
        }
    }
    None
}

fn classify_rest(st: &StaffLines, g: &Component, symbols: &ClassMap) -> Option<Rest> {
    let sp = st.sp;
    let b = g.bbox;
    let (w, h) = (b.w() as f64 / sp, b.h() as f64 / sp);
    // the glyph must sit inside the staff
    if b.cy() < st.top() - 1.0 * sp || b.cy() > st.bottom() + 1.0 * sp {
        return None;
    }
    let density = g.area as f64 / (b.w() * b.h()).max(1) as f64;
    // share of the glyph the network calls stems/rests (class 1)
    let restish = g.pixels.iter().filter(|&&(x, y)| symbols.class[y as usize * symbols.w + x as usize] == 1).count()
        as f64
        / g.area.max(1) as f64;
    let conf = (0.6 + 0.4 * restish.min(1.0)) as f32;
    if (0.8..=1.8).contains(&w) && (0.3..=0.8).contains(&h) && density > 0.7 {
        // a box: whole rest hangs from the fourth line, half rest sits on the middle line
        let hang = (b.y0 as f64 - st.ys[1]).abs();
        let sit = (b.y1 as f64 - st.ys[2]).abs();
        return Some(if hang <= sit {
            Rest { bbox: b, base: 4 * DIV, kind: "whole", dots: 0, whole_like: true, conf }
        } else {
            Rest { bbox: b, base: 2 * DIV, kind: "half", dots: 0, whole_like: false, conf }
        });
    }
    if restish < 0.2 {
        return None;
    }
    if (2.3..=3.5).contains(&h) && (0.6..=1.6).contains(&w) {
        // quarter rest (a zigzag ending in a wide hook) vs sixteenth rest (two flags on the
        // left of a slanted stem that ends narrow at the bottom left)
        let m = g.local_mask();
        let bottom: f64 = band_runs(&m, 0.72, 0.9).iter().map(|(a, b)| b - a).sum();
        return Some(if bottom < 0.5 {
            Rest { bbox: b, base: DIV / 4, kind: "16th", dots: 0, whole_like: false, conf }
        } else {
            Rest { bbox: b, base: DIV, kind: "quarter", dots: 0, whole_like: false, conf }
        });
    }
    if (1.3..=2.3).contains(&h) && (0.5..=1.4).contains(&w) {
        return Some(Rest { bbox: b, base: DIV / 2, kind: "eighth", dots: 0, whole_like: false, conf });
    }
    if (3.5..=4.6).contains(&h) && (0.8..=1.8).contains(&w) {
        return Some(Rest { bbox: b, base: DIV / 8, kind: "32nd", dots: 0, whole_like: false, conf: conf * 0.8 });
    }
    None
}

/// Rejoin flats whose thin stem was cut from the bowl where the bowl meets a staff line.
fn merge_split_flats(glyphs: &mut Vec<Component>, sp: f64) {
    loop {
        let mut pair = None;
        'outer: for (i, a) in glyphs.iter().enumerate() {
            let ab = a.bbox;
            if (ab.w() as f64) > 0.3 * sp || !(1.5..=3.2).contains(&(ab.h() as f64 / sp)) {
                continue;
            }
            for (j, b) in glyphs.iter().enumerate() {
                let bb = b.bbox;
                let gap = bb.x0 - ab.x1;
                if i != j
                    && gap >= -1
                    && gap as f64 <= 0.3 * sp
                    && (0.6..=1.4).contains(&(bb.h() as f64 / sp))
                    && (0.3..=1.0).contains(&(bb.w() as f64 / sp))
                    && ((bb.y1 - ab.y1).abs() as f64) <= 0.4 * sp
                {
                    pair = Some((i, j));
                    break 'outer;
                }
            }
        }
        let Some((i, j)) = pair else { break };
        let b = glyphs.remove(i.max(j));
        let a = &mut glyphs[i.min(j)];
        a.bbox = a.bbox.union(&b.bbox);
        a.area += b.area;
        a.pixels.extend(b.pixels);
    }
}

/// Number of separate blobs touching the left third of a glyph (flags of 8th/16th rests).
fn left_flag_count(m: &Mask) -> usize {
    let lim = m.w / 2;
    let mut sub = Mask::new(lim.max(1), m.h);
    for y in 0..m.h {
        for x in 0..lim {
            if m.get(x, y) {
                sub.set(x, y, true);
            }
        }
    }
    sub.components().into_iter().filter(|c| c.area >= 4 && (c.bbox.h() as usize) * 5 >= m.h).count()
}

fn find_barlines(c: &StaffCtx, heads: &[Head]) -> Vec<Bar> {
    let st = c.st;
    let sp = st.sp;
    let y0 = st.top().round() as i64;
    let y1 = st.bottom().round() as i64;
    let span = (y1 - y0 + 1) as f64;
    let mut cols: Vec<i64> = Vec::new();
    for x in st.x0 - 1..=st.x1 + 1 {
        let n = (y0..=y1).filter(|&y| c.soft.get_i(x, y)).count();
        if n as f64 >= 0.95 * span {
            cols.push(x);
        }
    }
    // group adjacent columns into lines
    let mut lines: Vec<(i64, i64)> = Vec::new();
    for x in cols {
        match lines.last_mut() {
            Some(l) if x <= l.1 + 1 => l.1 = x,
            _ => lines.push((x, x)),
        }
    }
    let mut bars: Vec<Bar> = Vec::new();
    for (a, b) in lines {
        let w = (b - a + 1) as f64;
        if w > 0.8 * sp {
            continue;
        }
        let x = (a + b) as f64 / 2.0;
        // away from noteheads (stems)
        if heads.iter().any(|h| h.bbox.x0 as f64 - 0.6 * sp <= x && x <= h.bbox.x1 as f64 + 0.6 * sp) {
            continue;
        }
        // a barline stops at the staff unless the system continues it to a joined staff
        let probe = (0.7 * sp) as i64;
        let xi = x.round() as i64;
        let ext_up = (xi - 1..=xi + 1).any(|xx| c.soft.get_i(xx, y0 - probe));
        let ext_down = (xi - 1..=xi + 1).any(|xx| c.soft.get_i(xx, y1 + probe));
        if (ext_up && !c.joined_above) || (ext_down && !c.joined_below) {
            continue;
        }
        bars.push(Bar { x, thick: w >= 0.3 * sp });
    }
    // the staff's own left edge (system line) is not a measure boundary
    bars.retain(|b| b.x - st.x0 as f64 > 1.0 * sp);
    // merge double / final barlines
    let mut merged: Vec<Bar> = Vec::new();
    for b in bars {
        match merged.last_mut() {
            Some(l) if b.x - l.x < 1.2 * sp => {
                l.thick |= b.thick;
                l.x = l.x.max(b.x);
            }
            _ => merged.push(b),
        }
    }
    merged
}

/// Staves of one system share barlines: nothing to align when the counts agree; otherwise
/// the staff with the most measures wins and the others are re-split at its barlines is not
/// attempted (the review reports the mismatch).
fn align_system_barlines(_sys: &mut SystemResult) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draw a synthetic staff (five lines, spacing `sp`) into a mask.
    fn staff_mask(w: usize, h: usize, top: usize, sp: usize, x0: usize, x1: usize) -> Mask {
        let mut m = Mask::new(w, h);
        for k in 0..5 {
            let y = top + k * sp;
            for x in x0..x1 {
                m.set(x, y, true);
                m.set(x, y + 1, true);
            }
        }
        m
    }

    #[test]
    fn staves_are_found_and_grouped() {
        let mut m = staff_mask(800, 400, 40, 14, 20, 780);
        let m2 = staff_mask(800, 400, 220, 14, 20, 780);
        for (i, p) in m2.px.iter().enumerate() {
            if *p != 0 {
                m.px[i] = 1;
            }
        }
        let st = find_staves(&m);
        assert_eq!(st.len(), 2);
        assert!((st[0].sp - 14.0).abs() < 0.6, "{}", st[0].sp);
        assert_eq!(st[0].position(st[0].bottom()), 0);
        assert_eq!(st[0].position(st[0].top()), 8);
        assert_eq!(st[0].position(st[0].top() - 7.0), 9);
        assert!(st[1].top() > 200.0);
    }

    #[test]
    fn pitch_from_position_and_clef() {
        assert_eq!(step_of(&Clef::treble(), 0), ('E', 4));
        assert_eq!(step_of(&Clef::treble(), -2), ('C', 4));
        assert_eq!(step_of(&Clef::treble(), 8), ('F', 5));
        assert_eq!(step_of(&Clef::bass(), 0), ('G', 2));
        assert_eq!(step_of(&Clef::bass(), 10), ('C', 4));
        let alto = Clef { sign: "C".into(), line: 3, octave_change: 0 };
        assert_eq!(step_of(&alto, 4), ('C', 4));
        assert_eq!(key_alter_for(2, 'C'), 1);
        assert_eq!(key_alter_for(2, 'G'), 0);
        assert_eq!(key_alter_for(-3, 'A'), -1);
    }

    #[test]
    fn durations() {
        assert_eq!(dotted(DIV, 1), 72);
        assert_eq!(dotted(2 * DIV, 2), 168);
        assert_eq!(type_name(DIV / 4), "16th");
        assert_eq!(type_name(4 * DIV), "whole");
    }

    fn draw_rect(m: &mut Mask, x0: usize, y0: usize, x1: usize, y1: usize) {
        for y in y0..y1 {
            for x in x0..x1 {
                m.set(x, y, true);
            }
        }
    }

    #[test]
    fn accidental_shapes() {
        let sp = 14.0;
        // sharp: two vertical strokes and two bars
        let mut s = Mask::new(13, 38);
        draw_rect(&mut s, 3, 0, 5, 38);
        draw_rect(&mut s, 8, 0, 10, 38);
        draw_rect(&mut s, 0, 12, 13, 16);
        draw_rect(&mut s, 0, 22, 13, 26);
        assert_eq!(classify_accidental(&s, sp).map(|a| a.0), Some(Acc::Sharp));
        // flat: a stem on the left and a bowl at the bottom
        let mut f = Mask::new(11, 33);
        draw_rect(&mut f, 0, 0, 2, 33);
        draw_rect(&mut f, 0, 20, 11, 22);
        draw_rect(&mut f, 9, 20, 11, 29);
        draw_rect(&mut f, 0, 30, 6, 33);
        assert_eq!(classify_accidental(&f, sp).map(|a| a.0), Some(Acc::Flat));
        // natural: stroke top-left, stroke bottom-right
        let mut n = Mask::new(9, 38);
        draw_rect(&mut n, 0, 0, 2, 28);
        draw_rect(&mut n, 7, 10, 9, 38);
        draw_rect(&mut n, 0, 10, 9, 14);
        draw_rect(&mut n, 0, 22, 9, 26);
        assert_eq!(classify_accidental(&n, sp).map(|a| a.0), Some(Acc::Natural));
        // too small for anything but a double sharp, too big for anything
        assert!(classify_accidental(&Mask::new(40, 80), sp).is_none());
    }

    #[test]
    fn clef_from_box() {
        let st = StaffLines { ys: [100.0, 114.0, 128.0, 142.0, 156.0], x0: 0, x1: 500, sp: 14.0, thickness: 2.0 };
        let g = BBox::new(10, 75, 30, 180);
        assert_eq!(classify_clef(&st, &g).unwrap().0, Clef::treble());
        let f = BBox::new(10, 99, 35, 142);
        assert_eq!(classify_clef(&st, &f).unwrap().0, Clef::bass());
        let c = BBox::new(10, 99, 35, 158);
        let (cl, _) = classify_clef(&st, &c).unwrap();
        assert_eq!((cl.sign.as_str(), cl.line), ("C", 3));
        assert!(classify_clef(&st, &BBox::new(10, 120, 20, 130)).is_none());
    }

    #[test]
    fn digits_by_holes_and_outline() {
        // an 8: two holes
        let mut e = Mask::new(12, 20);
        draw_rect(&mut e, 0, 0, 12, 20);
        for (x0, y0, x1, y1) in [(3, 3, 9, 8), (3, 12, 9, 17)] {
            for y in y0..y1 {
                for x in x0..x1 {
                    e.set(x, y, false);
                }
            }
        }
        assert_eq!(classify_digit(&e), Some(8));
        // a 2: no hole, full-width foot
        let mut two = Mask::new(12, 20);
        draw_rect(&mut two, 0, 0, 12, 3);
        draw_rect(&mut two, 9, 0, 12, 10);
        draw_rect(&mut two, 0, 9, 12, 11);
        draw_rect(&mut two, 0, 9, 3, 20);
        draw_rect(&mut two, 0, 17, 12, 20);
        assert_eq!(classify_digit(&two), Some(2));
        // a 1 is narrow
        let mut one = Mask::new(4, 20);
        draw_rect(&mut one, 0, 0, 4, 20);
        assert_eq!(classify_digit(&one), Some(1));
    }
}
