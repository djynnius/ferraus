//! Audio → notation: run `separate`, `transcribe` per stem, a native onset-based drum
//! transcriber on the drum stem, `analyze tempo` / `analyze key` / `analyze chords`, then
//! the MIDI → notation path. The other ops are called by name at run time.

use crate::midi::{MidiData, MidiNote, MidiTrack};
use crate::notate::Layout;
use ferraus_core::exec::run_nested;
use ferraus_core::{AudioBuffer, OpCtx, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const PPQ: u32 = 480;

/// One transcribed stem.
pub struct Stem {
    pub role: String,
    pub path: PathBuf,
}

/// Roles to transcribe for a `--parts` value.
pub fn parts_list(parts: &str) -> Vec<String> {
    if parts == "auto" || parts.is_empty() {
        return ["vocals", "piano", "guitar", "bass", "drums"].iter().map(|s| s.to_string()).collect();
    }
    parts.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect()
}

pub fn layout_for_role(role: &str) -> Layout {
    match role {
        "drums" => Layout::Drums,
        "piano" | "keys" => Layout::Grand,
        "bass" => Layout::Bass,
        "guitar" => Layout::Treble,
        _ => Layout::LeadSheet,
    }
}

pub fn program_for_role(role: &str) -> u8 {
    match role {
        "piano" | "keys" => 0,
        "guitar" => 25,
        "bass" => 33,
        "vocals" => 52,
        "strings" => 48,
        _ => 0,
    }
}

// ------------------------------------------------------------------------------------
// Parsing other ops' JSON results
// ------------------------------------------------------------------------------------

fn num_list(v: Option<&Value>) -> Vec<f64> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_f64().or_else(|| x.get("time").and_then(|t| t.as_f64())).or_else(|| x.get("t").and_then(|t| t.as_f64())))
            .collect(),
        _ => Vec::new(),
    }
}

/// Beat grid from `analyze tempo`.
#[derive(Clone, Debug, Default)]
pub struct BeatGrid {
    pub bpm: f64,
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub time: Option<(u32, u32)>,
}

pub fn parse_tempo(v: &Value) -> BeatGrid {
    let bpm = v.get("bpm").or_else(|| v.get("tempo")).and_then(|x| x.as_f64()).unwrap_or(120.0);
    let beats = num_list(v.get("beats"));
    let downbeats = num_list(v.get("downbeats"));
    let ts = v.get("time_signature").or_else(|| v.get("meter")).or_else(|| v.get("time"));
    let time = match ts {
        Some(Value::String(s)) => s.split_once('/').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?))),
        Some(Value::Object(o)) => {
            let n = o.get("numerator").or_else(|| o.get("beats")).and_then(|x| x.as_u64());
            let d = o.get("denominator").or_else(|| o.get("beat_type")).and_then(|x| x.as_u64());
            n.map(|n| (n as u32, d.unwrap_or(4) as u32))
        }
        _ => v.get("beats_per_bar").and_then(|x| x.as_u64()).map(|n| (n as u32, 4)),
    };
    BeatGrid { bpm: if bpm > 0.0 { bpm } else { 120.0 }, beats, downbeats, time }
}

pub fn parse_key_result(v: &Value) -> Option<crate::model::KeySig> {
    let key = v.get("key").and_then(|k| k.as_str())?;
    let mode = v.get("mode").and_then(|m| m.as_str()).unwrap_or("");
    crate::pitch::parse_key(&format!("{key} {mode}").replace("  ", " ")).or_else(|| crate::pitch::parse_key(key))
}

/// Chord labels (`C`, `Am7`, `C:maj`, `A:min7`, `G:7/B`) → (start s, end s, symbol).
pub fn parse_chords(v: &Value) -> Vec<(f64, f64, String)> {
    let arr = match v {
        Value::Array(a) => a.clone(),
        _ => v.get("chords").or_else(|| v.get("segments")).and_then(|c| c.as_array()).cloned().unwrap_or_default(),
    };
    let mut out = Vec::new();
    for c in arr {
        let start = c.get("start").or_else(|| c.get("time")).and_then(|x| x.as_f64());
        let end = c.get("end").and_then(|x| x.as_f64());
        let label = c.get("label").or_else(|| c.get("chord")).or_else(|| c.get("symbol")).and_then(|x| x.as_str());
        if let (Some(s), Some(l)) = (start, label) {
            if let Some(sym) = normalize_label(l) {
                out.push((s, end.unwrap_or(s), sym));
            }
        }
    }
    // fill missing ends from the next start
    for i in 0..out.len() {
        if out[i].1 <= out[i].0 {
            out[i].1 = out.get(i + 1).map(|n| n.0).unwrap_or(out[i].0 + 2.0);
        }
    }
    out
}

