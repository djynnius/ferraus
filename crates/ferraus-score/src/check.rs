//! `score check` and `score read --review`: measure durations, voices, ties, pitches,
//! unsupported constructs, and low-confidence measures.

use crate::model::*;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Issue {
    /// `error` (fails the check), `warning`, or `info`.
    pub severity: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staff: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measure: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<u32>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub ok: bool,
    pub errors: usize,
    pub warnings: usize,
    pub parts: usize,
    pub measures: usize,
    pub notes: usize,
    pub issues: Vec<Issue>,
    /// Measures whose durations do not add up (displayed numbers, per part).
    pub failing_measures: Vec<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub low_confidence: Vec<LowConfidence>,
}

fn issue(sev: &str, code: &str, part: &Part, staff: Option<u32>, measure: Option<u32>, voice: Option<u32>, msg: String) -> Issue {
    Issue {
        severity: sev.into(),
        code: code.into(),
        part: Some(part.id.clone()),
        staff,
        measure,
        voice,
        message: msg,
    }
}

fn q(d: u64, div: u32) -> String {
    // duration in quarter notes, as a fraction
    let g = crate::pitch::gcd(d, div as u64).max(1);
    let (n, dd) = (d / g, div as u64 / g);
    if dd == 1 {
        format!("{n} quarter(s)")
    } else {
        format!("{n}/{dd} quarter(s)")
    }
}

/// Validate a score. `unsupported` lists constructs a reader skipped.
pub fn check(score: &Score, unsupported: &[String]) -> Report {
    let mut issues: Vec<Issue> = Vec::new();
    let div = score.divisions;
    let mut notes = 0usize;
    let mut failing: Vec<u32> = Vec::new();
    if div == 0 {
        issues.push(Issue {
            severity: "error".into(),
            code: "bad_divisions".into(),
            part: None,
            staff: None,
            measure: None,
            voice: None,
            message: "divisions must be at least 1".into(),
        });
    }
    if score.parts.is_empty() {
        issues.push(Issue {
            severity: "error".into(),
            code: "no_parts".into(),
            part: None,
            staff: None,
            measure: None,
            voice: None,
            message: "the score has no parts".into(),
        });
    }
    let has_pickup = score
        .parts
        .first()
        .and_then(|p| p.staves.first())
        .and_then(|s| s.measures.first())
        .map(|m| m.implicit)
        .unwrap_or(false);
    for p in &score.parts {
        let counts: Vec<usize> = p.staves.iter().map(|s| s.measures.len()).collect();
        if counts.windows(2).any(|w| w[0] != w[1]) {
            issues.push(issue("error", "staff_measure_count", p, None, None, None, format!("staves have different measure counts: {counts:?}")));
        }
        if p.staves.is_empty() {
            issues.push(issue("error", "no_staves", p, None, None, None, "the part has no staves".into()));
        }
        for st in &p.staves {
            let n = st.measures.len();
            let pickup_len: u64 = st.measures.first().filter(|m| m.implicit).map(longest).unwrap_or(0);
            for (mi, m) in st.measures.iter().enumerate() {
                let t = Score::time_at(st, mi);
                let expected = t.measure_divisions(div) as u64;
                if m.voices.is_empty() {
                    issues.push(issue("error", "no_voices", p, Some(st.number), Some(m.number), None, "measure has no voices".into()));
                    failing.push(m.number);
                    continue;
                }
                if m.voices.len() > 4 {
                    issues.push(issue("warning", "too_many_voices", p, Some(st.number), Some(m.number), None, format!("{} voices on one staff (at most 4 are supported by most engravers)", m.voices.len())));
                }
                let mut seen = Vec::new();
                for v in &m.voices {
                    if seen.contains(&v.number) {
                        issues.push(issue("error", "duplicate_voice", p, Some(st.number), Some(m.number), Some(v.number), "voice number used twice".into()));
                    }
                    seen.push(v.number);
                    let total: u64 = v.events.iter().filter(|e| !e.grace).map(|e| e.duration as u64).sum();
                    let is_last = mi + 1 == n;
                    let first_voice = v.number == m.voices.iter().map(|x| x.number).min().unwrap_or(1);
                    if total > expected {
                        issues.push(issue("error", "measure_too_long", p, Some(st.number), Some(m.number), Some(v.number), format!("voice {} lasts {}, the {}/{} measure holds {}", v.number, q(total, div), t.beats, t.beat_type, q(expected, div))));
                        failing.push(m.number);
                    } else if total < expected && !m.implicit {
                        let completes_pickup = is_last && has_pickup && total + pickup_len == expected;
                        if first_voice && !completes_pickup {
                            issues.push(issue("error", "measure_too_short", p, Some(st.number), Some(m.number), Some(v.number), format!("voice {} lasts {}, the {}/{} measure needs {}", v.number, q(total, div), t.beats, t.beat_type, q(expected, div))));
                            failing.push(m.number);
                        } else if !first_voice {
                            issues.push(issue("info", "voice_incomplete", p, Some(st.number), Some(m.number), Some(v.number), format!("voice {} covers {} of {}", v.number, q(total, div), q(expected, div))));
                        }
                    }
                    for (ei, e) in v.events.iter().enumerate() {
                        if e.duration == 0 && !e.grace {
                            issues.push(issue("error", "zero_duration", p, Some(st.number), Some(m.number), Some(v.number), format!("event {} has no duration", ei + 1)));
                            failing.push(m.number);
                        }
                        if !e.rest && e.notes.is_empty() {
                            issues.push(issue("warning", "empty_event", p, Some(st.number), Some(m.number), Some(v.number), format!("event {} has no notes and is not a rest", ei + 1)));
                        }
                        if e.rest && !e.notes.is_empty() {
                            issues.push(issue("warning", "rest_with_notes", p, Some(st.number), Some(m.number), Some(v.number), format!("event {} is a rest but has notes", ei + 1)));
                        }
                        for nt in &e.notes {
                            notes += 1;
                            if !(0..=127).contains(&nt.midi) {
                                issues.push(issue("error", "pitch_range", p, Some(st.number), Some(m.number), Some(v.number), format!("event {}: MIDI note {} is outside 0..127", ei + 1, nt.midi)));
                            }
                            if !nt.unpitched && !p.percussion && nt.pitch.midi() != nt.midi {
                                issues.push(issue("error", "pitch_mismatch", p, Some(st.number), Some(m.number), Some(v.number), format!("event {}: {} is MIDI {} but the note says {}", ei + 1, nt.pitch.name(), nt.pitch.midi(), nt.midi)));
                            }
                            if !('A'..='G').contains(&nt.pitch.step) {
                                issues.push(issue("error", "bad_step", p, Some(st.number), Some(m.number), Some(v.number), format!("event {}: step '{}' is not A..G", ei + 1, nt.pitch.step)));
                            }
                            if nt.tie_start && !tie_continues(st, mi, v.number, ei, nt.midi) {
                                issues.push(issue("warning", "unterminated_tie", p, Some(st.number), Some(m.number), Some(v.number), format!("event {}: tie from {} has no matching note", ei + 1, nt.pitch.name())));
                            }
                        }
                        if let Some(t) = &e.note_type {
                            if crate::pitch::TYPES.iter().all(|x| x != t) && t != "breve" {
                                issues.push(issue("warning", "unknown_type", p, Some(st.number), Some(m.number), Some(v.number), format!("event {}: unknown note type '{t}'", ei + 1)));
                            }
                        }
                    }
                }
                for h in &m.harmonies {
                    if h.offset as u64 > expected.max(longest(m)) {
                        issues.push(issue("warning", "harmony_offset", p, Some(st.number), Some(m.number), None, format!("chord symbol {} is placed after the end of the measure", h.text)));
                    }
                    if crate::chords::quality_info(&h.quality).is_none() {
                        issues.push(issue("warning", "unknown_quality", p, Some(st.number), Some(m.number), None, format!("chord quality '{}' is not known", h.quality)));
                    }
                }
            }
        }
    }
    for u in unsupported {
        issues.push(Issue {
            severity: "warning".into(),
            code: "unsupported".into(),
            part: None,
            staff: None,
            measure: None,
            voice: None,
            message: u.clone(),
        });
    }
    for lc in &score.low_confidence {
        issues.push(Issue {
            severity: "info".into(),
            code: "low_confidence".into(),
            part: Some(lc.part.clone()),
            staff: None,
            measure: Some(lc.measure),
            voice: None,
            message: format!("confidence {:.2}: {}", lc.confidence, lc.reasons.join("; ")),
        });
    }
    failing.sort();
    failing.dedup();
    let errors = issues.iter().filter(|i| i.severity == "error").count();
    let warnings = issues.iter().filter(|i| i.severity == "warning").count();
    Report {
        ok: errors == 0,
        errors,
        warnings,
        parts: score.parts.len(),
        measures: score.measure_count(),
        notes,
        issues,
        failing_measures: failing,
        low_confidence: score.low_confidence.clone(),
    }
}

