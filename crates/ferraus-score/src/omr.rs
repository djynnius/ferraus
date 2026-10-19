//! Optical music recognition engines for `score read`.
//!
//! * `builtin`: oemer's two segmentation networks (MIT, ONNX, from the model registry:
//!   `omr/oemer` and `omr/oemer-seg`) label staff lines, noteheads, stems/rests, and
//!   clefs/keys; [`recon`] rebuilds staves, measures, and timed events from those maps and
//!   the page's ink. Pages come from images ([`raster::decode_image`]) or PDFs
//!   ([`raster::rasterize_pdf`]: pdfium when built with the `pdfium` feature, else
//!   `pdftoppm`, `mutool`, or macOS `sips`). The networks run through a backend the CLI
//!   registers ([`segment::register_seg_backend`]).
//! * `audiveris`: the optional Audiveris bridge (`ferraus doctor --install-audiveris`),
//!   which reads PDFs and images itself and exports `.mxl`.

pub mod bitmap;
pub mod raster;
pub mod recon;
pub mod segment;

use crate::model::{Clef, Event, KeySig, Measure, Note, Part, Score, Staff, TimeSig, Voice};
use ferraus_core::{Error, OpCtx, Result};
use recon::{MeasureResult, PageResult, DIV};
use std::path::Path;

/// Parse `1-4`, `2`, `1-3,6` into inclusive 1-based ranges.
pub fn parse_pages(s: &str) -> Result<Vec<(u32, u32)>> {
    let mut out = Vec::new();
    for part in s.split(',').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let a: u32 =
            a.parse().map_err(|_| Error::usage(format!("--pages: '{part}' is not a page or range like 1-4")))?;
        let b: u32 = if b.is_empty() || b == "end" {
            u32::MAX
        } else {
            b.parse().map_err(|_| Error::usage(format!("--pages: '{part}' is not a page or range like 1-4")))?
        };
        if a == 0 || b < a {
            return Err(Error::usage(format!("--pages: '{part}' is not a valid range (pages start at 1)")));
        }
        out.push((a, b));
    }
    Ok(out)
}

pub fn page_selected(ranges: &[(u32, u32)], page: u32) -> bool {
    ranges.is_empty() || ranges.iter().any(|(a, b)| page >= *a && page <= *b)
}

/// What a recognition run produced.
pub struct Recognized {
    pub score: Score,
    pub unsupported: Vec<String>,
    pub engine: String,
    pub movements: usize,
    /// Extra facts for the result (pages read, rasterizer, staff space, issues).
    pub info: serde_json::Value,
}

/// Constructs the builtin engine does not read; listed in every builtin result.
pub const BUILTIN_UNSUPPORTED: &[&str] = &[
    "lyrics and text are not read",
    "tuplets, grace notes, ties, slurs, dynamics, articulations, repeats, and endings are not read",
    "one voice per staff: a staff with two voices (stems up and down at once) is read as chords",
];

/// Recognize sheet music from a PDF or image.
pub fn recognize(ctx: &mut OpCtx, input: &Path, engine: &str, pages: Option<&str>, dpi: u32) -> Result<Recognized> {
    let ranges = match pages {
        Some(p) => parse_pages(p)?,
        None => Vec::new(),
    };
    match engine {
        "audiveris" => {
            let docs = crate::audiveris::transcribe(input, pages)?;
            let movements = docs.len();
            let mut iter = docs.into_iter();
            let (mut score, mut unsupported) = crate::musicxml::read_bytes(&iter.next().unwrap())?;
            for d in iter {
                // append further movements' measures to matching parts
                let (s2, u2) = crate::musicxml::read_bytes(&d)?;
                unsupported.extend(u2);
                append_movement(&mut score, s2);
            }
            Ok(Recognized { score, unsupported, engine: "audiveris".into(), movements, info: serde_json::json!({}) })
        }
        _ => recognize_builtin(ctx, input, &ranges, dpi),
    }
}