fn normalize_label(l: &str) -> Option<String> {
    let l = l.trim();
    if l.is_empty() || l == "N" || l == "X" {
        return None;
    }
    if let Some((root, q)) = l.split_once(':') {
        let (q, bass) = match q.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (q, None),
        };
        let suffix = match q {
            "maj" => "",
            "min" => "m",
            "7" => "7",
            "maj7" => "maj7",
            "min7" => "m7",
            "dim" => "dim",
            "dim7" => "dim7",
            "hdim7" => "m7b5",
            "aug" => "aug",
            "sus2" => "sus2",
            "sus4" => "sus4",
            "maj6" => "6",
            "min6" => "m6",
            "9" => "9",
            "add9" => "add9",
            _ => "",
        };
        let mut s = format!("{root}{suffix}");
        if let Some(b) = bass {
            if b.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                s.push('/');
                s.push_str(b);
            }
        }
        return Some(s);
    }
    Some(l.to_string())
}

/// Seconds → beat position through the beat grid (linear between beats, extrapolated
/// with the edge intervals or the tempo).
pub fn sec_to_beats(grid: &BeatGrid, t: f64) -> f64 {
    let b = &grid.beats;
    let spb = 60.0 / grid.bpm;
    if b.len() < 2 {
        return t / spb;
    }
    if t <= b[0] {
        return (t - b[0]) / (b[1] - b[0]).max(1e-3);
    }
    let n = b.len();
    if t >= b[n - 1] {
        return (n - 1) as f64 + (t - b[n - 1]) / (b[n - 1] - b[n - 2]).max(1e-3);
    }
    let i = b.partition_point(|x| *x <= t) - 1;
    i as f64 + (t - b[i]) / (b[i + 1] - b[i]).max(1e-6)
}

/// Offset (in beats) that puts the first downbeat on a bar line.
pub fn downbeat_offset(grid: &BeatGrid, beats_per_bar: u32) -> f64 {
    match grid.downbeats.first() {
        Some(d) => {
            let db = sec_to_beats(grid, *d).round();
            let bpb = beats_per_bar.max(1) as f64;
            // shift so that db lands on a multiple of the bar
            (bpb - db.rem_euclid(bpb)).rem_euclid(bpb)
        }
        None => 0.0,
    }
}

// ------------------------------------------------------------------------------------
// Native drum transcription
// ------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn new(kind: &str, f: f64, sr: f64, q: f64) -> Biquad {
        let w = 2.0 * std::f64::consts::PI * (f / sr).min(0.49);
        let (s, c) = (w.sin(), w.cos());
        let alpha = s / (2.0 * q);
        let (b0, b1, b2) = match kind {
            "lp" => ((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0),
            "hp" => ((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0),
            _ => (alpha, 0.0, -alpha), // band-pass (constant peak gain)
        };
        let a0 = 1.0 + alpha;
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: -2.0 * c / a0, a2: (1.0 - alpha) / a0, z1: 0.0, z2: 0.0 }
    }
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

fn band(x: &[f32], sr: f64, filters: &[(&str, f64)]) -> Vec<f64> {
    let mut fs: Vec<Biquad> = filters.iter().map(|(k, f)| Biquad::new(k, *f, sr, std::f64::consts::FRAC_1_SQRT_2)).collect();
    x.iter()
        .map(|s| {
            let mut v = *s as f64;
            for f in fs.iter_mut() {
                v = f.run(v);
            }
            v
        })
        .collect()
}

/// Frame energies (hop 5 ms, window 20 ms).
fn energy(x: &[f64], sr: f64) -> (Vec<f64>, f64) {
    let hop = (sr * 0.005).round().max(1.0) as usize;
    let win = hop * 4;
    let mut out = Vec::new();
    let mut i = 0;
    while i < x.len() {
        let end = (i + win).min(x.len());
        let e: f64 = x[i..end].iter().map(|v| v * v).sum::<f64>() / win as f64;
        out.push(e);
        i += hop;
    }
    (out, hop as f64 / sr)
}