fn longest(m: &Measure) -> u64 {
    m.voices
        .iter()
        .map(|v| v.events.iter().filter(|e| !e.grace).map(|e| e.duration as u64).sum::<u64>())
        .max()
        .unwrap_or(0)
}

/// Does a tie from event `ei` of voice `vn` in measure `mi` reach a note of the same pitch?
fn tie_continues(st: &Staff, mi: usize, vn: u32, ei: usize, midi: i32) -> bool {
    let m = &st.measures[mi];
    if let Some(v) = m.voices.iter().find(|v| v.number == vn) {
        if let Some(next) = v.events.get(ei + 1) {
            return next.notes.iter().any(|n| n.midi == midi);
        }
    }
    // first event of the same voice (or any voice) in the next measure
    if let Some(nm) = st.measures.get(mi + 1) {
        return nm
            .voices
            .iter()
            .filter_map(|v| v.events.first())
            .any(|e| e.notes.iter().any(|n| n.midi == midi));
    }
    false
}

/// One-line summary of the first issues (for error messages).
pub fn summary(r: &Report, max: usize) -> String {
    let errs: Vec<String> = r
        .issues
        .iter()
        .filter(|i| i.severity == "error")
        .take(max)
        .map(|i| {
            let mut loc = Vec::new();
            if let Some(p) = &i.part {
                loc.push(p.clone());
            }
            if let Some(m) = i.measure {
                loc.push(format!("m{m}"));
            }
            if let Some(s) = i.staff {
                loc.push(format!("staff {s}"));
            }
            format!("{} ({}): {}", i.code, loc.join(" "), i.message)
        })
        .collect();
    let more = r.errors.saturating_sub(max);
    if more > 0 {
        format!("{}; and {more} more", errs.join("; "))
    } else {
        errs.join("; ")
    }
}
