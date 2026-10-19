//! Test fixtures built in code (no binary files).
#![allow(dead_code, clippy::field_reassign_with_default, clippy::manual_is_multiple_of, clippy::needless_range_loop)]

use ferraus_core::testing::TempDir;
use ferraus_core::{Globals, OpCtx};
use ferraus_score::model::*;
use ferraus_score::pitch::duration_of;
use std::path::PathBuf;

pub const DIV: u32 = 48;

/// Parse one event: `C4:q`, `r:e`, `C4+E4+G4:h`, `D5:e3` (triplet eighth), `C4:q.`,
/// `C4:h~` (tie to the next note).
pub fn ev(spec: &str) -> Event {
    let (p, d) = spec.split_once(':').expect("pitch:duration");
    let tie = d.ends_with('~');
    let d = d.trim_end_matches('~');
    let triplet = d.ends_with('3');
    let d = d.trim_end_matches('3');
    let dots = d.chars().filter(|c| *c == '.').count() as u8;
    let base = match d.trim_end_matches('.') {
        "w" => "whole",
        "h" => "half",
        "q" => "quarter",
        "e" => "eighth",
        "s" => "16th",
        "t" => "32nd",
        x => panic!("bad duration {x}"),
    };
    let tup = if triplet { Some((3, 2)) } else { None };
    let dur = duration_of(base, dots, DIV, tup).unwrap();
    let mut e = if p == "r" {
        Event::rest(dur)
    } else {
        Event::notes(
            dur,
            p.split('+')
                .map(|n| {
                    let mut note = Note::new(Pitch::parse(n).unwrap_or_else(|| panic!("bad pitch {n}")));
                    note.tie_start = tie;
                    note
                })
                .collect(),
        )
    };
    if triplet {
        e.tuplet = Some(Tuplet { actual: 3, normal: 2, start: false, stop: false });
    }
    e
}

/// Build measures for one voice; ties and tuplet brackets are completed automatically.
pub fn measures(bars: &[&[&str]]) -> Vec<Vec<Event>> {
    let mut out: Vec<Vec<Event>> = bars.iter().map(|b| b.iter().map(|s| ev(s)).collect()).collect();
    // tie stops
    let mut open: Vec<i32> = Vec::new();
    for bar in out.iter_mut() {
        for e in bar.iter_mut() {
            for n in e.notes.iter_mut() {
                if open.contains(&n.midi) {
                    n.tie_stop = true;
                }
            }
            open = e.notes.iter().filter(|n| n.tie_start).map(|n| n.midi).collect();
        }
        // tuplet brackets: groups of consecutive triplet events adding up to a quarter
        let mut acc = 0u32;
        let n = bar.len();
        for i in 0..n {
            if bar[i].tuplet.is_some() {
                let start = acc == 0;
                acc += bar[i].duration;
                let stop = acc % DIV == 0;
                if stop {
                    acc = 0;
                }
                let t = bar[i].tuplet.as_mut().unwrap();
                t.start = start;
                t.stop = stop;
            }
        }
    }
    out
}

/// A one-part, one-staff score.
pub fn score(key: KeySig, time: TimeSig, pickup: bool, bars: &[&[&str]]) -> Score {
    score_with(key, time, pickup, vec![(Clef::treble(), measures(bars))])
}

/// One part with several staves (each a list of measures of voice 1).
pub fn score_with(key: KeySig, time: TimeSig, pickup: bool, staves: Vec<(Clef, Vec<Vec<Event>>)>) -> Score {
    let mut st_out = Vec::new();
    for (si, (clef, ms)) in staves.into_iter().enumerate() {
        let mut v = Vec::new();
        for (mi, evs) in ms.into_iter().enumerate() {
            let number = if pickup { mi as u32 } else { mi as u32 + 1 };
            let mut m = Measure {
                number,
                implicit: pickup && mi == 0,
                voices: vec![Voice { number: 1, events: evs }],
                ..Measure::default()
            };
            if mi == 0 {
                m.key = Some(key);
                m.time = Some(time);
                m.clef = Some(clef.clone());
                if si == 0 {
                    m.tempo = vec![TempoMark { offset: 0, bpm: 100.0, text: None }];
                }
            }
            v.push(m);
        }
        st_out.push(Staff { number: si as u32 + 1, measures: v });
    }
    Score {
        title: Some("Test".into()),
        divisions: DIV,
        parts: vec![Part {
            id: "P1".into(),
            name: "Piano".into(),
            abbreviation: None,
            program: Some(0),
            channel: Some(1),
            percussion: false,
            staves: st_out,
        }],
        ..Score::default()
    }
}

/// Notes as (absolute onset in quarters × 48, duration, midi), ties merged, per staff.
pub fn flat_notes(s: &Score, part: usize, staff: usize) -> Vec<(u64, u64, i32)> {
    let div = s.divisions as u64;
    let st = &s.parts[part].staves[staff];
    let mut out: Vec<(u64, u64, i32)> = Vec::new();
    let mut pos = 0u64;
    let pickup_shift = 0u64;
    for m in &st.measures {
        let mut len = 0u64;
        for v in &m.voices {
            let mut p = pos;
            for e in &v.events {
                for n in &e.notes {
                    let on = (p + pickup_shift) * 48 / div;
                    let d = e.duration as u64 * 48 / div;
                    if n.tie_stop {
                        if let Some(prev) = out.iter_mut().rev().find(|x| x.2 == n.midi && x.0 + x.1 == on) {
                            prev.1 += d;
                            continue;
                        }
                    }
                    out.push((on, d, n.midi));
                }
                p += e.duration as u64;
            }
            len = len.max(p - pos);
        }
        pos += len;
    }
    out.sort();
    out
}

/// Run an op from this crate's registry with inputs, params, and -o.
pub fn run_op(name: &str, inputs: Vec<PathBuf>, pairs: &[(&str, &str)], output: Option<PathBuf>) -> ferraus_core::Result<(serde_json::Value, Vec<String>)> {
    let reg = ferraus_core::testing::leak_registry(ferraus_score::ops());
    let spec = reg.get(name).expect("op");
    let args = ferraus_core::Args::build(spec, pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), &|_, _| None)?;
    let mut g = Globals::default();
    g.output = output.map(|p| p.display().to_string());
    g.overwrite = true;
    let mut ctx = OpCtx::new(name, inputs, g, &ferraus_core::testing::SIMPLE_IO, &ferraus_core::testing::NO_PROGRESS);
    ctx.registry = Some(reg);
    let v = match spec.runner {
        ferraus_core::Runner::Custom(f) => f(&args, &mut ctx)?,
        _ => panic!("custom op expected"),
    };
    Ok((v, ctx.outputs.iter().map(|o| o.path.clone()).collect()))
}

pub fn tmp(name: &str) -> TempDir {
    TempDir::new(name)
}