/// Onset times (s) and strengths in one band.
fn onsets(env: &[f64], hop_s: f64, rel_floor_db: f64, min_gap_s: f64) -> Vec<(f64, f64)> {
    if env.is_empty() {
        return Vec::new();
    }
    let maxe = env.iter().cloned().fold(0.0, f64::max).max(1e-12);
    let floor = maxe * 10f64.powf(rel_floor_db / 10.0);
    let log: Vec<f64> = env.iter().map(|e| (e.max(floor * 1e-3) / maxe).log10() * 10.0).collect();
    let flux: Vec<f64> = (0..log.len()).map(|i| if i == 0 { 0.0 } else { (log[i] - log[i - 1]).max(0.0) }).collect();
    // look ahead a few frames: the energy should rise by at least 6 dB over 15 ms
    let gap = (min_gap_s / hop_s).round() as usize;
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut last: Option<usize> = None;
    for i in 1..log.len() {
        let rise: f64 = (0..3).filter_map(|k| flux.get(i + k)).sum();
        let prev_rise: f64 = (0..3).filter_map(|k| flux.get(i - 1 + k)).sum();
        let is_start = rise >= 6.0 && flux[i] > 0.5 && prev_rise < rise + 1e-9 && (i < 2 || flux[i - 1] < flux[i] || flux[i - 1] < 0.5);
        if !is_start {
            continue;
        }
        let peak = (i..(i + 6).min(env.len())).map(|k| env[k]).fold(0.0, f64::max);
        if peak < floor {
            continue;
        }
        if let Some(l) = last {
            if i - l < gap {
                continue;
            }
        }
        out.push((i as f64 * hop_s, peak));
        last = Some(i);
    }
    out
}