fn recognize_builtin(ctx: &mut OpCtx, input: &Path, ranges: &[(u32, u32)], dpi: u32) -> Result<Recognized> {
    let backend = segment::backend()?;
    (backend.prepare)(ctx, &[segment::STAFF_MODEL, segment::SYMBOL_MODEL])?;
    let bytes = std::fs::read(input)?;
    let (pages, rasterizer) = if bytes.starts_with(b"%PDF") {
        raster::rasterize_pdf(input, &bytes, ranges, dpi)?
    } else {
        if !page_selected(ranges, 1) {
            return Err(Error::usage("an image has a single page; --pages must include page 1"));
        }
        (vec![raster::Page { number: 1, image: raster::decode_image(&bytes)? }], "image".to_string())
    };
    if pages.is_empty() {
        return Err(Error::input("no pages selected"));
    }
    let debug_dir = std::env::var_os("FERRAUS_OMR_DEBUG").map(std::path::PathBuf::from);
    let mut results: Vec<PageResult> = Vec::new();
    let n_pages = pages.len();
    for (pi, page) in pages.iter().enumerate() {
        let (w, h) = segment::model_size(page.image.w, page.image.h);
        let g = page.image.resize(w, h);
        let base = pi as f64 / n_pages as f64;
        let span = 1.0 / n_pages as f64;
        let staff = segment::segment(ctx, &backend, segment::STAFF_MODEL, &g, segment::STAFF_WINDOW, 3, &mut |c, f| {
            c.set_progress(base + span * 0.35 * f)
        })?;
        let symbols =
            segment::segment(ctx, &backend, segment::SYMBOL_MODEL, &g, segment::SYMBOL_WINDOW, 4, &mut |c, f| {
                c.set_progress(base + span * (0.35 + 0.6 * f))
            })?;
        let mut dbg = recon::Debug::default();
        let r = recon::read_page(&recon::PageInput { gray: &g, staff: &staff, symbols: &symbols }, &mut dbg);
        if let Some(d) = &debug_dir {
            let _ = write_debug(d, page.number, &g, &staff, &symbols, &dbg);
        }
        results.push(r);
    }
    ctx.set_progress(1.0);
    let (score, issues) = assemble(&results);
    if score.parts.is_empty() {
        return Err(Error::processing("no staves were recognized on the selected pages").with_hint(
            "check that the input is printed sheet music at a readable resolution (staff lines at least ~10 px apart), or try --engine audiveris",
        ));
    }
    let staff_space: Vec<f64> = results.iter().map(|r| (r.staff_space * 10.0).round() / 10.0).collect();
    let info = serde_json::json!({
        "pages": pages.iter().map(|p| p.number).collect::<Vec<_>>(),
        "rasterizer": rasterizer,
        "staff_space_px": staff_space,
        "issues": issues,
    });
    let unsupported = BUILTIN_UNSUPPORTED.iter().map(|s| s.to_string()).collect();
    Ok(Recognized { score, unsupported, engine: "builtin".into(), movements: 1, info })
}

fn majority<T: PartialEq + Clone>(items: impl Iterator<Item = T>) -> Option<T> {
    let mut counts: Vec<(T, usize)> = Vec::new();
    for it in items {
        match counts.iter_mut().find(|(v, _)| *v == it) {
            Some(c) => c.1 += 1,
            None => counts.push((it, 1)),
        }
    }
    // the first value wins ties (the first system is the most reliable header)
    let best = counts.iter().map(|c| c.1).max()?;
    counts.into_iter().find(|c| c.1 == best).map(|c| c.0)
}

/// Measure length in quarters × DIV for the events of a measure.
fn fill(m: &MeasureResult) -> u32 {
    m.events.iter().map(|e| e.duration).sum()
}

