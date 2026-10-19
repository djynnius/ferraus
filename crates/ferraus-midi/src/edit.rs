//! MIDI editing algorithms (pure functions on [`MidiFile`]), used by the ops and reusable
//! by other crates (transcribe, hum, score).

use crate::model::{Controller, MidiFile, Note, Program, Tempo, Timed, Track};
use crate::smf::Timing;
use crate::tempo::{bpm_to_us, us_to_bpm};
use ferraus_core::units::Grid;
use ferraus_core::{Error, Result};
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which part of each note `quantize` moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuantizeWhat {
    /// Snap note-ons; note-offs move with them (durations kept).
    Start,
    /// Snap note-offs only.
    End,
    /// Snap note-ons and note-offs independently.
    Both,
    /// Snap note-ons and round durations to whole grid steps.
    Duration,
}

impl QuantizeWhat {
    pub fn parse(s: &str) -> Result<QuantizeWhat> {
        Ok(match s {
            "start" => QuantizeWhat::Start,
            "end" => QuantizeWhat::End,
            "both" => QuantizeWhat::Both,
            "duration" => QuantizeWhat::Duration,
            o => return Err(Error::usage(format!("--what must be start, end, both, or duration, not '{o}'"))),
        })
    }
}

#[derive(Clone, Debug)]
pub struct QuantizeOptions {
    /// `None` detects the dominant subdivision.
    pub grid: Option<Grid>,
    /// 0..=1
    pub strength: f64,
    /// 0..=1, fraction of a grid step that every second grid point is delayed by.
    pub swing: f64,
    pub what: QuantizeWhat,
    /// Standard deviation of the Gaussian jitter added after quantizing, in seconds.
    pub humanize_s: f64,
    pub min_length: Option<Grid>,
    pub no_overlap: bool,
    pub groove: Option<Groove>,
    pub seed: u64,
    /// 0-based track indices to process; `None` = all.
    pub tracks: Option<Vec<usize>>,
}

