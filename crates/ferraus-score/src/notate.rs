//! MIDI → notation: quantization with triplet detection, measures and pickups, voice
//! separation (at most two voices per staff), rests, ties, key estimation and pitch
//! spelling, grand-staff hand split, drum map, chord symbols, lyrics, and confidence.

use crate::chords;
use crate::drums;
use crate::midi::{MidiData, MidiNote};
use crate::model::*;
use crate::pitch::{self, lcm, spell_midi, type_for};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Auto,
    LeadSheet,
    Grand,
    Treble,
    Bass,
    Drums,
    Solfa,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Layout> {
        Some(match s {
            "auto" => Layout::Auto,
            "lead-sheet" | "leadsheet" | "lead" => Layout::LeadSheet,
            "grand" | "piano" => Layout::Grand,
            "treble" => Layout::Treble,
            "bass" => Layout::Bass,
            "drums" | "drum" => Layout::Drums,
            "solfa" => Layout::Solfa,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Triplets {
    Auto,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordMode {
    Off,
    On,
    Diagrams,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    Auto,
    At(i32),
}

#[derive(Clone, Debug)]
pub struct NotateOptions {
    /// Straight grid steps per quarter note (4 for `1/16`).
    pub straight_steps: u32,
    /// Triplet grid steps per quarter note (3 for eighth-note triplets).
    pub triplet_steps: u32,
    pub triplets: Triplets,
    pub time: Option<TimeSig>,
    pub key: Option<KeySig>,
    pub tempo: Option<f64>,
    pub split: Split,
    pub layout: Layout,
    pub chords: ChordMode,
    pub transpose: i32,
    pub title: Option<String>,
    pub composer: Option<String>,
    pub lyrics: Option<String>,
    /// Detect a pickup from a leading partial bar.
    pub pickup: bool,
    /// Per-note confidence aligned with `MidiData::notes` (from a transcriber).
    pub note_confidence: Option<Vec<f32>>,
    /// Measures below this confidence are listed in `low_confidence`.
    pub confidence_threshold: f32,
    /// Layout per MIDI track index (audio stems: vocals → lead sheet, piano → grand...).
    pub track_layouts: Vec<(usize, Layout)>,
    /// Chord symbols from `analyze chords`: (start tick, end tick, symbol). When present
    /// they replace recognition from the notes.
    pub external_chords: Vec<(f64, f64, String)>,
}

impl Default for NotateOptions {
    fn default() -> Self {
        NotateOptions {
            straight_steps: 4,
            triplet_steps: 3,
            triplets: Triplets::Auto,
            time: None,
            key: None,
            tempo: None,
            split: Split::Auto,
            layout: Layout::Auto,
            chords: ChordMode::On,
            transpose: 0,
            title: None,
            composer: None,
            lyrics: None,
            pickup: true,
            note_confidence: None,
            confidence_threshold: 0.6,
            track_layouts: Vec::new(),
            external_chords: Vec::new(),
        }
    }
}

impl NotateOptions {
    /// Configure the grid from a `Grid` (`1/16`, `1/8T`).
    pub fn set_grid(&mut self, g: ferraus_core::units::Grid) {
        let n = (1.0 / g.beats()).round().max(1.0) as u32;
        if g.triplet {
            self.triplet_steps = n.max(3);
            self.straight_steps = (n * 2 / 3).max(1);
            if self.triplets == Triplets::Auto {
                self.triplets = Triplets::On;
            }
        } else {
            self.straight_steps = n;
            self.triplet_steps = (n * 3 / 4).max(3);
        }
    }
}

/// What notation decided, for the JSON result.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct NotateInfo {
    pub key: String,
    pub key_source: String,
    pub key_confidence: f64,
    pub time: String,
    pub tempo_bpm: f64,
    pub pickup_divisions: u32,
    pub measures: usize,
    pub parts: Vec<serde_json::Value>,
    pub chords: Vec<String>,
    pub triplet_beats: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capo: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
struct Win {
    start: u64,
    end: u64,
    triplet: bool,
}

#[derive(Clone, Debug)]
struct MSpan {
    start: u64,
    len: u64,
    /// Full nominal length (differs for a pickup).
    full: u64,
    time: TimeSig,
    implicit: bool,
    number: u32,
}

impl MSpan {
    fn end(&self) -> u64 {
        self.start + self.len
    }
    /// Start of the virtual full measure (for metric alignment of pickups).
    fn anchor(&self) -> i64 {
        self.end() as i64 - self.full as i64
    }
}

#[derive(Clone, Debug)]
struct QNote {
    start: u64,
    end: u64,
    midi: i32,
    vel: u8,
    err: f64,
    conf: Option<f32>,
}

/// A chord or note held by one voice.
#[derive(Clone, Debug)]
struct Item {
    start: u64,
    end: u64,
    notes: Vec<QNote>,
}

pub fn notate(mid: &MidiData, opts: &NotateOptions) -> (Score, NotateInfo) {
    let mut info = NotateInfo::default();
    let ss = opts.straight_steps.max(1) as u64;
    let ts = opts.triplet_steps.max(3) as u64;
    let div = lcm(48, lcm(ss, ts)) as u32;
    let dq = div as u64;
    let ppq = mid.ppq.max(1) as f64;
    let to_div = |tick: u64| tick as f64 * dq as f64 / ppq;

    // ---- measures -----------------------------------------------------------------
    let last_end = mid.notes.iter().map(|n| to_div(n.end)).fold(0.0f64, f64::max);
    let first_start = mid.notes.iter().map(|n| to_div(n.start)).fold(f64::MAX, f64::min);
    let mut sigs: Vec<(u64, TimeSig)> = match opts.time {
        Some(t) => vec![(0, t)],
        None => mid
            .time_sigs
            .iter()
            .map(|(t, n, d)| (to_div(*t).round() as u64, TimeSig::new((*n).max(1), (*d).max(1))))
            .collect(),
    };
    if sigs.is_empty() || sigs[0].0 > 0 {
        sigs.insert(0, (0, TimeSig::new(4, 4)));
    }
    sigs.dedup_by_key(|s| s.0);
    let mut spans: Vec<MSpan> = Vec::new();
    let mut pos = 0u64;
    let mut number = 1u32;
    let total_end = (last_end.max(1.0) - 0.25).max(0.0);
    while (pos as f64) < total_end || spans.is_empty() {
        let si = sigs.iter().rposition(|s| s.0 <= pos).unwrap_or(0);
        let t = sigs[si].1;
        let full = t.measure_divisions(div) as u64;
        let mut len = full;
        // a time-signature change before the measure ends shortens it
        if let Some(next) = sigs.get(si + 1) {
            if next.0 < pos + full && next.0 > pos {
                len = next.0 - pos;
            }
        }
        spans.push(MSpan { start: pos, len, full, time: t, implicit: false, number });
        number += 1;
        pos += len.max(1);
        if spans.len() > 100_000 {
            break;
        }
    }
    // pickup (a): a short first measure followed by a different signature
    if opts.pickup && spans.len() > 1 && opts.time.is_none() {
        let s0 = spans[0].clone();
        let s1 = spans[1].clone();
        if s0.time != s1.time && s0.len < s1.full && sigs.len() > 1 && sigs[1].0 == s0.len {
            spans[0].time = s1.time;
            spans[0].full = s1.full;
            spans[0].implicit = true;
        }
    }
    // pickup (b): the first note starts in the second half of the first bar
    if opts.pickup && !spans[0].implicit && first_start.is_finite() && spans.len() > 1 {
        let s0 = &spans[0];
        let fs = (first_start / (dq as f64 / ss as f64)).round() * (dq as f64 / ss as f64);
        let beat = s0.time.beat_divisions(div) as f64;
        if fs >= s0.len as f64 / 2.0 && fs < s0.len as f64 && (fs % beat).abs() < 1e-6 {
            let p = fs as u64;
            spans[0].start = p;
            spans[0].len -= p;
            spans[0].implicit = true;
        }
    }
    if spans[0].implicit {
        info.pickup_divisions = spans[0].len as u32;
        let mut n = 0u32;
        for s in spans.iter_mut() {
            s.number = n;
            n += 1;
        }
    }

    // ---- beat windows and triplet detection --------------------------------------
    let mut wins: Vec<Win> = Vec::new();
    for s in &spans {
        let anchor = s.anchor();
        let mut k = 0i64;
        loop {
            let ws = anchor + k * dq as i64;
            let we = ws + dq as i64;
            k += 1;
            if ws >= s.end() as i64 {
                break;
            }
            if we <= s.start as i64 {
                continue;
            }
            let a = ws.max(s.start as i64) as u64;
            let b = (we as u64).min(s.end());
            if b > a {
                wins.push(Win { start: a, end: b, triplet: false });
            }
        }
    }
    let straight = dq / ss;
    let tstep = dq / ts;
    let half = straight as f64 / 2.0;
    let win_index = |wins: &[Win], t: f64| -> usize {
        // window whose [start - half, end - half) contains t
        let probe = t + half;
        match wins.binary_search_by(|w| {
            if probe < w.start as f64 {
                std::cmp::Ordering::Greater
            } else if probe >= w.end as f64 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(i) => i,
            Err(i) => i.min(wins.len().saturating_sub(1)),
        }
    };
    if opts.triplets != Triplets::Off {
        let mut per_win: Vec<Vec<f64>> = vec![Vec::new(); wins.len()];
        for n in &mid.notes {
            let t = to_div(n.start);
            if !wins.is_empty() {
                per_win[win_index(&wins, t)].push(t);
            }
        }
        for (wi, onsets) in per_win.iter().enumerate() {
            let w = &wins[wi];
            if w.end - w.start != dq || onsets.is_empty() {
                continue;
            }
            let snap = |t: f64, step: u64| -> (f64, f64) {
                let r = ((t - w.start as f64) / step as f64).round() * step as f64 + w.start as f64;
                (r, (t - r).abs())
            };
            let es: f64 = onsets.iter().map(|t| snap(*t, straight).1).sum();
            let et: f64 = onsets.iter().map(|t| snap(*t, tstep).1).sum();
            // a triplet beat needs an onset that is on the triplet grid but off the straight one
            let offbeat = onsets.iter().any(|t| {
                let (r, _) = snap(*t, tstep);
                let rel = (r - w.start as f64).round() as u64;
                rel % dq != 0 && rel % straight != 0
            });
            let use_t = match opts.triplets {
                Triplets::Auto => offbeat && et + 1e-9 < es * 0.5,
                Triplets::On => offbeat && et <= es + 1e-9,
                Triplets::Off => false,
            };
            if use_t {
                wins[wi].triplet = true;
            }
        }
    }
    info.triplet_beats = wins.iter().filter(|w| w.triplet).count();
    let quant = |t: f64| -> (u64, f64) {
        if wins.is_empty() {
            return (t.max(0.0).round() as u64, 0.0);
        }
        let w = &wins[win_index(&wins, t)];
        let step = if w.triplet { tstep } else { straight };
        let k = ((t - w.start as f64) / step as f64).round();
        let r = (w.start as f64 + k * step as f64).clamp(w.start as f64, w.end as f64);
        let err = (t - r).abs() / (step as f64 / 2.0);
        (r.max(0.0) as u64, err.min(1.0))
    };
    let step_at = |p: u64| -> u64 {
        if wins.is_empty() {
            return straight;
        }
        let w = &wins[win_index(&wins, p as f64 - half + 0.5)];
        if w.triplet {
            tstep
        } else {
            straight
        }
    };

    // ---- group notes into parts --------------------------------------------------
    let mut groups: BTreeMap<(usize, u8), Vec<(usize, &MidiNote)>> = BTreeMap::new();
    for (i, n) in mid.notes.iter().enumerate() {
        groups.entry((n.track, n.channel)).or_default().push((i, n));
    }
    let all_drums = opts.layout == Layout::Drums;
    let mut pitched_q: Vec<QNote> = Vec::new();
    struct PartIn {
        track: usize,
        name: String,
        program: Option<u8>,
        channel: u8,
        drums: bool,
        notes: Vec<QNote>,
    }
    let mut parts_in: Vec<PartIn> = Vec::new();
    let pickup_start = spans[0].start;
    for ((track, ch), notes) in &groups {
        let forced = opts.track_layouts.iter().find(|t| t.0 == *track).map(|t| t.1);
        let drums = *ch == 9 || all_drums || forced == Some(Layout::Drums);
        let mut q: Vec<QNote> = Vec::new();
        for (i, n) in notes {
            let (s, err) = quant(to_div(n.start));
            let (mut e, _) = quant(to_div(n.end));
            let s = s.max(pickup_start);
            if e <= s {
                e = s + step_at(s);
            }
            let midi = if drums { n.key as i32 } else { n.key as i32 + opts.transpose };
            q.push(QNote {
                start: s,
                end: e,
                midi,
                vel: n.velocity,
                err,
                conf: opts.note_confidence.as_ref().and_then(|c| c.get(*i).copied()),
            });
        }
        // merge exact duplicates
        q.sort_by_key(|n| (n.start, n.midi, n.end));
        q.dedup_by(|a, b| a.start == b.start && a.midi == b.midi);
        let tr = mid.tracks.get(*track);
        let program = tr.and_then(|t| t.program.map(|p| p.1));
        let name = tr
            .and_then(|t| t.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                if drums {
                    "Drums".into()
                } else {
                    program.map(|p| drums::GM_PROGRAMS[p as usize].to_string()).unwrap_or_else(|| "Piano".into())
                }
            });
        if !drums {
            pitched_q.extend(q.iter().cloned());
        }
        parts_in.push(PartIn { track: *track, name, program, channel: *ch, drums, notes: q });
    }
    // keep quantized notes inside the measures
    let last_end_div = spans.last().map(|s| s.end()).unwrap_or(0);
    for p in parts_in.iter_mut() {
        for n in p.notes.iter_mut() {
            n.end = n.end.min(last_end_div);
            if n.start >= n.end {
                n.start = n.end.saturating_sub(1);
            }
        }
        p.notes.retain(|n| n.end > n.start);
    }
    for n in pitched_q.iter_mut() {
        n.end = n.end.min(last_end_div);
    }
    pitched_q.retain(|n| n.end > n.start);

    // ---- key ----------------------------------------------------------------------
    let (key, key_source, key_conf) = if let Some(k) = opts.key {
        (pitch::transpose_key(k, 0), "given".to_string(), 1.0)
    } else if let Some((_, sf, minor)) = mid.key_sigs.first() {
        let k = KeySig { fifths: (*sf).clamp(-7, 7), mode: if *minor { Mode::Minor } else { Mode::Major } };
        (pitch::transpose_key(k, opts.transpose), "midi".to_string(), 1.0)
    } else {
        let mut hist = [0.0f64; 12];
        for n in &pitched_q {
            hist[n.midi.rem_euclid(12) as usize] += (n.end - n.start) as f64;
        }
        let (k, c) = pitch::estimate_key(&hist);
        (k, "estimated".to_string(), c)
    };
    info.key = pitch::key_name(key);
    info.key_source = key_source;
    info.key_confidence = (key_conf * 100.0).round() / 100.0;
    info.time = format!("{}/{}", spans.get(1).unwrap_or(&spans[0]).time.beats, spans.get(1).unwrap_or(&spans[0]).time.beat_type);
    let bpm = opts.tempo.or_else(|| mid.first_bpm()).unwrap_or(120.0);
    info.tempo_bpm = (bpm * 100.0).round() / 100.0;

    // ---- build parts ----------------------------------------------------------------
    let mut score = Score {
        divisions: div,
        title: opts.title.clone().or_else(|| mid.title.clone()),
        composer: opts.composer.clone(),
        ..Score::default()
    };
    let ctx = Ctx { spans: &spans, wins: &wins, div, dq };
    for (pi, p) in parts_in.iter().enumerate() {
        let id = format!("P{}", pi + 1);
        if p.drums {
            let staff = notate_drums(&ctx, &p.notes);
            score.parts.push(Part {
                id,
                name: p.name.clone(),
                abbreviation: None,
                program: None,
                channel: Some(10),
                percussion: true,
                staves: vec![staff],
            });
            info.parts.push(serde_json::json!({"id": format!("P{}", pi + 1), "name": p.name, "layout": "drums", "notes": p.notes.len()}));
            continue;
        }
        let forced = opts.track_layouts.iter().find(|t| t.0 == p.track).map(|t| t.1);
        let layout = match forced.unwrap_or(opts.layout) {
            Layout::Auto => auto_layout(&p.notes),
            Layout::Solfa => Layout::LeadSheet,
            l => l,
        };
        let staves: Vec<Staff> = match layout {
            Layout::Grand => {
                let (rh, lh) = split_hands(&p.notes, opts.split);
                vec![
                    notate_staff(&ctx, &rh, 1, Clef::treble(), key),
                    notate_staff(&ctx, &lh, 2, Clef::bass(), key),
                ]
            }
            Layout::Bass => vec![notate_staff(&ctx, &p.notes, 1, Clef::bass(), key)],
            _ => {
                let clef = if layout == Layout::LeadSheet && median_pitch(&p.notes) < 55 { Clef::bass() } else { Clef::treble() };
                vec![notate_staff(&ctx, &p.notes, 1, clef, key)]
            }
        };
        let lname = match layout {
            Layout::Grand => "grand",
            Layout::Bass => "bass",
            Layout::Treble => "treble",
            _ => "lead-sheet",
        };
        info.parts.push(serde_json::json!({"id": id, "name": p.name, "layout": lname, "notes": p.notes.len()}));
        let channel = Some(p.channel + 1);
        score.parts.push(Part {
            id,
            name: p.name.clone(),
            abbreviation: None,
            program: p.program.or(Some(0)),
            channel,
            percussion: false,
            staves,
        });
    }
    if score.parts.is_empty() {
        // empty MIDI: one empty lead sheet
        score.parts.push(Part {
            id: "P1".into(),
            name: "Melody".into(),
            abbreviation: None,
            program: Some(0),
            channel: Some(1),
            percussion: false,
            staves: vec![notate_staff(&ctx, &[], 1, Clef::treble(), key)],
        });
    }

    // tempo marks
    let mut tempos: Vec<(u64, f64)> = match opts.tempo {
        Some(t) => vec![(spans[0].start, t)],
        None => mid.tempos.iter().map(|(t, us)| (to_div(*t).round() as u64, 60_000_000.0 / *us as f64)).collect(),
    };
    if tempos.is_empty() {
        tempos.push((spans[0].start, 120.0));
    }
    tempos.dedup_by(|a, b| (a.1 - b.1).abs() < 1e-6);
    for p in score.parts.iter_mut().take(1) {
        let st = &mut p.staves[0];
        for (t, b) in &tempos {
            let t = (*t).max(spans[0].start);
            if let Some(mi) = spans.iter().position(|s| t >= s.start && t < s.end()) {
                st.measures[mi].tempo.push(TempoMark { offset: (t - spans[mi].start) as u32, bpm: (*b * 1000.0).round() / 1000.0, text: None });
            }
        }
    }

    // chord symbols
    if opts.chords != ChordMode::Off && (!pitched_q.is_empty() || !opts.external_chords.is_empty()) {
        let mut beats: Vec<(u64, u64)> = Vec::new();
        let mut bar_starts: Vec<bool> = Vec::new();
        for s in &spans {
            let b = s.time.beat_divisions(div) as u64;
            let anchor = s.anchor();
            let mut k = 0i64;
            loop {
                let bs = anchor + k * b as i64;
                k += 1;
                if bs >= s.end() as i64 {
                    break;
                }
                let be = (bs + b as i64).min(s.end() as i64);
                if be <= s.start as i64 {
                    continue;
                }
                let a = bs.max(s.start as i64) as u64;
                bar_starts.push(a == s.start);
                beats.push((a, be as u64));
            }
        }
        let notes: Vec<(u64, u64, i32)> = pitched_q.iter().map(|n| (n.start, n.end, n.midi)).collect();
        let spans_c = if opts.external_chords.is_empty() {
            chords::recognize(&notes, &beats, &bar_starts)
        } else {
            external_spans(&opts.external_chords, &quant, &to_div)
        };
        let capo = if opts.transpose != 0 { chords::capo_for(key) } else { None };
        if opts.transpose != 0 {
            info.capo = Some(match capo {
                Some((c, k)) => serde_json::json!({"fret": c, "shapes_in": pitch::key_name(k)}),
                None => serde_json::json!({"fret": 0, "shapes_in": pitch::key_name(key)}),
            });
        }
        if let Some(target) = score.parts.iter_mut().find(|p| !p.percussion) {
            let st = &mut target.staves[0];
            for c in &spans_c {
                let (root, bass) = c.spell(key);
                let text = chords::chord_text(root, &c.quality, bass);
                info.chords.push(text.clone());
                if let Some(mi) = spans.iter().position(|s| c.start >= s.start && c.start < s.end()) {
                    let mut h = Harmony {
                        offset: (c.start - spans[mi].start) as u32,
                        root,
                        quality: c.quality.clone(),
                        bass,
                        text,
                        frame: None,
                        confidence: Some((c.confidence * 100.0).round() / 100.0),
                    };
                    if opts.chords == ChordMode::Diagrams {
                        let (shape_root, shape_name, cap) = match capo {
                            Some((cf, k)) => {
                                let r = (c.root_pc - cf as i32).rem_euclid(12);
                                let nm = chords::chord_text(pitch::spell_pc(r, k), &c.quality, None);
                                (r, Some(nm), Some(cf))
                            }
                            None => (c.root_pc, None, None),
                        };
                        if let Some(shape) = chords::guitar_shape(shape_root, &c.quality) {
                            let mut fr = chords::frame_for(shape);
                            fr.shape = shape_name;
                            fr.capo = cap;
                            h.frame = Some(fr);
                        }
                    }
                    st.measures[mi].harmonies.push(h);
                }
            }
            if let (ChordMode::Diagrams, Some((c, _))) = (opts.chords, capo) {
                st.measures[0].directions.push(Direction { offset: 0, kind: "words".into(), value: format!("Capo {c}"), placement: Some("above".into()) });
            }
        }
    }

    // lyrics: one syllable per melody note
    if let Some(text) = &opts.lyrics {
        if let Some(p) = score.parts.iter_mut().find(|p| !p.percussion) {
            apply_lyrics(&mut p.staves[0], text);
        }
    }

    // metadata
    for p in score.parts.iter_mut() {
        for st in p.staves.iter_mut() {
            if let Some(m0) = st.measures.first_mut() {
                if !p.percussion {
                    m0.key = Some(key);
                }
                m0.time = Some(spans[0].time);
            }
            for (mi, s) in spans.iter().enumerate().skip(1) {
                if s.time != spans[mi - 1].time {
                    st.measures[mi].time = Some(s.time);
                }
            }
        }
    }
    // final barline
    for p in score.parts.iter_mut() {
        for st in p.staves.iter_mut() {
            if let Some(m) = st.measures.last_mut() {
                m.barline = Some("light-heavy".into());
            }
        }
    }
    score.refresh_low_confidence(opts.confidence_threshold);
    info.measures = spans.len();
    (score, info)
}

/// Chord spans from external labels (ticks) snapped to the grid.
fn external_spans(ext: &[(f64, f64, String)], quant: &dyn Fn(f64) -> (u64, f64), to_div: &dyn Fn(u64) -> f64) -> Vec<chords::ChordSpan> {
    let mut out: Vec<chords::ChordSpan> = Vec::new();
    for (s, e, label) in ext {
        let Some((root, quality, bass)) = chords::parse_symbol(label) else { continue };
        let (qs, _) = quant(to_div(s.max(0.0) as u64));
        let (qe, _) = quant(to_div(e.max(0.0) as u64));
        if qe <= qs {
            continue;
        }
        if out.last().map(|l: &chords::ChordSpan| l.start == qs).unwrap_or(false) {
            out.pop();
        }
        out.push(chords::ChordSpan { start: qs, end: qe, root_pc: root.pc(), quality, bass_pc: bass.map(|b| b.pc()), confidence: 0.7 });
    }
    out
}

struct Ctx<'a> {
    spans: &'a [MSpan],
    wins: &'a [Win],
    div: u32,
    dq: u64,
}

fn median_pitch(notes: &[QNote]) -> i32 {
    if notes.is_empty() {
        return 67;
    }
    let mut v: Vec<i32> = notes.iter().map(|n| n.midi).collect();
    v.sort();
    v[v.len() / 2]
}

fn auto_layout(notes: &[QNote]) -> Layout {
    if notes.is_empty() {
        return Layout::LeadSheet;
    }
    let mut v: Vec<i32> = notes.iter().map(|n| n.midi).collect();
    v.sort();
    let lo = v[v.len() * 5 / 100];
    let hi = v[(v.len() * 95 / 100).min(v.len() - 1)];
    let below = v.iter().filter(|m| **m < 60).count() as f64 / v.len() as f64;
    let above = v.iter().filter(|m| **m >= 60).count() as f64 / v.len() as f64;
    // polyphony: share of notes overlapping another
    let mut overl = 0usize;
    for (i, a) in notes.iter().enumerate() {
        if notes.iter().enumerate().any(|(j, b)| i != j && b.start < a.end && a.start < b.end) {
            overl += 1;
        }
    }
    let poly = overl as f64 / notes.len() as f64;
    if hi - lo > 24 || (poly > 0.3 && below > 0.2 && above > 0.2) {
        Layout::Grand
    } else {
        Layout::LeadSheet
    }
}

/// Split notes into right and left hand.
fn split_hands(notes: &[QNote], split: Split) -> (Vec<QNote>, Vec<QNote>) {
    let mut rh = Vec::new();
    let mut lh = Vec::new();
    match split {
        Split::At(m) => {
            for n in notes {
                if n.midi >= m {
                    rh.push(n.clone());
                } else {
                    lh.push(n.clone());
                }
            }
        }
        Split::Auto => {
            // cluster by onset; choose a split per cluster around middle C that keeps
            // each hand within a hand span and close to its previous position
            let mut clusters: BTreeMap<u64, Vec<QNote>> = BTreeMap::new();
            for n in notes {
                clusters.entry(n.start).or_default().push(n.clone());
            }
            let mut rc = 67.0f64;
            let mut lc = 50.0f64;
            for (_, mut c) in clusters {
                c.sort_by_key(|n| n.midi);
                let k = c.len();
                let mut best = (f64::MAX, 0usize);
                for cut in 0..=k {
                    // notes [0, cut) → LH, [cut, k) → RH
                    let l = &c[..cut];
                    let r = &c[cut..];
                    let mut cost = 0.0;
                    let span = |s: &[QNote]| s.last().map(|x| x.midi).unwrap_or(0) - s.first().map(|x| x.midi).unwrap_or(0);
                    if span(l) > 14 {
                        cost += 50.0 + span(l) as f64;
                    }
                    if span(r) > 14 {
                        cost += 50.0 + span(r) as f64;
                    }
                    if l.len() > 5 || r.len() > 5 {
                        cost += 30.0;
                    }
                    for n in l {
                        cost += (n.midi as f64 - lc).abs() * 0.3;
                        if n.midi > 64 {
                            cost += (n.midi - 64) as f64 * 2.0;
                        }
                    }
                    for n in r {
                        cost += (n.midi as f64 - rc).abs() * 0.3;
                        if n.midi < 55 {
                            cost += (55 - n.midi) as f64 * 2.0;
                        }
                    }
                    // middle-C prior
                    for n in l {
                        if n.midi >= 60 {
                            cost += 1.5;
                        }
                    }
                    for n in r {
                        if n.midi < 60 {
                            cost += 1.5;
                        }
                    }
                    if cost < best.0 {
                        best = (cost, cut);
                    }
                }
                let (l, r) = c.split_at(best.1);
                if !l.is_empty() {
                    lc = 0.7 * lc + 0.3 * (l.iter().map(|n| n.midi as f64).sum::<f64>() / l.len() as f64);
                }
                if !r.is_empty() {
                    rc = 0.7 * rc + 0.3 * (r.iter().map(|n| n.midi as f64).sum::<f64>() / r.len() as f64);
                }
                lh.extend(l.iter().cloned());
                rh.extend(r.iter().cloned());
            }
        }
    }
    (rh, lh)
}

/// Assign notes to at most two voices.
fn separate_voices(notes: &[QNote]) -> [Vec<Item>; 2] {
    let mut sorted: Vec<QNote> = notes.to_vec();
    sorted.sort_by(|a, b| a.start.cmp(&b.start).then(b.midi.cmp(&a.midi)));
    let mut voices: [Vec<Item>; 2] = [Vec::new(), Vec::new()];
    let mut i = 0;
    while i < sorted.len() {
        let s = sorted[i].start;
        let mut j = i;
        while j < sorted.len() && sorted[j].start == s {
            j += 1;
        }
        // group by end
        let mut groups: Vec<Item> = Vec::new();
        for n in &sorted[i..j] {
            match groups.iter_mut().find(|g| g.end == n.end) {
                Some(g) => g.notes.push(n.clone()),
                None => groups.push(Item { start: s, end: n.end, notes: vec![n.clone()] }),
            }
        }
        groups.sort_by_key(|g| -g.notes.iter().map(|n| n.midi).max().unwrap_or(0));
        let mut taken = [false, false];
        for g in groups {
            let free = |v: &Vec<Item>| v.last().map(|it| it.end <= s).unwrap_or(true);
            let target = if !taken[0] && free(&voices[0]) {
                Some(0)
            } else if !taken[1] && free(&voices[1]) {
                Some(1)
            } else {
                None
            };
            match target {
                Some(v) => {
                    voices[v].push(g);
                    taken[v] = true;
                }
                None => {
                    if taken[0] || taken[1] {
                        // merge into the chord already started at this onset with the closest end
                        let v = if taken[0] && taken[1] {
                            let d0 = voices[0].last().unwrap().end.abs_diff(g.end);
                            let d1 = voices[1].last().unwrap().end.abs_diff(g.end);
                            if d0 <= d1 {
                                0
                            } else {
                                1
                            }
                        } else if taken[0] {
                            0
                        } else {
                            1
                        };
                        let last = voices[v].last_mut().unwrap();
                        for mut n in g.notes {
                            n.end = last.end;
                            last.notes.push(n);
                        }
                    } else {
                        // both voices still sounding: cut the one that ends first
                        let v = if voices[0].last().unwrap().end <= voices[1].last().unwrap().end { 0 } else { 1 };
                        let last = voices[v].last_mut().unwrap();
                        last.end = s.max(last.start + 1);
                        for n in last.notes.iter_mut() {
                            n.end = last.end;
                        }
                        voices[v].push(g);
                        taken[v] = true;
                    }
                }
            }
        }
        i = j;
    }
    // voice 1 must be the busier upper voice; if voice 1 is empty but voice 2 isn't, swap
    if voices[0].is_empty() && !voices[1].is_empty() {
        voices.swap(0, 1);
    }
    voices
}

/// Notatable straight durations (in divisions) for a measure context.
fn straight_values(div: u32) -> Vec<u32> {
    let mut v: Vec<u32> = Vec::new();
    for i in 0..7u32 {
        for dots in 0..=2u32 {
            let base = 4 * div as u64;
            let num = base * ((1u64 << (dots + 1)) - 1);
            let den = (1u64 << i) * (1u64 << dots);
            if num % den == 0 {
                v.push((num / den) as u32);
            }
        }
    }
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.dedup();
    v
}

fn base_of(len: u32, div: u32) -> u32 {
    // undotted value for a (possibly dotted) duration
    match type_for(len, div, None) {
        Some((t, _)) => pitch::duration_of(t, 0, div, None).unwrap_or(len),
        None => len,
    }
}

/// Decompose a span [a, b) inside measure `s` into notatable pieces (duration, tuplet).
fn decompose(ctx: &Ctx, s: &MSpan, a: u64, b: u64) -> Vec<(u32, Option<(u64, u64)>)> {
    let mut out = Vec::new();
    let vals = straight_values(ctx.div);
    let anchor = s.anchor();
    let mut p = a;
    while p < b {
        // window containing p
        let wi = ctx.wins.partition_point(|w| w.end <= p);
        let w = ctx.wins.get(wi).filter(|w| w.start <= p && p < w.end);
        if let Some(w) = w.filter(|w| w.triplet) {
            let e = b.min(w.end);
            if p == w.start && e == w.end {
                out.push(((e - p) as u32, None));
                p = e;
                continue;
            }
            // pieces in triplet units of this window
            let mut q = p;
            while q < e {
                let mut len = e - q;
                loop {
                    if type_for(len as u32, ctx.div, Some((3, 2))).is_some() {
                        break;
                    }
                    len -= 1;
                    if len == 0 {
                        break;
                    }
                }
                if len == 0 {
                    len = e - q;
                }
                out.push((len as u32, Some((w.start, w.end))));
                q += len;
            }
            p = e;
            continue;
        }
        // straight region: up to the next triplet window or b
        let mut e = b;
        for w in ctx.wins[wi.min(ctx.wins.len())..].iter() {
            if w.start >= b {
                break;
            }
            if w.triplet && w.start > p {
                e = e.min(w.start);
                break;
            }
        }
        let rel = (p as i64 - anchor) as u64;
        let rem = (e - p) as u32;
        let mut chosen = None;
        for &v in &vals {
            if v > rem {
                continue;
            }
            let base = base_of(v, ctx.div) as u64;
            let ok = rel % base == 0 || rel % v as u64 == 0;
            // a value must not cross the middle of a 4/4 bar unless it starts the bar
            if ok {
                chosen = Some(v);
                break;
            }
        }
        let v = chosen.unwrap_or_else(|| {
            // smallest unit fallback (odd remainders)
            let g = pitch::gcd(rel.max(1), rem as u64) as u32;
            g.min(rem).max(1)
        });
        out.push((v, None));
        p += v as u64;
    }
    out
}

/// Turn voice items into measures of events.
fn build_measures(ctx: &Ctx, items: &[Item], voice_no: u32, key: KeySig, drums: bool, keep_empty: bool) -> Vec<Option<Voice>> {
    let mut out = Vec::new();
    for s in ctx.spans {
        let ms = s.start;
        let me = s.end();
        let mut events: Vec<Event> = Vec::new();
        let mut segs: Vec<(u64, u64, Option<&Item>)> = Vec::new();
        let mut p = ms;
        let mut has_notes = false;
        for it in items.iter().filter(|it| it.end > ms && it.start < me) {
            let a = it.start.max(ms);
            let b = it.end.min(me);
            if a > p {
                segs.push((p, a, None));
            }
            if b > a.max(p) {
                segs.push((a.max(p), b, Some(it)));
                has_notes = true;
            }
            p = p.max(b);
        }
        if p < me {
            segs.push((p, me, None));
        }
        if !has_notes {
            if keep_empty {
                let mut r = Event::rest(s.len as u32);
                r.measure_rest = true;
                out.push(Some(Voice { number: voice_no, events: vec![r] }));
            } else {
                out.push(None);
            }
            continue;
        }
        for (a, b, it) in segs {
            let pieces = decompose(ctx, s, a, b);
            let n = pieces.len();
            let mut q = a;
            for (pi, (d, tw)) in pieces.into_iter().enumerate() {
                let mut ev = match it {
                    None => Event::rest(d),
                    Some(it) if drums && pi > 0 => {
                        let _ = it;
                        Event::rest(d)
                    }
                    Some(it) => {
                        let first_piece = q == it.start;
                        let last_piece = q + d as u64 == it.end;
                        let mut notes: Vec<Note> = it
                            .notes
                            .iter()
                            .map(|qn| {
                                let mut nt = if drums {
                                    let sp = drums::spec(qn.midi.clamp(0, 127) as u8);
                                    let mut nt = Note::new(drums::display_pitch(sp.key));
                                    nt.midi = qn.midi;
                                    nt.unpitched = true;
                                    nt.notehead = sp.notehead.map(|h| h.to_string());
                                    nt.instrument = Some(sp.name.to_string());
                                    nt
                                } else {
                                    Note::new(spell_midi(qn.midi, key))
                                };
                                if !drums {
                                    nt.tie_stop = !first_piece || (pi > 0);
                                    nt.tie_start = !last_piece || (pi + 1 < n);
                                }
                                nt.velocity = Some(qn.vel);
                                let c = note_conf(qn);
                                if c < 0.999 {
                                    nt.confidence = Some((c * 100.0).round() / 100.0);
                                }
                                nt
                            })
                            .collect();
                        notes.sort_by_key(|nt| (nt.pitch.midi(), nt.midi));
                        notes.dedup_by(|x, y| x.midi == y.midi);
                        let mut ev = Event::notes(d, notes);
                        let c: f32 = it.notes.iter().map(note_conf).fold(1.0, f32::min);
                        if c < 0.999 {
                            ev.confidence = Some((c * 100.0).round() / 100.0);
                            ev.confidence_reason = Some(if it.notes.iter().any(|n| n.err > 0.6) {
                                "timing far from the grid".into()
                            } else {
                                "transcriber confidence".into()
                            });
                        }
                        ev
                    }
                };
                if let Some((ws, we)) = tw {
                    ev.tuplet = Some(Tuplet { actual: 3, normal: 2, start: q == ws, stop: q + d as u64 == we });
                }
                events.push(ev);
                q += d as u64;
            }
        }
        // tuplet brackets: start on the first event of each window, stop on the last
        fix_tuplet_flags(&mut events, ctx, s);
        out.push(Some(Voice { number: voice_no, events }));
    }
    out
}

fn fix_tuplet_flags(events: &mut [Event], _ctx: &Ctx, s: &MSpan) {
    let mut pos = s.start;
    let mut bounds: Vec<(u64, u64)> = Vec::new();
    for e in events.iter() {
        bounds.push((pos, pos + e.duration as u64));
        pos += e.duration as u64;
    }
    let n = events.len();
    for i in 0..n {
        if let Some(t) = events[i].tuplet {
            let prev_t = i > 0 && events[i - 1].tuplet.is_some() && !events[i - 1].tuplet.unwrap().stop;
            let next_t = i + 1 < n && events[i + 1].tuplet.is_some() && !t.stop;
            let mut t2 = t;
            t2.start = t.start || !prev_t;
            t2.stop = t.stop || !next_t;
            events[i].tuplet = Some(t2);
        }
    }
}

fn note_conf(n: &QNote) -> f32 {
    let timing = 1.0 - (n.err as f32 * 0.5).clamp(0.0, 0.5);
    let ext = n.conf.unwrap_or(1.0);
    (timing * ext).clamp(0.0, 1.0)
}

fn notate_staff(ctx: &Ctx, notes: &[QNote], number: u32, clef: Clef, key: KeySig) -> Staff {
    let voices = separate_voices(notes);
    let v1 = build_measures(ctx, &voices[0], 1, key, false, true);
    let v2 = build_measures(ctx, &voices[1], 2, key, false, false);
    let mut measures = Vec::new();
    for (mi, s) in ctx.spans.iter().enumerate() {
        let mut vs = Vec::new();
        if let Some(v) = v1[mi].clone() {
            vs.push(v);
        }
        if let Some(v) = v2[mi].clone() {
            vs.push(v);
        }
        let mut m = Measure {
            number: s.number,
            implicit: s.implicit,
            voices: vs,
            ..Measure::default()
        };
        if mi == 0 {
            m.clef = Some(clef.clone());
        }
        measure_confidence(&mut m, notes, s);
        measures.push(m);
    }
    Staff { number, measures }
}

fn measure_confidence(m: &mut Measure, notes: &[QNote], s: &MSpan) {
    let inside: Vec<&QNote> = notes.iter().filter(|n| n.start >= s.start && n.start < s.end()).collect();
    if inside.is_empty() {
        return;
    }
    let mut c: f32 = inside.iter().map(|n| note_conf(n)).sum::<f32>() / inside.len() as f32;
    let mut reasons = Vec::new();
    // density: max simultaneous notes
    let mut maxpoly = 0usize;
    for n in &inside {
        let k = notes.iter().filter(|o| o.start <= n.start && o.end > n.start).count();
        maxpoly = maxpoly.max(k);
    }
    if maxpoly > 6 {
        c *= 0.8;
        reasons.push("polyphony too dense".to_string());
    }
    let mean_err = inside.iter().map(|n| n.err).sum::<f64>() / inside.len() as f64;
    if mean_err > 0.5 {
        reasons.push("timing far from the grid (tempo unstable or rubato)".to_string());
    }
    let mean_vel = inside.iter().map(|n| n.vel as f64).sum::<f64>() / inside.len() as f64;
    if mean_vel < 25.0 {
        c *= 0.9;
        reasons.push("low-energy passage".to_string());
    }
    if inside.iter().any(|n| n.conf.map(|x| x < 0.5).unwrap_or(false)) {
        reasons.push("transcriber confidence low".to_string());
    }
    m.confidence = Some((c * 100.0).round() / 100.0);
    m.confidence_reasons = reasons;
}

fn notate_drums(ctx: &Ctx, notes: &[QNote]) -> Staff {
    // hands (voice 1) and feet (voice 2); each hit lasts until the next hit in its voice,
    // capped at one beat
    let mut hands: Vec<QNote> = Vec::new();
    let mut feet: Vec<QNote> = Vec::new();
    for n in notes {
        if drums::spec(n.midi.clamp(0, 127) as u8).foot {
            feet.push(n.clone());
        } else {
            hands.push(n.clone());
        }
    }
    let to_items = |v: &[QNote]| -> Vec<Item> {
        let mut by_onset: BTreeMap<u64, Vec<QNote>> = BTreeMap::new();
        for n in v {
            by_onset.entry(n.start).or_default().push(n.clone());
        }
        let onsets: Vec<u64> = by_onset.keys().copied().collect();
        let mut items = Vec::new();
        for (i, (s, ns)) in by_onset.into_iter().enumerate() {
            let next = onsets.get(i + 1).copied().unwrap_or(u64::MAX);
            // measure end
            let mend = ctx.spans.iter().find(|sp| s >= sp.start && s < sp.end()).map(|sp| sp.end()).unwrap_or(s + ctx.dq);
            let end = (s + ctx.dq).min(next).min(mend).max(s + 1);
            let mut notes = ns;
            for n in notes.iter_mut() {
                n.end = end;
            }
            items.push(Item { start: s, end, notes });
        }
        items
    };
    let hi = to_items(&hands);
    let fi = to_items(&feet);
    let key = KeySig::major(0);
    let v1 = build_measures(ctx, &hi, 1, key, true, true);
    let v2 = build_measures(ctx, &fi, 2, key, true, false);
    let mut measures = Vec::new();
    for (mi, s) in ctx.spans.iter().enumerate() {
        let mut vs = Vec::new();
        if let Some(v) = v1[mi].clone() {
            vs.push(v);
        }
        if let Some(v) = v2[mi].clone() {
            vs.push(v);
        }
        let mut m = Measure { number: s.number, implicit: s.implicit, voices: vs, ..Measure::default() };
        if mi == 0 {
            m.clef = Some(Clef::percussion());
        }
        measure_confidence(&mut m, notes, s);
        measures.push(m);
    }
    Staff { number: 1, measures }
}

/// Split lyric text into syllables: whitespace separates words, `-` separates syllables
/// inside a word, `_` holds the previous syllable over a note.
pub fn syllables(text: &str) -> Vec<Option<Lyric>> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        if word == "_" {
            out.push(None);
            continue;
        }
        let parts: Vec<&str> = word.split('-').filter(|s| !s.is_empty()).collect();
        let n = parts.len();
        for (i, p) in parts.iter().enumerate() {
            let syllabic = match (n, i) {
                (1, _) => "single",
                (_, 0) => "begin",
                (_, i) if i + 1 == n => "end",
                _ => "middle",
            };
            out.push(Some(Lyric { text: p.to_string(), syllabic: syllabic.into(), verse: 1 }));
        }
    }
    out
}

/// Attach one syllable per melody note (voice 1, notes that are not tie continuations).
pub fn apply_lyrics(staff: &mut Staff, text: &str) {
    let syl = syllables(text);
    let mut it = syl.into_iter();
    'outer: for m in staff.measures.iter_mut() {
        if let Some(v) = m.voices.iter_mut().find(|v| v.number == 1) {
            for e in v.events.iter_mut() {
                if e.is_rest() || e.notes.iter().all(|n| n.tie_stop) {
                    continue;
                }
                match it.next() {
                    Some(Some(l)) => e.lyrics = vec![l],
                    Some(None) => {}
                    None => break 'outer,
                }
            }
        }
    }
}

/// Measure duration expected for a time signature.
pub fn measure_len(t: TimeSig, div: u32) -> u32 {
    t.measure_divisions(div)
}
