//! Edit lists for `score read --fix edits.json`.
//!
//! ```json
//! [
//!   {"measure": 12, "voice": 1, "note": 3, "set": {"pitch": "E5"}},
//!   {"measure": 4, "note": 2, "set": {"duration": "eighth"}},
//!   {"measure": 7, "part": "P2", "staff": 2, "note": 1, "set": {"pitch": ["C3", "G3"]}},
//!   {"measure": 9, "note": 4, "set": {"rest": true}},
//!   {"measure": 9, "note": 4, "delete": true},
//!   {"measure": 3, "set": {"time": "3/4", "key": "G"}}
//! ]
//! ```
//!
//! `measure` is the displayed measure number (a pickup is 0). `note` is the 1-based index of
//! the event in the voice, counting rests and chords as one event each (as in
//! `score to-json`). `part` defaults to the first part, `staff` and `voice` to 1.
//! `set` keys: `pitch` (one name or a list for a chord; `"rest"` makes a rest), `duration`
//! (divisions, or a type such as `quarter`, `eighth.`, `16th`), `rest`, `tie` (start a tie
//! to the next note), `lyric`, `dynamic`, `articulations`, `solfa`, and for measures
//! `time`, `key`, `repeat_start`, `repeat_end`.

use crate::model::*;
use crate::pitch::{duration_of, parse_key, spell_midi};
use ferraus_core::{Error, Result};
use serde_json::{json, Value};

/// Apply edits; returns one result entry per edit (`ok` and a message).
pub fn apply(score: &mut Score, edits: &Value) -> Result<Vec<Value>> {
    let list = match edits {
        Value::Array(a) => a.clone(),
        Value::Object(o) => match o.get("edits") {
            Some(Value::Array(a)) => a.clone(),
            _ => vec![edits.clone()],
        },
        _ => return Err(Error::usage("edits must be a JSON array of edit objects")),
    };
    let mut results = Vec::new();
    for (i, e) in list.iter().enumerate() {
        match apply_one(score, e) {
            Ok(msg) => results.push(json!({"edit": i + 1, "ok": true, "message": msg})),
            Err(err) => results.push(json!({"edit": i + 1, "ok": false, "message": err.message})),
        }
    }
    Ok(results)
}

fn get_u32(v: &Value, k: &str) -> Option<u32> {
    v.get(k).and_then(|x| x.as_u64().map(|n| n as u32).or_else(|| x.as_str().and_then(|s| s.parse().ok())))
}

fn parse_duration(v: &Value, div: u32) -> Result<u32> {
    if let Some(n) = v.as_u64() {
        return Ok(n as u32);
    }
    let s = v.as_str().ok_or_else(|| Error::usage("duration must be divisions or a note type"))?;
    let dots = s.chars().rev().take_while(|c| *c == '.').count() as u8;
    let base = s.trim_end_matches('.');
    let base = match base {
        "1" | "w" => "whole",
        "2" | "h" => "half",
        "4" | "q" => "quarter",
        "8" | "e" => "eighth",
        "16" | "s" => "16th",
        "32" => "32nd",
        "64" => "64th",
        other => other,
    };
    duration_of(base, dots, div, None)
        .ok_or_else(|| Error::usage(format!("'{s}' is not a duration the score's divisions ({div}) can express")))
}

