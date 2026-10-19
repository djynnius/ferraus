//! Notation → MIDI performance (`score play`): tempo, repeats and endings unrolled,
//! dynamics to velocity, articulations to note lengths, swing, humanize, program
//! changes, and an optional click track.

use crate::midi::{MidiData, MidiNote, MidiTrack};
use crate::model::*;
use crate::musicxml::dynamic_velocity;
use crate::pitch::lcm;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct PlayOptions {
    pub tempo: Option<f64>,
    pub repeats: bool,
    pub dynamics: bool,
    pub articulation: bool,
    /// 0..1: timing jitter up to ±20 ms and velocity jitter up to ±12 at 100%.
    pub humanize: f64,
    /// 0..1: off-beat eighths move toward the triplet position (100% = 2:1 swing).
    pub swing: f64,
    /// Part id → GM program.
    pub instrument_map: HashMap<String, u8>,
    pub click: bool,
    pub seed: u64,
}

impl Default for PlayOptions {
    fn default() -> Self {
        PlayOptions {
            tempo: None,
            repeats: true,
            dynamics: true,
            articulation: true,
            humanize: 0.0,
            swing: 0.0,
            instrument_map: HashMap::new(),
            click: false,
            seed: 0,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PlayInfo {
    pub duration_s: f64,
    pub notes: usize,
    pub measures_played: usize,
    pub order: Vec<u32>,
    pub tempo_bpm: f64,
    pub tracks: usize,
}

/// Measure indices in playback order.
pub fn playback_order(measures: &[Measure], repeats: bool) -> Vec<usize> {
    let n = measures.len();
    if !repeats {
        return (0..n).collect();
    }
    let mut order = Vec::new();
    let mut i = 0usize;
    let mut start = 0usize;
    let mut pass = 1u32;
    let mut guard = 0usize;
    while i < n && guard < n * 64 + 64 {
        guard += 1;
        let m = &measures[i];
        if m.repeat_start && i != start {
            start = i;
            pass = 1;
        }
        if let Some(e) = &m.ending {
            let nums = e.numbers();
            if !nums.is_empty() && !nums.contains(&pass) {
                i += 1;
                continue;
            }
        }
        order.push(i);
        if let Some(t) = m.repeat_end {
            if pass < t {
                pass += 1;
                i = start;
                continue;
            }
            pass = 1;
            start = i + 1;
        } else if let Some(e) = &m.ending {
            if e.stops() && e.numbers().iter().copied().max().unwrap_or(0) >= pass {
                pass = 1;
                start = i + 1;
            }
        }
        i += 1;
    }
    order
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    fn jitter(&mut self) -> f64 {
        // roughly gaussian in [-1, 1]
        (self.next() + self.next() + self.next()) / 1.5 - 1.0
    }
}

/// Length in divisions of each measure (the longest voice, or the time signature).
fn measure_lengths(score: &Score) -> Vec<u64> {
    let n = score.measure_count();
    let mut lens = vec![0u64; n];
    for p in &score.parts {
        for st in &p.staves {
            for (mi, m) in st.measures.iter().enumerate() {
                for v in &m.voices {
                    let s: u64 = v.events.iter().filter(|e| !e.grace).map(|e| e.duration as u64).sum();
                    lens[mi] = lens[mi].max(s);
                }
            }
        }
    }
    if let Some(st) = score.parts.first().and_then(|p| p.staves.first()) {
        for (mi, l) in lens.iter_mut().enumerate() {
            if *l == 0 && mi < st.measures.len() {
                *l = Score::time_at(st, mi).measure_divisions(score.divisions) as u64;
            }
        }
    }
    lens
}

pub fn perform(score: &Score, opts: &PlayOptions) -> (MidiData, PlayInfo) {
    let div = score.divisions.max(1) as u64;
    let ppq = {
        let l = lcm(div, 480);
        if l <= 32767 {
            l
        } else {
            div.min(32767)
        }
    };
    let tk = |d: f64| -> u64 { (d * ppq as f64 / div as f64).round().max(0.0) as u64 };
    let mut out = MidiData { ppq: ppq as u32, title: score.title.clone(), ..MidiData::default() };
    let lens = measure_lengths(score);
    let ref_staff = score.parts.first().and_then(|p| p.staves.first());
    let order = match ref_staff {
        Some(st) => playback_order(&st.measures, opts.repeats),
        None => Vec::new(),
    };
    // start position (divisions) of each played occurrence
    let mut starts: Vec<u64> = Vec::with_capacity(order.len());
    let mut pos = 0u64;
    for &mi in &order {
        starts.push(pos);
        pos += lens.get(mi).copied().unwrap_or(0);
    }
    let total = pos;
    let mut rng = Rng(opts.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) | 1);

    // tempo, time and key signatures along the played order
    let mut first_bpm = None;
    if let Some(st) = ref_staff {
        let mut last_time: Option<TimeSig> = None;
        let mut last_key: Option<KeySig> = None;
        for (oi, &mi) in order.iter().enumerate() {
            let m = &st.measures[mi];
            let t = Score::time_at(st, mi);
            let len = lens[mi];
            let full = t.measure_divisions(div as u32) as u64;
            // pickup or other short measure: write its real length as a signature
            let shown = if len != full && len > 0 { short_sig(len, div, t) } else { t };
            if last_time != Some(shown) {
                out.time_sigs.push((tk(starts[oi] as f64), shown.beats, shown.beat_type));
                last_time = Some(shown);
            }
            if !score.parts[0].percussion {
                let k = Score::key_at(st, mi);
                if last_key != Some(k) {
                    out.key_sigs.push((tk(starts[oi] as f64), k.fifths, k.mode == Mode::Minor));
                    last_key = Some(k);
                }
            }
            if opts.tempo.is_none() {
                for tm in &m.tempo {
                    if first_bpm.is_none() {
                        first_bpm = Some(tm.bpm);
                    }
                    let at = tk((starts[oi] + tm.offset as u64) as f64);
                    if out.tempos.last().map(|(_, us)| *us) != Some(us_per_q(tm.bpm)) {
                        out.tempos.push((at, us_per_q(tm.bpm)));
                    }
                }
            }
        }
    }
    let bpm = opts.tempo.or(first_bpm).unwrap_or(120.0);
    if opts.tempo.is_some() || out.tempos.is_empty() || out.tempos[0].0 > 0 {
        out.tempos.retain(|_| opts.tempo.is_none());
        out.tempos.insert(0, (0, us_per_q(bpm)));
    }
    out.tempos.dedup_by_key(|t| t.0);

    // channels: percussion on 10, others 1..16 skipping 10
    let mut next_ch = 0u8;
    for (pi, p) in score.parts.iter().enumerate() {
        let ch = if p.percussion {
            9
        } else if let Some(c) = p.channel.filter(|c| *c >= 1 && *c <= 16 && *c != 10) {
            c - 1
        } else {
            let c = next_ch;
            next_ch += 1;
            if next_ch == 9 {
                next_ch += 1;
            }
            c.min(15)
        };
        let program = opts.instrument_map.get(&p.id).copied().or(p.program).unwrap_or(0);
        out.tracks.push(MidiTrack {
            name: Some(p.name.clone()),
            program: if p.percussion { None } else { Some((ch, program)) },
        });
        // seconds-per-division for humanize jitter conversion
        let sec_per_div = 60.0 / bpm / div as f64;
        let mut open: HashMap<(usize, i32), usize> = HashMap::new();
        for st in &p.staves {
            let mut level: u8 = 80;
            for (oi, &mi) in order.iter().enumerate() {
                let Some(m) = st.measures.get(mi) else { continue };
                let mstart = starts[oi];
                let mut dyns: Vec<(u64, u8)> = m
                    .directions
                    .iter()
                    .filter(|d| d.kind == "dynamic")
                    .filter_map(|d| dynamic_velocity(&d.value).map(|v| (d.offset as u64, v)))
                    .collect();
                dyns.sort();
                for v in &m.voices {
                    let mut vpos = 0u64;
                    let mut vlevel = level;
                    let mut slur = false;
                    for e in &v.events {
                        for (o, vel) in &dyns {
                            if *o <= vpos {
                                vlevel = *vel;
                            }
                        }
                        if let Some(d) = e.dynamic.as_deref().and_then(dynamic_velocity) {
                            vlevel = d;
                        }
                        if e.slurs.iter().any(|s| s == "start") {
                            slur = true;
                        }
                        let dur = e.duration as u64;
                        if !e.is_rest() && !e.grace {
                            let onset = (mstart + vpos) as f64;
                            let mut on = onset;
                            let mut len = dur as f64;
                            if opts.articulation {
                                let a = &e.articulations;
                                if a.iter().any(|x| x == "staccatissimo") {
                                    len *= 0.25;
                                } else if a.iter().any(|x| x == "staccato") {
                                    len *= 0.5;
                                } else if a.iter().any(|x| x == "tenuto" || x == "legato") || slur {
                                    // full length
                                } else {
                                    // plain notes: a small articulation gap (5%, at most a 64th note)
                                    len -= (len * 0.05).min(div as f64 / 16.0);
                                }
                                if a.iter().any(|x| x == "fermata") {
                                    len *= 1.5;
                                }
                            }
                            if opts.swing > 0.0 {
                                let rel = (vpos) % div;
                                if rel == div / 2 {
                                    on += opts.swing * div as f64 / 6.0;
                                    len -= opts.swing * div as f64 / 6.0;
                                }
                            }
                            let mut accent = 0i32;
                            if opts.articulation && e.articulations.iter().any(|x| x == "accent" || x == "marcato") {
                                accent = 15;
                            }
                            for n in &e.notes {
                                let mut vel = if opts.dynamics && score_has_dynamics(score) {
                                    vlevel as i32
                                } else {
                                    n.velocity.map(|v| v as i32).unwrap_or(vlevel as i32)
                                };
                                vel += accent;
                                let mut start_d = on;
                                if opts.humanize > 0.0 {
                                    start_d += rng.jitter() * opts.humanize * 0.020 / sec_per_div;
                                    vel += (rng.jitter() * opts.humanize * 12.0).round() as i32;
                                }
                                let vel = vel.clamp(1, 127) as u8;
                                let key = n.midi.clamp(0, 127);
                                let start_t = tk(start_d.max(0.0));
                                let end_t = tk((start_d + len).max(start_d + 1.0)).max(start_t + 1);
                                if n.tie_stop {
                                    if let Some(&ix) = open.get(&(st.number as usize, key)) {
                                        // extend the tied note
                                        let full_end = tk((mstart + vpos) as f64 + len);
                                        out.notes[ix].end = full_end.max(out.notes[ix].end);
                                        if !n.tie_start {
                                            open.remove(&(st.number as usize, key));
                                        }
                                        continue;
                                    }
                                }
                                out.notes.push(MidiNote {
                                    track: pi,
                                    channel: ch,
                                    key: key as u8,
                                    velocity: vel,
                                    start: start_t,
                                    end: end_t,
                                });
                                if n.tie_start {
                                    open.insert((st.number as usize, key), out.notes.len() - 1);
                                }
                            }
                        }
                        if e.slurs.iter().any(|s| s == "stop") {
                            slur = false;
                        }
                        vpos += if e.grace { 0 } else { dur };
                    }
                    if v.number == 1 {
                        level = vlevel;
                    }
                }
            }
        }
        // lyrics of the first staff, voice 1
        if let Some(st) = p.staves.first() {
            for (oi, &mi) in order.iter().enumerate() {
                let Some(m) = st.measures.get(mi) else { continue };
                if let Some(v) = m.voices.iter().find(|v| v.number == 1) {
                    let mut vpos = 0u64;
                    for e in &v.events {
                        if let Some(l) = e.lyrics.first() {
                            let txt = match l.syllabic.as_str() {
                                "begin" | "middle" => format!("{}-", l.text),
                                _ => l.text.clone(),
                            };
                            out.lyrics.push((tk((starts[oi] + vpos) as f64), pi, txt));
                        }
                        vpos += e.duration as u64;
                    }
                }
            }
        }
    }
    if opts.click {
        let ti = out.tracks.len();
        out.tracks.push(MidiTrack { name: Some("Click".into()), program: None });
        if let Some(st) = ref_staff {
            for (oi, &mi) in order.iter().enumerate() {
                let t = Score::time_at(st, mi);
                let beat = t.beat_divisions(div as u32) as u64;
                let len = lens[mi];
                let full = t.measure_divisions(div as u32) as u64;
                // align beats to the end of a pickup
                let offset = if len < full { (full - len) % beat } else { 0 };
                let mut b = (beat - offset) % beat;
                let mut first = len == full;
                while b < len {
                    let s = tk((starts[oi] + b) as f64);
                    out.notes.push(MidiNote {
                        track: ti,
                        channel: 9,
                        key: if first { 76 } else { 77 },
                        velocity: if first { 110 } else { 80 },
                        start: s,
                        end: s + ppq / 8,
                    });
                    first = false;
                    b += beat;
                }
            }
        }
    }
    out.notes.sort_by_key(|n| (n.start, n.track, n.key));
    out.end_tick = tk(total as f64);
    let info = PlayInfo {
        duration_s: out.tick_to_seconds(out.end_tick.max(out.notes.iter().map(|n| n.end).max().unwrap_or(0))),
        notes: out.notes.len(),
        measures_played: order.len(),
        order: order
            .iter()
            .map(|i| ref_staff.and_then(|s| s.measures.get(*i)).map(|m| m.number).unwrap_or(0))
            .collect(),
        tempo_bpm: bpm,
        tracks: out.tracks.len(),
    };
    (out, info)
}

fn score_has_dynamics(score: &Score) -> bool {
    score.parts.iter().any(|p| {
        p.staves.iter().any(|s| {
            s.measures.iter().any(|m| {
                m.directions.iter().any(|d| d.kind == "dynamic")
                    || m.voices.iter().any(|v| v.events.iter().any(|e| e.dynamic.is_some()))
            })
        })
    })
}

fn us_per_q(bpm: f64) -> u32 {
    (60_000_000.0 / bpm.max(1.0)).round() as u32
}

/// A time signature that spells a short measure's length (`1/4` for a one-beat pickup).
fn short_sig(len: u64, div: u64, t: TimeSig) -> TimeSig {
    for den in [t.beat_type, 4, 8, 16, 32] {
        let unit = div * 4 / den as u64;
        if unit > 0 && len % unit == 0 {
            return TimeSig::new((len / unit) as u32, den);
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(rs: bool, re: Option<u32>, ending: Option<(&str, &str)>) -> Measure {
        Measure {
            repeat_start: rs,
            repeat_end: re,
            ending: ending.map(|(n, k)| Ending { number: n.into(), kind: k.into() }),
            ..Measure::default()
        }
    }

    #[test]
    fn repeats_and_voltas() {
        // |: 0 1 [1. 2 :| [2. 3 ] 4
        let ms = vec![
            m(true, None, None),
            m(false, None, None),
            m(false, Some(2), Some(("1", "start-stop"))),
            m(false, None, Some(("2", "start-discontinue"))),
            m(false, None, None),
        ];
        assert_eq!(playback_order(&ms, true), vec![0, 1, 2, 0, 1, 3, 4]);
        assert_eq!(playback_order(&ms, false), vec![0, 1, 2, 3, 4]);
        // plain repeat without forward sign, played 3 times
        let ms = vec![m(false, None, None), m(false, Some(3), None), m(false, None, None)];
        assert_eq!(playback_order(&ms, true), vec![0, 1, 0, 1, 0, 1, 2]);
    }
}