/// Time signature from how full the measures are, when none was printed or read.
fn infer_time(lengths: &[u32]) -> Option<TimeSig> {
    let len = majority(lengths.iter().copied().filter(|&l| l > 0))?;
    Some(match len {
        l if l == 4 * DIV => TimeSig::new(4, 4),
        l if l == 3 * DIV => TimeSig::new(3, 4),
        l if l == 2 * DIV => TimeSig::new(2, 4),
        l if l == 5 * DIV => TimeSig::new(5, 4),
        l if l == 6 * DIV => TimeSig::new(6, 4),
        l if l == 3 * DIV / 2 => TimeSig::new(3, 8),
        l if l * 2 % DIV == 0 => TimeSig::new(l * 2 / DIV, 8),
        _ => return None,
    })
}

/// Try one-symbol corrections that make a measure add up: add or drop a dot, or halve or
/// double one event. Applied only when exactly one correction fits.
fn repair_measure(m: &mut MeasureResult, expected: u32) -> bool {
    let have = fill(m);
    if have == expected || m.events.is_empty() {
        return have == expected;
    }
    // a lone whole rest is a whole-measure rest in any meter
    if m.events.len() == 1 && m.events[0].rest && m.events[0].measure_rest {
        m.events[0].duration = expected;
        return true;
    }
    let mut fixes: Vec<(usize, u32, u8, &'static str)> = Vec::new();
    for (i, e) in m.events.iter().enumerate() {
        let base = match e.dots {
            0 => e.duration,
            1 => e.duration * 2 / 3,
            _ => e.duration * 4 / 7,
        };
        let others = have - e.duration;
        let mut cands: Vec<(u32, u8, &'static str)> = Vec::new();
        if e.dots == 0 {
            cands.push((base * 3 / 2, 1, "dot added"));
        } else {
            cands.push((base, 0, "dot removed"));
        }
        cands.push((e.duration * 2, e.dots, "duration doubled"));
        if e.duration % 2 == 0 {
            cands.push((e.duration / 2, e.dots, "duration halved"));
        }
        for (d, dots, why) in cands {
            if others + d == expected {
                fixes.push((i, d, dots, why));
            }
        }
    }
    // prefer fixing the least confident event
    fixes.sort_by(|a, b| m.events[a.0].confidence.partial_cmp(&m.events[b.0].confidence).unwrap());
    let lowest = fixes.first().map(|f| m.events[f.0].confidence);
    let tied = fixes.iter().filter(|f| Some(m.events[f.0].confidence) == lowest).count();
    if let Some(&(i, d, dots, why)) = fixes.first() {
        if tied == 1 || fixes.len() == 1 {
            let e = &mut m.events[i];
            e.duration = d;
            e.dots = dots;
            let base = match dots {
                0 => d,
                1 => d * 2 / 3,
                _ => d * 4 / 7,
            };
            e.note_type = type_from(base).into();
            e.confidence *= 0.7;
            m.confidence = m.confidence.min(e.confidence);
            let r = format!("{why} to fill the measure");
            e.reason = Some(r.clone());
            m.reasons.push(r);
            return true;
        }
    }
    false
}

fn type_from(base: u32) -> &'static str {
    match base {
        x if x >= 4 * DIV => "whole",
        x if x >= 2 * DIV => "half",
        x if x >= DIV => "quarter",
        x if x >= DIV / 2 => "eighth",
        x if x >= DIV / 4 => "16th",
        _ => "32nd",
    }
}

/// Build the score from page results: parts from the staves per system, clef/key/time from
/// the system headers (majority), measures appended system after system.
pub fn assemble(pages: &[PageResult]) -> (Score, Vec<String>) {
    let mut issues: Vec<String> = Vec::new();
    for (i, p) in pages.iter().enumerate() {
        for s in &p.issues {
            issues.push(format!("page {}: {s}", i + 1));
        }
    }
    let systems: Vec<&recon::SystemResult> = pages.iter().flat_map(|p| p.systems.iter()).collect();
    let mut score = Score { divisions: DIV, ..Score::default() };
    let Some(n) = majority(systems.iter().map(|s| s.staves.len())) else { return (score, issues) };
    if n == 0 {
        return (score, issues);
    }
    let odd = systems.iter().filter(|s| s.staves.len() != n).count();
    if odd > 0 {
        issues.push(format!("{odd} system(s) do not have {n} staves; their extra staves were dropped or left empty"));
    }
    // per staff index: clef, key, time
    let clefs: Vec<Clef> = (0..n)
        .map(|k| {
            majority(systems.iter().filter_map(|s| s.staves.get(k).and_then(|st| st.clef.clone())))
                .unwrap_or_else(|| if k == 1 && n == 2 { Clef::bass() } else { Clef::treble() })
        })
        .collect();
    let keys: Vec<i8> = (0..n)
        .map(|k| majority(systems.iter().filter_map(|s| s.staves.get(k).and_then(|st| st.key))).unwrap_or(0))
        .collect();
    let key = majority(keys.iter().copied()).unwrap_or(0);
    let printed_time = systems.first().and_then(|s| s.staves.iter().find_map(|st| st.time));
    // the measures of each staff index, in order
    let mut staff_measures: Vec<Vec<MeasureResult>> = vec![Vec::new(); n];
    for s in &systems {
        let count = s.staves.iter().take(n).map(|st| st.measures.len()).max().unwrap_or(0);
        for (k, sm) in staff_measures.iter_mut().enumerate() {
            let ms = s.staves.get(k).map(|st| st.measures.clone()).unwrap_or_default();
            let have = ms.len();
            sm.extend(ms);
            for _ in have..count {
                sm.push(MeasureResult {
                    confidence: 0.0,
                    reasons: vec!["no measure was found on this staff here".into()],
                    ..Default::default()
                });
            }
        }
    }
    let lengths: Vec<u32> = staff_measures.iter().flat_map(|ms| ms.iter().map(fill)).collect();
    let time = match printed_time {
        Some(t) => {
            // a misread digit: trust the measures when they clearly disagree
            let inferred = infer_time(&lengths);
            let agree = lengths.iter().filter(|&&l| l == t.measure_divisions(DIV)).count();
            match inferred {
                Some(i) if agree * 3 < lengths.len() && i.measure_divisions(DIV) != t.measure_divisions(DIV) => {
                    issues.push(format!(
                        "the time signature read as {}/{} does not match the measures; {}/{} used",
                        t.beats, t.beat_type, i.beats, i.beat_type
                    ));
                    i
                }
                _ => t,
            }
        }
        None => {
            let t = infer_time(&lengths).unwrap_or(TimeSig::new(4, 4));
            issues.push(format!("no time signature was read; {}/{} inferred from the measures", t.beats, t.beat_type));
            t
        }
    };
    let expected = time.measure_divisions(DIV);
    let total = staff_measures.iter().map(|m| m.len()).max().unwrap_or(0);
    for ms in staff_measures.iter_mut() {
        for (i, m) in ms.iter_mut().enumerate() {
            let ok = repair_measure(m, expected);
            let pickup = i == 0 && fill(m) < expected && fill(m) > 0;
            let last = i + 1 == total && fill(m) < expected && fill(m) > 0;
            if !ok && !pickup && !last {
                m.confidence = m.confidence.min(0.4);
                m.reasons.push(format!(
                    "durations add up to {}/{} quarters, the measure needs {}/{}",
                    fill(m),
                    DIV,
                    expected,
                    DIV
                ));
            }
        }
    }
    // a pickup only when every staff agrees the first measure is short
    let pickup = staff_measures.iter().all(|ms| ms.first().map(|m| fill(m) < expected && fill(m) > 0).unwrap_or(false));
    let grand = n == 2 && clefs[0].sign == "G" && clefs[1].sign == "F";
    let to_staff = |k: usize, number: u32| -> Staff {
        let ms = &staff_measures[k];
        let mut out = Vec::new();
        for (i, m) in ms.iter().enumerate() {
            let num = if pickup { i as u32 } else { i as u32 + 1 };
            let mut events: Vec<Event> = m
                .events
                .iter()
                .map(|e| {
                    let mut ev = if e.rest {
                        Event::rest(e.duration)
                    } else {
                        Event::notes(
                            e.duration,
                            e.pitches
                                .iter()
                                .map(|p| {
                                    let mut n = Note::new(*p);
                                    n.confidence = Some(e.confidence);
                                    n
                                })
                                .collect(),
                        )
                    };
                    ev.note_type = Some(e.note_type.clone());
                    ev.dots = e.dots;
                    ev.measure_rest = e.rest && e.measure_rest && m.events.len() == 1;
                    ev.confidence = Some((e.confidence * 100.0).round() / 100.0);
                    ev.confidence_reason = e.reason.clone();
                    ev
                })
                .collect();
            if events.is_empty() && m.confidence > 0.0 {
                // an empty measure on the page is a full-measure rest
                let mut r = Event::rest(expected);
                r.measure_rest = true;
                events.push(r);
            }
            out.push(Measure {
                number: num,
                implicit: pickup && i == 0,
                key: (i == 0).then_some(KeySig::major(if grand { key } else { keys[k] })),
                time: (i == 0).then_some(time),
                clef: (i == 0).then(|| clefs[k].clone()),
                voices: if events.is_empty() { Vec::new() } else { vec![Voice { number: 1, events }] },
                barline: (m.final_bar && i + 1 == ms.len()).then(|| "light-heavy".to_string()),
                confidence: Some((m.confidence.clamp(0.0, 1.0) * 100.0).round() / 100.0),
                confidence_reasons: m.reasons.clone(),
                ..Default::default()
            });
        }
        Staff { number, measures: out }
    };
    if grand {
        score.parts.push(Part {
            id: "P1".into(),
            name: "Piano".into(),
            abbreviation: None,
            program: Some(0),
            channel: None,
            percussion: false,
            staves: vec![to_staff(0, 1), to_staff(1, 2)],
        });
    } else {
        for k in 0..n {
            score.parts.push(Part {
                id: format!("P{}", k + 1),
                name: if n == 1 { "Music".into() } else { format!("Staff {}", k + 1) },
                abbreviation: None,
                program: None,
                channel: None,
                percussion: false,
                staves: vec![to_staff(k, 1)],
            });
        }
    }
    (score, issues)
}

/// `FERRAUS_OMR_DEBUG=<dir>`: write the network maps and the reader's boxes as PNGs.
fn write_debug(
    dir: &Path,
    page: u32,
    g: &bitmap::Gray,
    staff: &segment::ClassMap,
    symbols: &segment::ClassMap,
    dbg: &recon::Debug,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut maps = image::RgbImage::new(g.w as u32, g.h as u32);
    let mut boxes = image::RgbImage::new(g.w as u32, g.h as u32);
    for y in 0..g.h {
        for x in 0..g.w {
            let v = g.at(x, y);
            let light = 128 + v / 2;
            let mut c = [light, light, light];
            match staff.class[y * g.w + x] {
                1 => c = [120, 120, 255],
                2 => c = [200, 200, 200],
                _ => {}
            }
            match symbols.class[y * g.w + x] {
                1 => c = [0, 170, 0],
                2 => c = [220, 0, 0],
                3 => c = [230, 140, 0],
                _ => {}
            }
            maps.put_pixel(x as u32, y as u32, image::Rgb(c));
            boxes.put_pixel(x as u32, y as u32, image::Rgb([v, v, v]));
        }
    }
    for (b, col) in &dbg.boxes {
        let (x0, y0, x1, y1) = b.clamp(g.w, g.h);
        for x in x0..x1 {
            for y in [y0, y1.saturating_sub(1)] {
                if y < g.h {
                    boxes.put_pixel(x as u32, y as u32, image::Rgb(*col));
                }
            }
        }
        for y in y0..y1 {
            for x in [x0, x1.saturating_sub(1)] {
                if x < g.w {
                    boxes.put_pixel(x as u32, y as u32, image::Rgb(*col));
                }
            }
        }
    }
    let err = |e: image::ImageError| Error::processing(format!("debug image: {e}"));
    maps.save(dir.join(format!("page{page}-maps.png"))).map_err(err)?;
    boxes.save(dir.join(format!("page{page}-boxes.png"))).map_err(err)?;
    Ok(())
}

fn append_movement(score: &mut Score, other: Score) {
    if other.divisions != score.divisions {
        let l = crate::pitch::lcm(score.divisions as u64, other.divisions as u64) as u32;
        score.rescale(l);
        let mut o = other;
        o.rescale(l);
        return append_movement(score, o);
    }
    for (pi, p) in other.parts.into_iter().enumerate() {
        if let Some(dst) = score.parts.get_mut(pi) {
            for (si, st) in p.staves.into_iter().enumerate() {
                if let Some(dst_st) = dst.staves.get_mut(si) {
                    let base = dst_st.measures.last().map(|m| m.number).unwrap_or(0);
                    for mut m in st.measures {
                        m.number += base;
                        dst_st.measures.push(m);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recon::Ev;

    #[test]
    fn pages() {
        assert_eq!(parse_pages("1-4").unwrap(), vec![(1, 4)]);
        assert_eq!(parse_pages("2, 5-6").unwrap(), vec![(2, 2), (5, 6)]);
        assert!(parse_pages("0-2").is_err());
        assert!(parse_pages("x").is_err());
        let r = parse_pages("1-2,4").unwrap();
        assert!(page_selected(&r, 4) && !page_selected(&r, 3));
    }

    fn ev(d: u32, dots: u8, conf: f32) -> Ev {
        Ev {
            x: 0.0,
            rest: false,
            pitches: vec![crate::model::Pitch { step: 'C', alter: 0, octave: 4 }],
            duration: d,
            dots,
            note_type: type_from(d).into(),
            measure_rest: false,
            confidence: conf,
            reason: None,
        }
    }

    #[test]
    fn a_missed_dot_is_repaired() {
        // dotted half read as a half in 4/4 next to a quarter
        let mut m = MeasureResult {
            events: vec![ev(2 * DIV, 0, 0.5), ev(DIV, 0, 0.9)],
            confidence: 1.0,
            ..Default::default()
        };
        assert!(repair_measure(&mut m, 4 * DIV));
        assert_eq!(m.events[0].duration, 3 * DIV);
        assert_eq!(m.events[0].dots, 1);
        assert!(m.confidence < 0.5);
        // nothing fits: left alone
        let mut m = MeasureResult { events: vec![ev(DIV / 4, 0, 0.9)], confidence: 1.0, ..Default::default() };
        assert!(!repair_measure(&mut m, 4 * DIV));
        assert_eq!(m.events[0].duration, DIV / 4);
    }

    #[test]
    fn time_from_measure_fill() {
        assert_eq!(infer_time(&[3 * DIV, 3 * DIV, 2 * DIV]), Some(TimeSig::new(3, 4)));
        assert_eq!(infer_time(&[4 * DIV]), Some(TimeSig::new(4, 4)));
        assert_eq!(infer_time(&[]), None);
    }

    #[test]
    fn assemble_builds_a_grand_staff() {
        use recon::{StaffResult, SystemResult};
        let m = |d: u32| MeasureResult { events: vec![ev(d, 0, 0.9)], confidence: 0.9, ..Default::default() };
        let staff = |clef: Clef| StaffResult {
            clef: Some(clef),
            key: Some(1),
            time: Some(TimeSig::new(4, 4)),
            measures: vec![m(4 * DIV), m(4 * DIV)],
            lines: None,
        };
        let page = PageResult {
            systems: vec![SystemResult { staves: vec![staff(Clef::treble()), staff(Clef::bass())] }],
            issues: vec![],
            staff_space: 14.0,
        };
        let (score, issues) = assemble(&[page]);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(score.parts.len(), 1);
        assert_eq!(score.parts[0].staves.len(), 2);
        let m1 = &score.parts[0].staves[1].measures[0];
        assert_eq!(m1.clef, Some(Clef::bass()));
        assert_eq!(m1.key, Some(KeySig::major(1)));
        let report = crate::check::check(&score, &[]);
        assert!(report.ok, "{:?}", report.issues);
    }
}