fn apply_one(score: &mut Score, e: &Value) -> Result<String> {
    let div = score.divisions;
    let mnum = get_u32(e, "measure").ok_or_else(|| Error::usage("each edit needs a 'measure'"))?;
    let part_id = e.get("part").and_then(|p| p.as_str()).map(|s| s.to_string());
    let staff_n = get_u32(e, "staff").unwrap_or(1);
    let voice_n = get_u32(e, "voice").unwrap_or(1);
    let note_i = get_u32(e, "note").or_else(|| get_u32(e, "event"));
    let empty = json!({});
    let set = e.get("set").unwrap_or(&empty);
    let pi = match &part_id {
        Some(id) => {
            score.parts.iter().position(|p| &p.id == id).ok_or_else(|| Error::usage(format!("no part '{id}'")))?
        }
        None => 0,
    };
    let part = score.parts.get_mut(pi).ok_or_else(|| Error::usage("the score has no parts"))?;
    let perc = part.percussion;
    let staves_count = part.staves.len();
    let st = part.staves.iter_mut().find(|s| s.number == staff_n).ok_or_else(|| {
        Error::usage(format!(
            "part {} has {} staff/staves, not {staff_n}",
            part_id.clone().unwrap_or_else(|| "P1".into()),
            staves_count
        ))
    })?;
    let mi =
        st.measures.iter().position(|m| m.number == mnum).ok_or_else(|| Error::usage(format!("no measure {mnum}")))?;
    let key = Score::key_at(st, mi);
    // measure-level edits
    if note_i.is_none() {
        let m = &mut st.measures[mi];
        let mut done = Vec::new();
        if let Some(t) = set.get("time").and_then(|t| t.as_str()) {
            let (a, b) = t.split_once('/').ok_or_else(|| Error::usage("time must look like 3/4"))?;
            m.time = Some(TimeSig::new(
                a.trim().parse().map_err(|_| Error::usage("bad time"))?,
                b.trim().parse().map_err(|_| Error::usage("bad time"))?,
            ));
            done.push(format!("time {t}"));
        }
        if let Some(k) = set.get("key").and_then(|t| t.as_str()) {
            m.key = Some(parse_key(k).ok_or_else(|| Error::usage(format!("'{k}' is not a key")))?);
            done.push(format!("key {k}"));
        }
        if let Some(b) = set.get("repeat_start").and_then(|b| b.as_bool()) {
            m.repeat_start = b;
            done.push("repeat start".into());
        }
        if let Some(r) = set.get("repeat_end") {
            m.repeat_end = r.as_u64().map(|n| n as u32).or(if r.as_bool() == Some(true) { Some(2) } else { None });
            done.push("repeat end".into());
        }
        if done.is_empty() {
            return Err(Error::usage(
                "a measure edit needs 'note' or a measure-level 'set' (time, key, repeat_start, repeat_end)",
            ));
        }
        return Ok(format!("measure {mnum}: {}", done.join(", ")));
    }
    let ni = note_i.unwrap() as usize;
    let m = &mut st.measures[mi];
    let v = m
        .voices
        .iter_mut()
        .find(|v| v.number == voice_n)
        .ok_or_else(|| Error::usage(format!("measure {mnum} has no voice {voice_n}")))?;
    if ni == 0 || ni > v.events.len() {
        return Err(Error::usage(format!(
            "measure {mnum} voice {voice_n} has {} event(s); note {ni} does not exist",
            v.events.len()
        )));
    }
    let idx = ni - 1;
    if e.get("delete").and_then(|d| d.as_bool()) == Some(true) {
        v.events.remove(idx);
        return Ok(format!("measure {mnum} voice {voice_n}: deleted event {ni}"));
    }
    if let Some(ins) = e.get("insert_after") {
        let d = parse_duration(ins.get("duration").unwrap_or(&json!("quarter")), div)?;
        let mut ev = Event::rest(d);
        if let Some(p) = ins.get("pitch") {
            ev = Event::notes(d, pitches(p, key, perc)?);
        }
        v.events.insert(idx + 1, ev);
        return Ok(format!("measure {mnum} voice {voice_n}: inserted after event {ni}"));
    }
    let ev = &mut v.events[idx];
    let mut done = Vec::new();
    if let Some(p) = set.get("pitch") {
        if p.as_str() == Some("rest") {
            ev.notes.clear();
            ev.rest = true;
            done.push("rest".to_string());
        } else {
            let mut ns = pitches(p, key, perc)?;
            // keep ties and velocities of the first note
            if let Some(old) = ev.notes.first() {
                for n in ns.iter_mut() {
                    n.tie_start = old.tie_start;
                    n.tie_stop = old.tie_stop;
                    n.velocity = old.velocity;
                }
            }
            ev.notes = ns;
            ev.rest = false;
            ev.confidence = None;
            ev.confidence_reason = None;
            done.push(format!("pitch {}", ev.notes.iter().map(|n| n.pitch.name()).collect::<Vec<_>>().join("+")));
        }
    }
    if let Some(d) = set.get("duration") {
        let dur = parse_duration(d, div)?;
        ev.duration = dur;
        ev.note_type = None;
        ev.dots = 0;
        done.push(format!("duration {dur}"));
    }
    if set.get("rest").and_then(|r| r.as_bool()) == Some(true) {
        ev.notes.clear();
        ev.rest = true;
        done.push("rest".into());
    }
    if let Some(t) = set.get("tie").and_then(|t| t.as_bool()) {
        for n in ev.notes.iter_mut() {
            n.tie_start = t;
        }
        done.push(format!("tie {t}"));
    }
    if let Some(l) = set.get("lyric").and_then(|l| l.as_str()) {
        ev.lyrics = vec![Lyric { text: l.to_string(), syllabic: "single".into(), verse: 1 }];
        done.push(format!("lyric {l}"));
    }
    if let Some(s) = set.get("solfa").and_then(|l| l.as_str()) {
        ev.solfa = Some(s.to_string());
        done.push(format!("solfa {s}"));
    }
    if let Some(d) = set.get("dynamic") {
        ev.dynamic = d.as_str().map(|s| s.to_string());
        done.push("dynamic".into());
    }
    if let Some(a) = set.get("articulations").and_then(|a| a.as_array()) {
        ev.articulations = a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
        done.push("articulations".into());
    }
    if done.is_empty() {
        return Err(Error::usage("the edit's 'set' has no known keys"));
    }
    Ok(format!("measure {mnum} voice {voice_n} note {ni}: {}", done.join(", ")))
}

fn pitches(p: &Value, key: KeySig, perc: bool) -> Result<Vec<Note>> {
    let names: Vec<String> = match p {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
        Value::Number(n) => vec![format!("midi:{n}")],
        _ => return Err(Error::usage("pitch must be a name like E5, a MIDI number, or a list")),
    };
    let mut out = Vec::new();
    for n in names {
        let note = if let Some(m) = n.strip_prefix("midi:").and_then(|x| x.parse::<i32>().ok()) {
            Note::new(spell_midi(m, key))
        } else if perc {
            let k = crate::drums::key_for_name(&n)
                .ok_or_else(|| Error::usage(format!("'{n}' is not a drum name (kick, snare, hh, ride, crash...)")))?;
            let sp = crate::drums::spec(k);
            let mut nt = Note::new(crate::drums::display_pitch(k));
            nt.midi = k as i32;
            nt.unpitched = true;
            nt.notehead = sp.notehead.map(|h| h.to_string());
            nt.instrument = Some(sp.name.to_string());
            nt
        } else {
            Note::new(
                Pitch::parse(&n)
                    .ok_or_else(|| Error::usage(format!("'{n}' is not a pitch (use names like E5, F#4, Bb3)")))?,
            )
        };
        out.push(note);
    }
    Ok(out)
}