/// Kick / snare / hi-hat hits (time s, GM key, velocity) from a drum stem.
pub fn transcribe_drums(buf: &AudioBuffer) -> Vec<(f64, u8, u8)> {
    let x = buf.mono_mix();
    let sr = buf.sample_rate as f64;
    let low = band(&x, sr, &[("lp", 150.0), ("lp", 150.0)]);
    let mid = band(&x, sr, &[("hp", 250.0), ("lp", 5000.0)]);
    let high = band(&x, sr, &[("hp", 7000.0), ("hp", 7000.0)]);
    let (el, hop) = energy(&low, sr);
    let (em, _) = energy(&mid, sr);
    let (eh, _) = energy(&high, sr);
    let kicks = onsets(&el, hop, -35.0, 0.06);
    let snares = onsets(&em, hop, -30.0, 0.06);
    let hats = onsets(&eh, hop, -35.0, 0.04);
    let maxl = el.iter().cloned().fold(1e-12, f64::max);
    let maxm = em.iter().cloned().fold(1e-12, f64::max);
    let maxh = eh.iter().cloned().fold(1e-12, f64::max);
    let vel = |e: f64, m: f64| -> u8 { (40.0 + 87.0 * (e / m).sqrt()).clamp(1.0, 127.0) as u8 };
    let frame = |t: f64| (t / hop).round() as usize;
    let mut out = Vec::new();
    for (t, e) in &kicks {
        // snare bodies leak into the low band: a kick must carry a large share of the
        // low band's peak energy compared with the mid band at the same moment
        let f = frame(*t);
        let lowe = (f..(f + 6).min(el.len())).map(|k| el[k] / maxl).fold(0.0, f64::max);
        let mide = (f..(f + 6).min(em.len())).map(|k| em[k] / maxm).fold(0.0, f64::max);
        if lowe < 0.1 && lowe < mide * 0.5 {
            continue;
        }
        out.push((*t, 36u8, vel(*e, maxl)));
    }
    for (t, e) in &snares {
        // a kick's click leaks into the mid band: keep only snares whose mid energy is
        // strong relative to the low band at that moment
        let f = frame(*t);
        let lowe = (f..(f + 6).min(el.len())).map(|k| el[k] / maxl).fold(0.0, f64::max);
        let mide = (f..(f + 6).min(em.len())).map(|k| em[k] / maxm).fold(0.0, f64::max);
        let hie = (f..(f + 6).min(eh.len())).map(|k| eh[k] / maxh).fold(0.0, f64::max);
        let near_kick = kicks.iter().any(|(kt, _)| (kt - t).abs() < 0.03);
        if near_kick && mide < lowe * 0.5 {
            continue;
        }
        // hi-hat noise leaks into the mid band: require mid-band dominance
        if mide < 0.1 && mide < hie * 0.5 {
            continue;
        }
        out.push((*t, 38u8, vel(*e, maxm)));
    }
    for (t, e) in &hats {
        let f = frame(*t);
        let mide = (f..(f + 4).min(em.len())).map(|k| em[k] / maxm).fold(0.0, f64::max);
        let hie = (f..(f + 4).min(eh.len())).map(|k| eh[k] / maxh).fold(0.0, f64::max);
        let near_snare = out.iter().any(|(st, k, _)| *k == 38 && (st - t).abs() < 0.03);
        // snare wires leak into the top band; drop weak coincident hats
        if near_snare && hie < mide * 0.3 {
            continue;
        }
        out.push((*t, 42u8, vel(*e, maxh)));
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
    out
}

// ------------------------------------------------------------------------------------
// Orchestration
// ------------------------------------------------------------------------------------

/// Result of the audio front end: a MIDI file ready for notation, plus metadata.
pub struct AudioFront {
    pub midi: MidiData,
    pub key: Option<crate::model::KeySig>,
    pub time: Option<(u32, u32)>,
    pub chords: Vec<(f64, f64, String)>,
    pub layouts: Vec<(usize, Layout)>,
    pub stems: Vec<Value>,
    pub bpm: f64,
    pub note_confidence: Vec<f32>,
}

fn tmp_dir() -> Result<PathBuf> {
    let d = std::env::temp_dir().join(format!(
        "ferraus-score-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Run the audio pipeline. Errors from the composed ops (missing models: exit 5, missing
/// ops) are returned unchanged.
pub fn front(ctx: &mut OpCtx, audio: &Path, parts: &str) -> Result<AudioFront> {
    let roles = parts_list(parts);
    let tmp = tmp_dir()?;
    let res = front_inner(ctx, audio, &roles, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

fn front_inner(ctx: &mut OpCtx, audio: &Path, roles: &[String], tmp: &Path) -> Result<AudioFront> {
    // 1. tempo, meter, beats
    let (tv, _) = run_nested(ctx, "analyze tempo", &[], vec![audio.to_path_buf()], None)?;
    let grid = parse_tempo(&tv);
    let time = grid.time;
    // 2. key
    let key = match run_nested(ctx, "analyze key", &[], vec![audio.to_path_buf()], None) {
        Ok((kv, _)) => parse_key_result(&kv),
        Err(e) => return Err(e),
    };
    // 3. chords (best-effort; a missing chords op is not fatal)
    let chords = match run_nested(ctx, "analyze chords", &[], vec![audio.to_path_buf()], None) {
        Ok((cv, _)) => parse_chords(&cv),
        Err(e) if e.kind == ferraus_core::ErrorKind::ModelNotCached => return Err(e),
        Err(e) => {
            ctx.warn("chords_unavailable", format!("analyze chords failed: {}", e.message));
            Vec::new()
        }
    };
    // 4. separation
    let need6 = roles.iter().any(|r| r == "piano" || r == "guitar");
    let stems_arg = if need6 { "6" } else { "4" };
    let template = tmp.join("stem.{stem}.wav");
    let (_, outs) = run_nested(ctx, "separate", &[("stems", stems_arg)], vec![audio.to_path_buf()], Some(&template))?;
    let mut stems: Vec<Stem> = Vec::new();
    for o in &outs {
        if let Some(role) = &o.role {
            stems.push(Stem { role: role.clone(), path: PathBuf::from(&o.path) });
        }
    }
    let bpb = time.map(|t| t.0).unwrap_or(4);
    let offset = downbeat_offset(&grid, bpb);
    let to_tick = |t: f64| -> u64 { ((sec_to_beats(&grid, t) + offset).max(0.0) * PPQ as f64).round() as u64 };
    let mut midi = MidiData { ppq: PPQ, ..MidiData::default() };
    midi.tempos.push((0, (60_000_000.0 / grid.bpm).round() as u32));
    if let Some((n, d)) = time {
        midi.time_sigs.push((0, n, d));
    }
    let mut layouts = Vec::new();
    let mut stem_info = Vec::new();
    let mut confidences: Vec<f32> = Vec::new();
    for role in roles {
        let Some(stem) = stems.iter().find(|s| &s.role == role) else {
            ctx.warn("stem_missing", format!("separate produced no '{role}' stem"));
            continue;
        };
        let ti = midi.tracks.len();
        if role == "drums" {
            let buf = ctx.read_audio_path(&stem.path)?;
            let hits = transcribe_drums(&buf);
            for (t, k, v) in &hits {
                let s = to_tick(*t);
                midi.notes.push(MidiNote { track: ti, channel: 9, key: *k, velocity: *v, start: s, end: s + PPQ as u64 / 8 });
                confidences.push(0.8);
            }
            midi.tracks.push(MidiTrack { name: Some("Drums".into()), program: None });
            layouts.push((ti, Layout::Drums));
            stem_info.push(serde_json::json!({"role": role, "notes": hits.len(), "method": "native onset drum transcription"}));
            continue;
        }
        let mode = match role.as_str() {
            "vocals" | "bass" => "mono",
            _ => "poly",
        };
        let out = tmp.join(format!("{role}.mid"));
        let (tres, touts) = run_nested(ctx, "transcribe", &[("mode", mode)], vec![stem.path.clone()], Some(&out))?;
        let mid_path = touts.iter().find(|o| o.path.ends_with(".mid")).map(|o| PathBuf::from(&o.path)).unwrap_or(out.clone());
        let bytes = std::fs::read(&mid_path)?;
        let m = crate::midi::read_midi(&bytes)?;
        let conf_list = transcribe_confidences(&tres);
        let ch = (ti as u8).min(8);
        let mut count = 0;
        for n in &m.notes {
            let s_sec = m.tick_to_seconds(n.start);
            let e_sec = m.tick_to_seconds(n.end);
            let s = to_tick(s_sec);
            let e = to_tick(e_sec).max(s + 1);
            midi.notes.push(MidiNote { track: ti, channel: ch, key: n.key, velocity: n.velocity, start: s, end: e });
            confidences.push(match_conf(&conf_list, s_sec, n.key));
            count += 1;
        }
        midi.tracks.push(MidiTrack { name: Some(role_name(role)), program: Some((ch, program_for_role(role))) });
        layouts.push((ti, layout_for_role(role)));
        stem_info.push(serde_json::json!({"role": role, "notes": count, "mode": mode}));
    }
    // notate() reads confidences aligned with midi.notes in their stored order; sort both
    let mut idx: Vec<usize> = (0..midi.notes.len()).collect();
    idx.sort_by_key(|i| (midi.notes[*i].start, midi.notes[*i].track, midi.notes[*i].key));
    let notes: Vec<MidiNote> = idx.iter().map(|i| midi.notes[*i].clone()).collect();
    let confs: Vec<f32> = idx.iter().map(|i| confidences.get(*i).copied().unwrap_or(1.0)).collect();
    midi.notes = notes;
    // chords: seconds → ticks
    let chords_t: Vec<(f64, f64, String)> = chords
        .iter()
        .map(|(s, e, l)| (to_tick(*s) as f64, to_tick(*e) as f64, l.clone()))
        .collect();
    Ok(AudioFront { midi, key, time, chords: chords_t, layouts, stems: stem_info, bpm: grid.bpm, note_confidence: confs })
}

fn role_name(role: &str) -> String {
    let mut c = role.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn transcribe_confidences(v: &Value) -> Vec<(f64, u8, f32)> {
    let notes = v.get("notes").and_then(|n| n.as_array()).cloned().unwrap_or_default();
    notes
        .iter()
        .filter_map(|n| {
            let t = n.get("start").or_else(|| n.get("onset_s")).or_else(|| n.get("start_s")).and_then(|x| x.as_f64())?;
            let p = n.get("pitch").or_else(|| n.get("midi")).or_else(|| n.get("key")).and_then(|x| x.as_u64())? as u8;
            let c = n.get("confidence").and_then(|x| x.as_f64())? as f32;
            Some((t, p, c))
        })
        .collect()
}

fn match_conf(list: &[(f64, u8, f32)], t: f64, key: u8) -> f32 {
    list.iter()
        .filter(|(lt, k, _)| *k == key && (lt - t).abs() < 0.05)
        .map(|x| x.2)
        .next()
        .unwrap_or(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_drums(sr: u32, hits: &[(f64, u8)], secs: f64) -> AudioBuffer {
        let n = (sr as f64 * secs) as usize;
        let mut x = vec![0f32; n];
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed as f64 / u64::MAX as f64) * 2.0 - 1.0
        };
        let mut hp = Biquad::new("hp", 8000.0, sr as f64, 0.7);
        for (t, k) in hits {
            let s0 = (t * sr as f64) as usize;
            match k {
                36 => {
                    for i in 0..(sr as usize / 4) {
                        let tt = i as f64 / sr as f64;
                        let f = 50.0 + 80.0 * (-tt * 30.0).exp();
                        let v = (2.0 * std::f64::consts::PI * f * tt).sin() * (-tt * 12.0).exp() * 0.9;
                        if s0 + i < n {
                            x[s0 + i] += v as f32;
                        }
                    }
                }
                38 => {
                    for i in 0..(sr as usize / 6) {
                        let tt = i as f64 / sr as f64;
                        let v = (rnd() * 0.5 + 0.3 * (2.0 * std::f64::consts::PI * 200.0 * tt).sin()) * (-tt * 25.0).exp();
                        if s0 + i < n {
                            x[s0 + i] += v as f32;
                        }
                    }
                }
                _ => {
                    for i in 0..(sr as usize / 20) {
                        let tt = i as f64 / sr as f64;
                        let v = hp.run(rnd()) * (-tt * 80.0).exp() * 0.6;
                        if s0 + i < n {
                            x[s0 + i] += v as f32;
                        }
                    }
                }
            }
        }
        AudioBuffer::from_mono(sr, x)
    }

    #[test]
    fn drum_transcription_f1() {
        // 2 bars of a rock beat at 100 bpm: kick 1 & 3, snare 2 & 4, hi-hat eighths
        let beat = 0.6;
        let mut hits = Vec::new();
        for bar in 0..2 {
            let b0 = 0.2 + bar as f64 * 4.0 * beat;
            for k in 0..8 {
                hits.push((b0 + k as f64 * beat / 2.0, 42u8));
            }
            hits.push((b0, 36));
            hits.push((b0 + 2.0 * beat, 36));
            hits.push((b0 + beat, 38));
            hits.push((b0 + 3.0 * beat, 38));
        }
        let buf = synth_drums(22050, &hits, 5.4);
        let got = transcribe_drums(&buf);
        for key in [36u8, 38, 42] {
            let refs: Vec<f64> = hits.iter().filter(|h| h.1 == key).map(|h| h.0).collect();
            let est: Vec<f64> = got.iter().filter(|h| h.1 == key).map(|h| h.0).collect();
            let mut used = vec![false; est.len()];
            let mut tp = 0;
            for r in &refs {
                if let Some(i) = est.iter().enumerate().position(|(i, e)| !used[i] && (e - r).abs() <= 0.05) {
                    used[i] = true;
                    tp += 1;
                }
            }
            let p = tp as f64 / est.len().max(1) as f64;
            let r = tp as f64 / refs.len() as f64;
            let f1 = if p + r > 0.0 { 2.0 * p * r / (p + r) } else { 0.0 };
            assert!(f1 >= 0.9, "key {key}: F1 {f1:.2} (est {est:?})");
        }
    }

    #[test]
    fn beat_mapping_and_parsing() {
        let g = parse_tempo(&serde_json::json!({"bpm": 120.0, "beats": [0.5, 1.0, 1.5, 2.0], "downbeats": [1.0], "time_signature": "3/4"}));
        assert_eq!(g.time, Some((3, 4)));
        assert!((sec_to_beats(&g, 1.25) - 1.5).abs() < 1e-9);
        // downbeat at beat 1 → shift by 2 so it lands on a bar line in 3/4
        assert_eq!(downbeat_offset(&g, 3), 2.0);
        let c = parse_chords(&serde_json::json!({"chords": [{"start": 0.0, "end": 2.0, "label": "A:min7"}, {"start": 2.0, "label": "G:maj/B"}, {"start": 4.0, "label": "N"}]}));
        assert_eq!(c[0].2, "Am7");
        assert_eq!(c[1].2, "G/B");
        assert_eq!(c.len(), 2);
        let k = parse_key_result(&serde_json::json!({"key": "E", "mode": "minor"}));
        assert_eq!(k, Some(crate::model::KeySig::minor(1)));
    }
}