impl Default for QuantizeOptions {
    fn default() -> Self {
        QuantizeOptions {
            grid: Some(Grid {
                num: 1,
                den: 16,
                triplet: false,
                dotted: false,
            }),
            strength: 1.0,
            swing: 0.0,
            what: QuantizeWhat::Start,
            humanize_s: 0.0,
            min_length: Some(Grid {
                num: 1,
                den: 64,
                triplet: false,
                dotted: false,
            }),
            no_overlap: true,
            groove: None,
            seed: 0,
            tracks: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct QuantizeReport {
    pub grid: String,
    pub grid_detected: bool,
    pub notes: usize,
    pub notes_moved: usize,
    pub mean_shift_ticks: f64,
    pub max_shift_ticks: f64,
    pub notes_removed_as_duplicates: usize,
}

/// Per-grid-position timing offsets and velocity deltas, captured from a reference part.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Groove {
    #[serde(default = "groove_version")]
    pub ferraus_groove: u32,
    /// Grid the offsets refer to (`1/16`).
    pub grid: String,
    /// Grid positions per cycle (one bar).
    pub steps: usize,
    pub positions: Vec<GrooveStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

fn groove_version() -> u32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GrooveStep {
    pub step: usize,
    /// Timing offset as a fraction of one grid step (+ late, - early).
    pub offset: f64,
    /// Velocity added to notes on this step.
    pub velocity_delta: f64,
    /// Number of reference notes that contributed.
    #[serde(default)]
    pub count: usize,
}

impl Groove {
    pub fn grid(&self) -> Result<Grid> {
        Grid::parse(&self.grid)
    }
    fn offset(&self, k: i64) -> f64 {
        let n = self.steps.max(1) as i64;
        let p = k.rem_euclid(n) as usize;
        self.positions
            .iter()
            .find(|s| s.step == p)
            .map(|s| s.offset)
            .unwrap_or(0.0)
    }
    fn velocity_delta(&self, k: i64) -> f64 {
        let n = self.steps.max(1) as i64;
        let p = k.rem_euclid(n) as usize;
        self.positions
            .iter()
            .find(|s| s.step == p)
            .map(|s| s.velocity_delta)
            .unwrap_or(0.0)
    }
}

fn selected(tracks: &Option<Vec<usize>>, i: usize) -> bool {
    tracks.as_ref().map(|t| t.contains(&i)).unwrap_or(true)
}

/// Candidate grids for `auto`, coarse to fine.
const AUTO_GRIDS: &[(u32, bool)] = &[(4, false), (8, false), (8, true), (16, false), (16, true), (32, false)];

/// Detect the dominant subdivision of a set of onsets: the coarsest grid on which at
/// least 85% of onsets lie within a 1/48-note (or an eighth of the grid step).
pub fn detect_grid(onsets: &[u64], ppq: u16) -> Grid {
    let mk = |den: u32, triplet: bool| Grid {
        num: 1,
        den,
        triplet,
        dotted: false,
    };
    if onsets.is_empty() {
        return mk(16, false);
    }
    for &(den, trip) in AUTO_GRIDS {
        let g = mk(den, trip);
        let step = g.ticks(ppq as u32);
        let tol = (ppq as f64 / 12.0).min(step / 8.0).max(1.0);
        let hits = onsets
            .iter()
            .filter(|&&t| {
                let r = t as f64 / step;
                ((r - r.round()) * step).abs() <= tol
            })
            .count();
        if hits as f64 >= 0.85 * onsets.len() as f64 {
            return g;
        }
    }
    mk(16, false)
}

/// Grid point `k` with swing or groove applied.
fn grid_point(k: i64, step: f64, swing: f64, groove: Option<&Groove>) -> f64 {
    let base = k as f64 * step;
    match groove {
        Some(g) => base + g.offset(k) * step,
        None => {
            if k.rem_euclid(2) == 1 {
                base + swing * step
            } else {
                base
            }
        }
    }
}

/// Nearest (swung/grooved) grid point to `t`: (index, position).
fn nearest(t: f64, step: f64, swing: f64, groove: Option<&Groove>) -> (i64, f64) {
    let k0 = (t / step).floor() as i64;
    let mut best = (k0, grid_point(k0, step, swing, groove));
    for k in (k0 - 2)..=(k0 + 2) {
        if k < 0 {
            continue;
        }
        let p = grid_point(k, step, swing, groove);
        if (p - t).abs() < (best.1 - t).abs() {
            best = (k, p);
        }
    }
    best
}

/// Standard normal sample (Box–Muller).
fn gauss(rng: &mut rand_chacha::ChaCha8Rng) -> f64 {
    let u1: f64 = rng.gen_range(f64::EPSILON..1.0);
    let u2: f64 = rng.gen::<f64>();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Quantize notes in place. See the PRD quantize algorithm.
pub fn quantize(f: &mut MidiFile, o: &QuantizeOptions) -> Result<QuantizeReport> {
    let ppq = match f.timing {
        Timing::Ppq(p) => p,
        _ => return Err(Error::unsupported("quantize needs a PPQ-timed file, not SMPTE timing")),
    };
    let (grid, detected) = match (&o.groove, o.grid) {
        (Some(g), _) => (g.grid()?, false),
        (None, Some(g)) => (g, false),
        (None, None) => {
            let onsets: Vec<u64> = f
                .tracks
                .iter()
                .enumerate()
                .filter(|(i, _)| selected(&o.tracks, *i))
                .flat_map(|(_, t)| t.notes.iter().map(|n| n.tick))
                .collect();
            (detect_grid(&onsets, ppq), true)
        }
    };
    let step = grid.ticks(ppq as u32);
    if step < 1.0 {
        return Err(Error::usage(format!("grid {grid} is finer than one tick at PPQ {ppq}")));
    }
    let s = o.strength.clamp(0.0, 1.0);
    let min_len = o.min_length.map(|g| g.ticks(ppq as u32).round().max(1.0) as u64).unwrap_or(1);
    let map = f.tempo_map();
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(o.seed);
    let groove = o.groove.as_ref();
    let mut moved = 0;
    let mut total_shift = 0.0;
    let mut max_shift: f64 = 0.0;
    let mut count = 0;
    let mut removed = 0;
    for (ti, t) in f.tracks.iter_mut().enumerate() {
        if !selected(&o.tracks, ti) || t.notes.is_empty() {
            continue;
        }
        for n in &mut t.notes {
            count += 1;
            let on = n.tick as f64;
            let off = n.end() as f64;
            let dur = n.duration as f64;
            let (k, p) = nearest(on, step, o.swing, groove);
            let snap_on = on + s * (p - on);
            let (new_on, new_off) = match o.what {
                QuantizeWhat::Start => (snap_on, snap_on + dur),
                QuantizeWhat::End => {
                    let (_, pe) = nearest(off, step, o.swing, groove);
                    (on, off + s * (pe - off))
                }
                QuantizeWhat::Both => {
                    let (_, pe) = nearest(off, step, o.swing, groove);
                    (snap_on, off + s * (pe - off))
                }
                QuantizeWhat::Duration => {
                    let q = ((dur / step).round().max(1.0)) * step;
                    (snap_on, snap_on + dur + s * (q - dur))
                }
            };
            if let Some(g) = groove {
                let dv = g.velocity_delta(k) * s;
                n.velocity = (n.velocity as f64 + dv).round().clamp(1.0, 127.0) as u8;
            }
            let ni = new_on.round().max(0.0) as u64;
            let mut nd = (new_off.round().max(0.0) as u64).saturating_sub(ni);
            if nd < min_len {
                nd = min_len;
            }
            let shift = (ni as f64 - on).abs();
            if ni != n.tick || nd != n.duration {
                moved += 1;
            }
            total_shift += shift;
            max_shift = max_shift.max(shift);
            n.tick = ni;
            n.duration = nd;
        }
        if o.humanize_s > 0.0 {
            for n in &mut t.notes {
                let j = gauss(&mut rng) * o.humanize_s;
                let dt = map.seconds_to_ticks_at(n.tick as f64, j.max(-map.tick_to_seconds(n.tick as f64)));
                n.tick = (n.tick as f64 + dt).round().max(0.0) as u64;
            }
        }
        if o.no_overlap {
            removed += remove_overlaps(&mut t.notes);
        }
        t.notes.sort_by_key(|n| (n.tick, n.pitch, n.channel));
        t.clear_seqs();
    }
    f.fit_ends();
    Ok(QuantizeReport {
        grid: grid.to_string(),
        grid_detected: detected,
        notes: count,
        notes_moved: moved,
        mean_shift_ticks: if count > 0 { total_shift / count as f64 } else { 0.0 },
        max_shift_ticks: max_shift,
        notes_removed_as_duplicates: removed,
    })
}

/// Truncate notes that overlap the next note of the same pitch and channel; drop exact
/// duplicates (same start), keeping the louder/longer one. Returns notes removed.
pub fn remove_overlaps(notes: &mut Vec<Note>) -> usize {
    let mut by: BTreeMap<(u8, u8), Vec<usize>> = BTreeMap::new();
    for (i, n) in notes.iter().enumerate() {
        by.entry((n.channel, n.pitch)).or_default().push(i);
    }
    let mut drop = vec![false; notes.len()];
    for idx in by.values_mut() {
        idx.sort_by_key(|&i| (notes[i].tick, std::cmp::Reverse(notes[i].velocity), std::cmp::Reverse(notes[i].duration)));
        let mut prev: Option<usize> = None;
        for &i in idx.iter() {
            if let Some(p) = prev {
                if notes[p].tick == notes[i].tick {
                    drop[i] = true;
                    continue;
                }
                if notes[p].end() > notes[i].tick {
                    notes[p].duration = notes[i].tick - notes[p].tick;
                }
            }
            prev = Some(i);
        }
    }
    let before = notes.len();
    let mut i = 0;
    notes.retain(|_| {
        let keep = !drop[i];
        i += 1;
        keep
    });
    before - notes.len()
}

/// Capture a groove from a reference file on `grid` (cycle = one bar of the first time signature).
pub fn extract_groove(f: &MidiFile, grid: Grid) -> Result<Groove> {
    let ppq = f
        .ppq()
        .ok_or_else(|| Error::unsupported("groove extraction needs a PPQ-timed file"))?;
    let step = grid.ticks(ppq as u32);
    if step < 1.0 {
        return Err(Error::usage("grid is finer than one tick"));
    }
    let (num, den) = f.time_signature_at(0);
    let bar_beats = num as f64 * 4.0 / den.max(1) as f64;
    let steps = ((bar_beats / grid.beats()).round() as usize).max(1);
    let notes: Vec<&Note> = f.tracks.iter().flat_map(|t| t.notes.iter()).collect();
    if notes.is_empty() {
        return Err(Error::input("the reference file has no notes"));
    }
    let mean_v = notes.iter().map(|n| n.velocity as f64).sum::<f64>() / notes.len() as f64;
    let mut acc = vec![(0.0f64, 0.0f64, 0usize); steps];
    for n in &notes {
        let k = (n.tick as f64 / step).round() as i64;
        let p = k.rem_euclid(steps as i64) as usize;
        acc[p].0 += (n.tick as f64 - k as f64 * step) / step;
        acc[p].1 += n.velocity as f64 - mean_v;
        acc[p].2 += 1;
    }
    Ok(Groove {
        ferraus_groove: 1,
        grid: grid.to_string(),
        steps,
        positions: acc
            .iter()
            .enumerate()
            .map(|(i, (o, v, c))| GrooveStep {
                step: i,
                offset: if *c > 0 { o / *c as f64 } else { 0.0 },
                velocity_delta: if *c > 0 { v / *c as f64 } else { 0.0 },
                count: *c,
            })
            .collect(),
        source: None,
    })
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct EditReport {
    pub notes_changed: usize,
    pub notes_dropped: usize,
}

/// Transpose notes by whole semitones. Channel 10 (drums) is skipped unless `include_drums`.
/// Notes pushed outside 0..=127 are dropped.
pub fn transpose(f: &mut MidiFile, semitones: i32, include_drums: bool, tracks: &Option<Vec<usize>>) -> EditReport {
    let mut r = EditReport::default();
    for (ti, t) in f.tracks.iter_mut().enumerate() {
        if !selected(tracks, ti) {
            continue;
        }
        let before = t.notes.len();
        t.notes.retain_mut(|n| {
            if n.channel == 9 && !include_drums {
                return true;
            }
            let p = n.pitch as i32 + semitones;
            if !(0..=127).contains(&p) {
                return false;
            }
            if semitones != 0 {
                r.notes_changed += 1;
            }
            n.pitch = p as u8;
            true
        });
        r.notes_dropped += before - t.notes.len();
        for e in &mut t.poly_pressure {
            if e.data.channel != 9 || include_drums {
                e.data.pitch = (e.data.pitch as i32 + semitones).clamp(0, 127) as u8;
            }
        }
    }
    r
}

#[derive(Clone, Debug, Default)]
pub struct VelocityOptions {
    pub set: Option<u8>,
    pub scale: f64,
    /// 0..=1: pull velocities toward their mean.
    pub compress: f64,
    /// Map the current min..max onto lo..hi.
    pub range: Option<(u8, u8)>,
}

/// Change note velocities: set, then scale, then compress, then range; clamped to 1..=127.
pub fn velocity(f: &mut MidiFile, o: &VelocityOptions, tracks: &Option<Vec<usize>>) -> EditReport {
    let mut r = EditReport::default();
    let sel: Vec<usize> = (0..f.tracks.len()).filter(|i| selected(tracks, *i)).collect();
    let mut vals: Vec<f64> = sel
        .iter()
        .flat_map(|&i| f.tracks[i].notes.iter().map(|n| n.velocity as f64))
        .collect();
    if vals.is_empty() {
        return r;
    }
    if let Some(s) = o.set {
        vals.iter_mut().for_each(|v| *v = s as f64);
    }
    vals.iter_mut().for_each(|v| *v *= o.scale);
    if o.compress > 0.0 {
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        let c = o.compress.clamp(0.0, 1.0);
        vals.iter_mut().for_each(|v| *v = mean + (*v - mean) * (1.0 - c));
    }
    if let Some((lo, hi)) = o.range {
        let mn = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let mx = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        vals.iter_mut().for_each(|v| {
            *v = if mx - mn < 1e-9 {
                (lo as f64 + hi as f64) / 2.0
            } else {
                lo as f64 + (*v - mn) / (mx - mn) * (hi as f64 - lo as f64)
            }
        });
    }
    let mut k = 0;
    for &i in &sel {
        for n in &mut f.tracks[i].notes {
            let v = vals[k].round().clamp(1.0, 127.0) as u8;
            k += 1;
            if v != n.velocity {
                r.notes_changed += 1;
            }
            n.velocity = v;
        }
    }
    r
}

/// Extend every note to the next onset in its track and channel (chords move together),
/// leaving `gap_ticks(at_tick)` ticks of space. The last notes keep their length.
pub fn legato(f: &mut MidiFile, gap_ticks: &dyn Fn(u64) -> u64, tracks: &Option<Vec<usize>>) -> EditReport {
    let mut r = EditReport::default();
    for (ti, t) in f.tracks.iter_mut().enumerate() {
        if !selected(tracks, ti) {
            continue;
        }
        let mut by_ch: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
        for (i, n) in t.notes.iter().enumerate() {
            by_ch.entry(n.channel).or_default().push(i);
        }
        for idx in by_ch.values() {
            let mut onsets: Vec<u64> = idx.iter().map(|&i| t.notes[i].tick).collect();
            onsets.sort();
            onsets.dedup();
            for &i in idx {
                let n = &mut t.notes[i];
                if let Some(&next) = onsets.iter().find(|&&o| o > n.tick) {
                    let end = next.saturating_sub(gap_ticks(next)).max(n.tick + 1);
                    if end != n.end() {
                        n.duration = end - n.tick;
                        r.notes_changed += 1;
                    }
                }
            }
        }
        t.clear_seqs();
    }
    f.fit_ends();
    r
}

/// Replace the tempo map with one tempo (notes keep their beat positions).
pub fn tempo_set(f: &mut MidiFile, bpm: f64) {
    f.tempos = vec![Timed::new(
        0,
        Tempo {
            us_per_quarter: bpm_to_us(bpm),
            track: 0,
        },
    )];
}

/// Multiply every tempo by `factor` (2.0 = twice as fast).
pub fn tempo_scale(f: &mut MidiFile, factor: f64) {
    if f.tempos.is_empty() {
        tempo_set(f, 120.0 * factor);
        return;
    }
    for t in &mut f.tempos {
        t.data.us_per_quarter = bpm_to_us(us_to_bpm(t.data.us_per_quarter) * factor);
    }
}

/// How `split` groups events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitBy {
    Track,
    Channel,
}

/// Split into one file per note-bearing track (or channel). Tempo, time and key
/// signatures are copied into every part. Returns `(role, file)` pairs.
pub fn split(f: &MidiFile, by: SplitBy) -> Vec<(String, MidiFile)> {
    let base = |track: Track| {
        let mut g = MidiFile {
            format: 1,
            timing: f.timing,
            tempos: f.tempos.iter().cloned().map(|mut e| {
                e.data.track = 0;
                e.seq = None;
                e
            }).collect(),
            time_signatures: f.time_signatures.iter().cloned().map(|mut e| {
                e.data.track = 0;
                e.seq = None;
                e
            }).collect(),
            key_signatures: f.key_signatures.iter().cloned().map(|mut e| {
                e.data.track = 0;
                e.seq = None;
                e
            }).collect(),
            tracks: vec![track],
            extra_chunks: vec![],
        };
        g.format = 0;
        g.clear_seqs();
        g.fit_ends();
        g
    };
    let mut out = Vec::new();
    match by {
        SplitBy::Track => {
            for (i, t) in f.tracks.iter().enumerate() {
                if t.notes.is_empty() {
                    continue;
                }
                out.push((format!("track{}", i + 1), base(t.clone())));
            }
        }
        SplitBy::Channel => {
            let mut chans: Vec<u8> = f.tracks.iter().flat_map(|t| t.notes.iter().map(|n| n.channel)).collect();
            chans.sort();
            chans.dedup();
            for ch in chans {
                let mut t = Track::new();
                for src in &f.tracks {
                    t.notes.extend(src.notes.iter().filter(|n| n.channel == ch).cloned());
                    t.controllers.extend(src.controllers.iter().filter(|e| e.data.channel == ch).cloned());
                    t.programs.extend(src.programs.iter().filter(|e| e.data.channel == ch).cloned());
                    t.pitch_bends.extend(src.pitch_bends.iter().filter(|e| e.data.channel == ch).cloned());
                    t.channel_pressure.extend(src.channel_pressure.iter().filter(|e| e.data.channel == ch).cloned());
                    t.poly_pressure.extend(src.poly_pressure.iter().filter(|e| e.data.channel == ch).cloned());
                }
                t.notes.sort_by_key(|n| (n.tick, n.pitch));
                t.end_tick = f.duration_ticks();
                out.push((format!("ch{}", ch + 1), base(t)));
            }
        }
    }
    out
}

/// Merge files into one format-1 file at the first file's PPQ. `single_track` merges all
/// notes into one track. The tempo map comes from the first file that has one.
pub fn merge(files: &[MidiFile], single_track: bool) -> Result<(MidiFile, Vec<String>)> {
    let first = files.first().ok_or_else(|| Error::usage("merge needs at least one input"))?;
    let ppq = first.ppq().unwrap_or(960);
    let mut warnings = Vec::new();
    let mut out = MidiFile {
        format: 1,
        timing: Timing::Ppq(ppq),
        tempos: vec![],
        time_signatures: vec![],
        key_signatures: vec![],
        tracks: vec![],
        extra_chunks: vec![],
    };
    let mut merged = Track::new();
    for (fi, f) in files.iter().enumerate() {
        let mut g = f.clone();
        if g.ppq().is_none() {
            return Err(Error::unsupported("merge needs PPQ-timed files, not SMPTE timing"));
        }
        g.set_ppq(ppq);
        if out.tempos.is_empty() && !g.tempos.is_empty() {
            out.tempos = g.tempos.iter().cloned().map(|mut e| {
                e.data.track = 0;
                e.seq = None;
                e
            }).collect();
            out.time_signatures = g.time_signatures.iter().cloned().map(|mut e| {
                e.data.track = 0;
                e.seq = None;
                e
            }).collect();
            out.key_signatures = g.key_signatures.iter().cloned().map(|mut e| {
                e.data.track = 0;
                e.seq = None;
                e
            }).collect();
        } else if fi > 0 && g.tempo_map().segments() != out.tempo_map().segments() && !g.tempos.is_empty() {
            warnings.push(format!("input {} has a different tempo map; the first file's tempo map is used", fi + 1));
        }
        for t in g.tracks {
            if single_track {
                let Track {
                    notes,
                    controllers,
                    programs,
                    pitch_bends,
                    channel_pressure,
                    poly_pressure,
                    meta,
                    sysex,
                    raw,
                    end_tick,
                    ..
                } = t;
                merged.notes.extend(notes);
                merged.controllers.extend(controllers);
                merged.programs.extend(programs);
                merged.pitch_bends.extend(pitch_bends);
                merged.channel_pressure.extend(channel_pressure);
                merged.poly_pressure.extend(poly_pressure);
                merged.meta.extend(meta.into_iter().filter(|m| m.data.kind != 0x03));
                merged.sysex.extend(sysex);
                merged.raw.extend(raw);
                merged.end_tick = merged.end_tick.max(end_tick);
            } else {
                out.tracks.push(t);
            }
        }
    }
    if single_track {
        merged.notes.sort_by_key(|n| (n.tick, n.pitch));
        merged.clear_seqs();
        out.tracks.push(merged);
        out.format = 0;
    } else {
        // tempo events now live in the merged header; seqs from different files cannot be compared
        out.clear_seqs();
    }
    out.fit_ends();
    Ok((out, warnings))
}

/// Keep `[start, end)` ticks. Notes overlapping the edges are clipped. With `shift`, the
/// range start becomes tick 0 and the tempo, signatures, programs, and controller values
/// in effect at the start are carried to tick 0.
pub fn trim(f: &MidiFile, start: u64, end: u64, shift: bool) -> MidiFile {
    let mut g = f.clone();
    let off = if shift { start } else { 0 };
    let inside = |t: u64| t >= start && t < end;
    let carry = |t: u64| t < start;
    // global maps: keep events inside; carry the last one before start
    fn trim_global<T: Clone>(v: &[Timed<T>], start: u64, end: u64, off: u64) -> Vec<Timed<T>> {
        let mut out = Vec::new();
        let at_start = v.iter().any(|e| e.tick == start);
        if !at_start {
            if let Some(last) = v.iter().filter(|e| e.tick < start).max_by_key(|e| e.tick) {
                let mut c = last.clone();
                c.tick = start - off;
                c.seq = None;
                out.push(c);
            }
        }
        for e in v.iter().filter(|e| e.tick >= start && e.tick < end) {
            let mut c = e.clone();
            c.tick -= off;
            out.push(c);
        }
        out
    }
    g.tempos = trim_global(&f.tempos, start, end, off);
    g.time_signatures = trim_global(&f.time_signatures, start, end, off);
    g.key_signatures = trim_global(&f.key_signatures, start, end, off);
    for (src, t) in f.tracks.iter().zip(g.tracks.iter_mut()) {
        t.notes = src
            .notes
            .iter()
            .filter(|n| n.tick < end && n.end() > start || (n.duration == 0 && inside(n.tick)))
            .map(|n| {
                let mut c = n.clone();
                let s = n.tick.max(start);
                let e = n.end().min(end);
                if s != n.tick || e != n.end() {
                    c.seq = None;
                    c.off_seq = None;
                }
                c.tick = s - off;
                c.duration = e.saturating_sub(s);
                c
            })
            .collect();
        macro_rules! keep {
            ($field:ident) => {
                t.$field = src
                    .$field
                    .iter()
                    .filter(|e| inside(e.tick))
                    .cloned()
                    .map(|mut e| {
                        e.tick -= off;
                        e
                    })
                    .collect();
            };
        }
        keep!(pitch_bends);
        keep!(channel_pressure);
        keep!(poly_pressure);
        keep!(sysex);
        keep!(raw);
        // meta: keep names (carry), others inside
        t.meta = src
            .meta
            .iter()
            .filter(|e| inside(e.tick) || (carry(e.tick) && matches!(e.data.kind, 0x03 | 0x04 | 0x20 | 0x21)))
            .cloned()
            .map(|mut e| {
                if e.tick < start {
                    e.tick = start;
                    e.seq = None;
                }
                e.tick -= off;
                e
            })
            .collect();
        // programs and controllers: carry the last value before start per channel/number
        let mut progs: BTreeMap<u8, Timed<Program>> = BTreeMap::new();
        let mut ccs: BTreeMap<(u8, u8), Timed<Controller>> = BTreeMap::new();
        for e in src.programs.iter().filter(|e| carry(e.tick)) {
            progs.insert(e.data.channel, e.clone());
        }
        for e in src.controllers.iter().filter(|e| carry(e.tick)) {
            ccs.insert((e.data.channel, e.data.controller), e.clone());
        }
        t.programs = progs
            .into_values()
            .map(|mut e| {
                e.tick = start - off;
                e.seq = None;
                e
            })
            .chain(src.programs.iter().filter(|e| inside(e.tick)).cloned().map(|mut e| {
                e.tick -= off;
                e
            }))
            .collect();
        t.controllers = ccs
            .into_values()
            .map(|mut e| {
                e.tick = start - off;
                e.seq = None;
                e
            })
            .chain(src.controllers.iter().filter(|e| inside(e.tick)).cloned().map(|mut e| {
                e.tick -= off;
                e
            }))
            .collect();
        t.end_tick = end.min(src.end_tick.max(src.last_event_tick())).max(start) - off;
    }
    g.fit_ends();
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g16() -> Grid {
        Grid::parse("1/16").unwrap()
    }

    #[test]
    fn detects_grids() {
        let ppq = 480;
        let eighths: Vec<u64> = (0..16).map(|i| i * 240 + (i % 3)).collect();
        assert_eq!(detect_grid(&eighths, ppq).to_string(), "1/8");
        let trip: Vec<u64> = (0..12).map(|i| i * 160).collect();
        assert_eq!(detect_grid(&trip, ppq).to_string(), "1/8T");
        let sixteenths: Vec<u64> = (0..16).map(|i| i * 120).collect();
        assert_eq!(detect_grid(&sixteenths, ppq).to_string(), "1/16");
    }

    #[test]
    fn overlaps_removed() {
        let mut v = vec![Note::new(0, 500, 60, 100, 0), Note::new(480, 100, 60, 90, 0), Note::new(480, 50, 60, 80, 0)];
        assert_eq!(remove_overlaps(&mut v), 1);
        assert_eq!(v[0].duration, 480);
    }

    #[test]
    fn strength_half_moves_halfway() {
        let mut f = MidiFile::from_beat_notes(&[(60, 0.1, 0.2, 100, 1)], 120.0, 480);
        let o = QuantizeOptions {
            grid: Some(g16()),
            strength: 0.5,
            ..Default::default()
        };
        quantize(&mut f, &o).unwrap();
        // 48 ticks -> grid 0 at 50%: 24
        assert_eq!(f.tracks[0].notes[0].tick, 24);
        assert_eq!(f.tracks[0].notes[0].duration, 96);
    }

    #[test]
    fn what_modes() {
        let base = MidiFile::from_beat_notes(&[(60, 0.1, 0.6, 100, 1)], 120.0, 480);
        let run = |what| {
            let mut f = base.clone();
            quantize(&mut f, &QuantizeOptions { grid: Some(g16()), what, ..Default::default() }).unwrap();
            (f.tracks[0].notes[0].tick, f.tracks[0].notes[0].end())
        };
        // on=48, off=336
        assert_eq!(run(QuantizeWhat::Start), (0, 288));
        assert_eq!(run(QuantizeWhat::End), (48, 360));
        assert_eq!(run(QuantizeWhat::Both), (0, 360));
        assert_eq!(run(QuantizeWhat::Duration), (0, 240));
    }
}
