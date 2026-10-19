//! Hum-to-MIDI: the `transcribe` command.
//!
//! **Mono** (singing, humming, whistling, monophonic instruments): pitch track at a 10 ms
//! hop (pYIN, or a model tracker registered by `ferraus-ml`) → voiced segments → split on
//! sustained pitch jumps over 80 cents or energy dips ([`ferraus_pitch::notes::segment`])
//! → median pitch per segment rounded to a MIDI note → velocity from peak RMS through a
//! loudness curve → merge sub-minimum notes → optional key snap → optional octave shift →
//! tempo map → MIDI file plus a JSON sidecar with per-note confidence.
//!
//! **Poly**: Basic Pitch activations from a registered [`poly::PolyTranscriber`], decoded by
//! [`poly::output_to_notes`], then the same merge, snap, octave, and tempo rules.
//!
//! **Auto** runs the pitch tracker, applies the monophony test in [`polyphony`], and
//! transcribes in mono when it finds a single voiced line; on polyphonic input it uses the
//! poly transcriber when one is installed and otherwise falls back to mono with a warning.
//!
//! The pieces are public so other commands (`hum`, `score`) can reuse them: [`transcribe`]
//! takes an [`AudioBuffer`] and [`TranscribeOptions`] and returns a [`Transcription`].

pub mod eval;
pub mod poly;
pub mod polyphony;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

use crate::edit::{self, QuantizeOptions, QuantizeReport};
use crate::gm;
use crate::model::{Controller, KeySig, MidiFile, Note, PitchBend, Program, Tempo, Timed, Track};
use crate::tempo::bpm_to_us;
use ferraus_analysis::tempo::{detect_tempo, detect_tempo_from_onsets, OnsetEnvelope, TempoOptions};
use ferraus_core::ctx::OutputRecord;
use ferraus_core::prelude::*;
use ferraus_core::units::{hz_to_midi, midi_to_name, Grid};
use ferraus_pitch::key::{self, Key, Mode as KeyMode};
use ferraus_pitch::notes::{self as pnotes, NoteOptions};
use ferraus_pitch::pyin::PitchFrame;
use ferraus_pitch::tracker::{self, TrackOptions};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub use polyphony::PolyphonyEstimate;

/// PPQ of written files.
pub const PPQ: u16 = 960;

/// `--mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Mono,
    Poly,
    Auto,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        match s.trim() {
            "mono" => Ok(Mode::Mono),
            "poly" => Ok(Mode::Poly),
            "auto" => Ok(Mode::Auto),
            o => Err(Error::usage(format!("--mode must be mono, poly, or auto, not '{o}'"))),
        }
    }
}

/// `--velocity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Velocity {
    /// Peak RMS (mono) or mean activation (poly) through the loudness curve.
    FromRms,
    Fixed(u8),
}

impl Velocity {
    pub fn parse(s: &str) -> Result<Velocity> {
        let t = s.trim();
        if t == "from-rms" {
            return Ok(Velocity::FromRms);
        }
        let n = t.strip_prefix("fixed:").or_else(|| t.strip_prefix("fixed=")).unwrap_or(t);
        match n.trim().parse::<u8>() {
            Ok(v) if (1..=127).contains(&v) => Ok(Velocity::Fixed(v)),
            _ => Err(Error::usage(format!("--velocity must be from-rms or fixed:<1-127>, not '{s}'"))),
        }
    }
}

/// `--octave`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Octave {
    Auto,
    Shift(i32),
}

impl Octave {
    pub fn parse(s: &str) -> Result<Octave> {
        let t = s.trim();
        if t == "auto" {
            return Ok(Octave::Auto);
        }
        match t.trim_start_matches('+').parse::<i32>() {
            Ok(v) if (-4..=4).contains(&v) => Ok(Octave::Shift(v)),
            _ => Err(Error::usage(format!("--octave must be auto or a whole number of octaves -4..4, not '{s}'"))),
        }
    }
}

/// `--quantize`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuantizeSetting {
    Off,
    /// Detect the dominant subdivision.
    Auto,
    Grid(Grid),
}

/// Everything `transcribe` can be told.
#[derive(Clone, Debug)]
pub struct TranscribeOptions {
    pub mode: Mode,
    /// Pitch tracker for mono mode (`pyin`, `crepe`, ...).
    pub tracker: String,
    /// Minimum note length (seconds).
    pub min_note_s: f64,
    /// 0..1: energy-dip and pitch-jump sensitivity (0.5 = 80-cent jumps, 9 dB dips).
    pub onset_sensitivity: f64,
    pub velocity: Velocity,
    /// Fixed tempo, or `None` to detect it.
    pub bpm: Option<f64>,
    pub quantize: QuantizeSetting,
    /// Strength of `quantize` (0..1).
    pub quantize_strength: f64,
    pub key_snap: bool,
    pub pitch_bend: bool,
    pub octave: Octave,
    /// GM program (0-based).
    pub program: u8,
    /// Seed for anything stochastic (quantize humanize is off, so unused today).
    pub seed: u64,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        TranscribeOptions {
            mode: Mode::Auto,
            tracker: "pyin".into(),
            min_note_s: 0.060,
            onset_sensitivity: 0.5,
            velocity: Velocity::FromRms,
            bpm: None,
            quantize: QuantizeSetting::Off,
            quantize_strength: 1.0,
            key_snap: true,
            pitch_bend: false,
            octave: Octave::Auto,
            program: 0,
            seed: 0,
        }
    }
}

/// One transcribed note (as written, after snapping, shifting, and quantizing).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TranscribedNote {
    pub start_s: f64,
    pub end_s: f64,
    pub duration_s: f64,
    /// Onset in quarter-note beats from the start of the file.
    pub start_beats: f64,
    pub pitch: u8,
    pub name: String,
    pub velocity: u8,
    /// Transcriber confidence for this note (0..1): mean voicing probability (mono) or
    /// mean note activation (poly).
    pub confidence: f64,
    /// Detected pitch before rounding, snapping, and octave shift (fractional MIDI).
    pub detected_midi: f64,
    /// Detected onset and offset before quantizing (seconds).
    pub detected_start_s: f64,
    pub detected_end_s: f64,
    /// True when key snap moved this note.
    pub key_snapped: bool,
}

/// Detected or given key.
#[derive(Clone, Debug, Serialize)]
pub struct KeyInfo {
    pub key: String,
    pub confidence: f64,
    /// Key snap was requested and the confidence was above 0.6.
    pub snapped: bool,
    pub notes_snapped: usize,
}

/// Tempo used for the file.
#[derive(Clone, Debug, Serialize)]
pub struct TempoInfo {
    pub bpm: f64,
    /// `fixed`, `notes` (note onsets), `audio` (onset envelope of the recording), or `default`.
    pub source: String,
    pub confidence: f64,
    /// Time of the first beat (seconds). When it is not at 0 the file starts with a one-beat
    /// pickup tempo so that beats (and the quantize grid) line up with the recording.
    pub first_beat_s: f64,
}

/// The result of [`transcribe`].
#[derive(Clone, Debug)]
pub struct Transcription {
    pub midi: MidiFile,
    pub notes: Vec<TranscribedNote>,
    /// `mono` or `poly`.
    pub mode: &'static str,
    /// Tracker (`pyin`, ...) or poly transcriber name.
    pub engine: String,
    pub tempo: TempoInfo,
    pub key: Option<KeyInfo>,
    pub octave_shift: i32,
    pub polyphony: Option<PolyphonyEstimate>,
    pub quantize: Option<QuantizeReport>,
    pub pitch_bends: usize,
    /// (code, message)
    pub warnings: Vec<(String, String)>,
    /// (name, version, sha256) of a model used.
    pub model: Option<(String, String, String)>,
}

/// A note before the shared post-processing.
#[derive(Clone, Debug)]
struct RawNote {
    start_s: f64,
    end_s: f64,
    midi: f64,
    confidence: f64,
    /// Linear peak RMS (mono) — `None` in poly mode.
    peak_rms: Option<f64>,
    /// Mean activation (poly).
    amplitude: Option<f64>,
    /// (time_s, fractional midi) of voiced frames (mono), for pitch bends.
    contour: Vec<(f64, f64)>,
    pitch: i32,
    key_snapped: bool,
    velocity: u8,
}

impl RawNote {
    fn dur(&self) -> f64 {
        self.end_s - self.start_s
    }
}

// -------------------------------------------------------------------------------------
// core pipeline
// -------------------------------------------------------------------------------------

/// Transcribe a buffer (any channel count; channels are mixed to mono).
pub fn transcribe(buf: &AudioBuffer, o: &TranscribeOptions) -> Result<Transcription> {
    let sr = buf.sample_rate;
    let x = buf.mono_mix();
    let mut warnings: Vec<(String, String)> = Vec::new();
    let mut model = None;
    let mut polyphony = None;

    let track = |name: &str, model: &mut Option<(String, String, String)>| -> Result<Vec<PitchFrame>> {
        let t = tracker::by_name(name)?;
        if let Some(m) = t.model() {
            *model = Some(m);
        }
        let opts = TrackOptions { fmin: 55.0, fmax: 1760.0f64.min(0.45 * sr as f64), hop_s: 0.010 };
        t.track(&x, sr, &opts)
    };

    let (raw, mode, engine) = match o.mode {
        Mode::Mono => {
            let frames = track(&o.tracker, &mut model)?;
            (mono_notes(&frames, o), "mono", o.tracker.clone())
        }
        Mode::Poly => {
            let t = poly::poly_transcriber()?;
            let (notes, name) = poly_notes(&*t, &x, sr, o, &mut model)?;
            (notes, "poly", name)
        }
        Mode::Auto => {
            let frames = track(&o.tracker, &mut model)?;
            let est = polyphony::estimate(&x, sr, &frames);
            let mono = est.monophonic;
            polyphony = Some(est);
            if mono {
                (mono_notes(&frames, o), "mono", o.tracker.clone())
            } else {
                match poly::poly_transcriber() {
                    Ok(t) => {
                        let (notes, name) = poly_notes(&*t, &x, sr, o, &mut model)?;
                        (notes, "poly", name)
                    }
                    Err(e) => {
                        let why = if e.kind == ErrorKind::ModelNotCached {
                            format!(
                                "the input sounds polyphonic, but the polyphonic transcriber is not installed; transcribed as a single line instead. Install it with 'ferraus models pull {}'",
                                poly::MODEL_NAME
                            )
                        } else {
                            format!("the input sounds polyphonic, but the polyphonic transcriber failed ({}); transcribed as a single line instead", e.message)
                        };
                        warnings.push(("polyphonic_input".into(), why));
                        (mono_notes(&frames, o), "mono", o.tracker.clone())
                    }
                }
            }
        }
    };

    finish(buf, raw, mode, engine, o, warnings, model, polyphony)
}

fn mono_notes(frames: &[PitchFrame], o: &TranscribeOptions) -> Vec<RawNote> {
    let s = o.onset_sensitivity.clamp(0.0, 1.0);
    let nopts = NoteOptions {
        min_note_s: o.min_note_s,
        jump_cents: 80.0 * (1.5 - s),
        stay_s: 0.030,
        sensitivity: s,
        gap_fill_s: 0.020,
    };
    let hop = pnotes::track_hop(frames);
    let min_len = (2.0 * o.min_note_s / hop).round().max(12.0) as usize;
    let mut segs = refine_steps(frames, pnotes::segment(frames, &nopts), min_len, nopts.jump_cents / 160.0);
    refine_onsets(frames, &mut segs);
    segs.into_iter()
        .map(|n| {
            let contour = frames[n.start_frame..n.end_frame]
                .iter()
                .filter(|f| f.voiced && f.freq_hz > 0.0)
                .map(|f| (f.time_s, hz_to_midi(f.freq_hz)))
                .collect();
            RawNote {
                start_s: n.start_s,
                end_s: n.end_s,
                midi: n.midi,
                confidence: n.confidence,
                peak_rms: Some(n.peak_rms),
                amplitude: None,
                contour,
                pitch: n.midi.round() as i32,
                key_snapped: false,
                velocity: 100,
            }
        })
        .collect()
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    Some(if n % 2 == 1 { s[n / 2] } else { 0.5 * (s[n / 2 - 1] + s[n / 2]) })
}

fn plateau(v: &[f64], m: f64) -> bool {
    2 * v.iter().filter(|x| (*x - m).abs() <= 0.3).count() >= v.len()
}

fn l1(v: &[f64], m: f64) -> f64 {
    v.iter().map(|x| (x - m).abs()).sum()
}

/// Describe frames `s..e` as a note (`None` when no frame is voiced).
fn note_from_frames(frames: &[PitchFrame], s: usize, e: usize) -> Option<pnotes::Note> {
    let hop = pnotes::track_hop(frames);
    let vf: Vec<usize> = (s..e).filter(|&k| frames[k].voiced && frames[k].freq_hz > 0.0).collect();
    let mids: Vec<f64> = vf.iter().map(|&k| hz_to_midi(frames[k].freq_hz)).collect();
    Some(pnotes::Note {
        start_s: frames[s].time_s,
        end_s: frames[e - 1].time_s + hop,
        midi: median(&mids)?,
        confidence: vf.iter().map(|&k| frames[k].confidence).sum::<f64>() / vf.len() as f64,
        peak_rms: (s..e).map(|k| frames[k].rms).fold(0.0, f64::max),
        start_frame: s,
        end_frame: e,
    })
}

/// Split notes that hold two pitches in succession: legato steps whose jump stays under
/// the jump threshold because the voice drifts (a semitone step with ±20 cents of drift
/// can be a 60-cent jump). The best split is the piecewise-constant (L1) fit of the
/// note's pitch track; it is taken when the two sides' medians differ by at least `step`
/// semitones, round to different MIDI notes, and the fit removes at least 40% of the
/// deviation. Each side must be `min_len` voiced frames long and hold a plateau (half of
/// its frames within 30 cents of its median), so a glide between two notes never becomes a
/// passing note of its own; sides are refined again.
fn refine_steps(frames: &[PitchFrame], notes: Vec<pnotes::Note>, min_len: usize, step: f64) -> Vec<pnotes::Note> {
    let mut out = Vec::with_capacity(notes.len());
    let mut stack: Vec<pnotes::Note> = notes.into_iter().rev().collect();
    while let Some(n) = stack.pop() {
        let idx: Vec<usize> =
            (n.start_frame..n.end_frame).filter(|&k| frames[k].voiced && frames[k].freq_hz > 0.0).collect();
        if idx.len() < 2 * min_len {
            out.push(n);
            continue;
        }
        let m: Vec<f64> = idx.iter().map(|&k| hz_to_midi(frames[k].freq_hz)).collect();
        let total = l1(&m, median(&m).unwrap_or(n.midi));
        let mut best: Option<(usize, f64)> = None;
        for k in min_len..=(m.len() - min_len) {
            let (a, b) = m.split_at(k);
            let (ma, mb) = (median(a).unwrap_or(0.0), median(b).unwrap_or(0.0));
            if (ma - mb).abs() < step || ma.round() == mb.round() || !plateau(a, ma) || !plateau(b, mb) {
                continue;
            }
            let cost = l1(a, ma) + l1(b, mb);
            if best.map(|(_, c)| cost < c).unwrap_or(true) {
                best = Some((k, cost));
            }
        }
        match best {
            Some((k, cost)) if cost < 0.6 * total => {
                let at = idx[k];
                match (note_from_frames(frames, n.start_frame, at), note_from_frames(frames, at, n.end_frame)) {
                    (Some(l), Some(r)) => {
                        stack.push(r);
                        stack.push(l);
                    }
                    _ => out.push(n),
                }
            }
            _ => out.push(n),
        }
    }
    out
}

/// Move each re-articulated note's onset back to where its energy rises: pYIN declares
/// voicing only once a few periods are clear, which on soft or breathy onsets is 20 to 50
/// ms after the note starts. The onset moves to the earliest frame (at most 120 ms back,
/// never into the previous note) whose RMS is above both −20 dB of the note's peak and
/// 6 dB over the recording's noise floor (10th percentile of frame RMS). Legato notes
/// (starting where the previous one ends) keep their pitch-change onset.
fn refine_onsets(frames: &[PitchFrame], notes: &mut [pnotes::Note]) {
    if frames.is_empty() {
        return;
    }
    let mut r: Vec<f64> = frames.iter().map(|f| f.rms).collect();
    r.sort_by(|a, b| a.total_cmp(b));
    let floor = r[r.len() / 10];
    let max_back = 12;
    for i in 0..notes.len() {
        let limit = if i > 0 { notes[i - 1].end_frame } else { 0 };
        let s = notes[i].start_frame;
        if i > 0 && limit >= s {
            continue;
        }
        let thr = (notes[i].peak_rms * 0.1).max(floor * 2.0);
        let mut k = s;
        while k > limit && s - (k - 1) <= max_back && frames[k - 1].rms > thr {
            k -= 1;
        }
        if k < s {
            notes[i].start_frame = k;
            notes[i].start_s = frames[k].time_s;
        }
    }
}

fn poly_notes(
    t: &dyn poly::PolyTranscriber,
    x: &[f32],
    sr: u32,
    o: &TranscribeOptions,
    model: &mut Option<(String, String, String)>,
) -> Result<(Vec<RawNote>, String)> {
    if let Some(m) = t.model() {
        *model = Some(m);
    }
    let act = t.activations(x, sr)?;
    let bo = poly::BasicPitchOptions {
        onset_thresh: (1.0 - o.onset_sensitivity).clamp(0.05, 0.95) as f32,
        min_note_len: (o.min_note_s * poly::frames_per_second()).round().max(1.0) as usize,
        include_pitch_bends: false,
        ..Default::default()
    };
    let notes = poly::output_to_notes(&act, &bo)?
        .into_iter()
        .map(|n| RawNote {
            start_s: n.start_s,
            end_s: n.end_s,
            midi: n.pitch as f64,
            confidence: n.amplitude as f64,
            peak_rms: None,
            amplitude: Some(n.amplitude as f64),
            contour: Vec::new(),
            pitch: n.pitch as i32,
            key_snapped: false,
            velocity: 100,
        })
        .collect();
    Ok((notes, t.name().to_string()))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    buf: &AudioBuffer,
    mut raw: Vec<RawNote>,
    mode: &'static str,
    engine: String,
    o: &TranscribeOptions,
    mut warnings: Vec<(String, String)>,
    model: Option<(String, String, String)>,
    polyphony: Option<PolyphonyEstimate>,
) -> Result<Transcription> {
    raw.sort_by(|a, b| a.start_s.total_cmp(&b.start_s).then(a.midi.total_cmp(&b.midi)));
    merge_short(&mut raw, o.min_note_s);

    // key detection and snap
    let key = if raw.len() >= 3 { Some(snap_to_key(&mut raw, o.key_snap)) } else { None };

    // slides as pitch bends (mono only)
    if o.pitch_bend && mode == "mono" {
        merge_slides(&mut raw);
    } else if o.pitch_bend {
        warnings.push((
            "pitch_bend_mono_only".into(),
            "--pitch-bend is applied in mono mode only (pitch bends are channel-wide)".into(),
        ));
    }

    // octave
    let octave_shift = match o.octave {
        Octave::Shift(n) => n,
        Octave::Auto => auto_octave(&raw, o.program),
    };
    if octave_shift != 0 {
        for n in &mut raw {
            n.pitch += 12 * octave_shift;
        }
    }
    let before = raw.len();
    raw.retain(|n| (0..=127).contains(&n.pitch));
    if raw.len() < before {
        warnings.push((
            "notes_out_of_range".into(),
            format!("{} notes fell outside MIDI 0-127 after the octave shift and were dropped", before - raw.len()),
        ));
    }

    // velocity
    assign_velocity(&mut raw, o.velocity);

    // tempo
    let tempo = estimate_tempo(buf, &raw, o.bpm, &mut warnings);

    // MIDI
    let (mut midi, pitch_bends) = build_midi(&raw, &tempo, o, key.as_ref());
    let quantize = match o.quantize {
        QuantizeSetting::Off => None,
        q => {
            let opts = QuantizeOptions {
                grid: match q {
                    QuantizeSetting::Grid(g) => Some(g),
                    _ => None,
                },
                strength: o.quantize_strength.clamp(0.0, 1.0),
                seed: o.seed,
                ..Default::default()
            };
            let before: Vec<u64> = midi.tracks[0].notes.iter().map(|n| n.tick).collect();
            let rep = edit::quantize(&mut midi, &opts)?;
            move_bends_with_notes(&mut midi.tracks[0], &before);
            Some(rep)
        }
    };
    let notes = describe_notes(&midi, &raw);
    if notes.is_empty() {
        warnings
            .push(("no_notes".into(), "no notes were found; the input may be silent, unpitched, or too short".into()));
    }
    Ok(Transcription {
        midi,
        notes,
        mode,
        engine,
        tempo,
        key,
        octave_shift,
        polyphony,
        quantize,
        pitch_bends,
        warnings,
        model,
    })
}

/// After quantizing a mono track, shift each note's pitch bends by the distance its note
/// moved (`before` = note ticks before quantizing, in order).
fn move_bends_with_notes(t: &mut Track, before: &[u64]) {
    if t.pitch_bends.is_empty() || t.notes.len() != before.len() || before.is_empty() {
        return;
    }
    let after: Vec<u64> = t.notes.iter().map(|n| n.tick).collect();
    for b in &mut t.pitch_bends {
        let i = before.partition_point(|&s| s <= b.tick).saturating_sub(1);
        b.tick = (b.tick as i64 + after[i] as i64 - before[i] as i64).max(0) as u64;
    }
    t.pitch_bends.sort_by_key(|b| b.tick);
    t.end_tick = t.end_tick.max(t.last_event_tick());
}

/// Notes shorter than `min_s` merge into a touching neighbour of (nearly) the same pitch,
/// else are dropped.
fn merge_short(notes: &mut Vec<RawNote>, min_s: f64) {
    const TOUCH: f64 = 0.030;
    while let Some(i) = notes.iter().position(|n| n.dur() < min_s - 1e-9) {
        let me = notes[i].clone();
        let near = |j: usize| -> bool {
            let n = &notes[j];
            let touching = (me.start_s - n.end_s).abs() <= TOUCH || (n.start_s - me.end_s).abs() <= TOUCH;
            (n.midi - me.midi).abs() <= 1.0 && touching
        };
        let cands: Vec<usize> = (0..notes.len()).filter(|&j| j != i && near(j)).collect();
        let target = cands
            .into_iter()
            .min_by(|&a, &b| (notes[a].midi - me.midi).abs().total_cmp(&(notes[b].midi - me.midi).abs()));
        match target {
            Some(j) => {
                let n = &mut notes[j];
                n.start_s = n.start_s.min(me.start_s);
                n.end_s = n.end_s.max(me.end_s);
                n.peak_rms = match (n.peak_rms, me.peak_rms) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                n.contour.extend(me.contour);
                n.contour.sort_by(|a, b| a.0.total_cmp(&b.0));
                notes.remove(i);
            }
            None => {
                notes.remove(i);
            }
        }
    }
}

/// Key snap moves an out-of-key note only when its detected pitch is within this many
/// semitones of a key note, so a chromatic note sung in tune stays where it is.
pub const SNAP_MAX_SEMITONES: f64 = 0.75;

/// Detect the key from the notes (duration × confidence weighted chroma, Krumhansl-
/// Schmuckler) and, when `snap` is set and the confidence is above 0.6, move out-of-key
/// notes that are within [`SNAP_MAX_SEMITONES`] of a key note onto it.
fn snap_to_key(notes: &mut [RawNote], snap: bool) -> KeyInfo {
    let pn: Vec<pnotes::Note> = notes
        .iter()
        .map(|n| pnotes::Note {
            start_s: n.start_s,
            end_s: n.end_s,
            midi: n.midi,
            confidence: n.confidence,
            peak_rms: n.peak_rms.unwrap_or(0.0),
            start_frame: 0,
            end_frame: 0,
        })
        .collect();
    let est = key::detect_from_chroma(&key::chroma_from_notes(&pn), None);
    let apply = snap && est.confidence > 0.6;
    let mut moved = 0;
    if apply {
        let pcs = est.best.pitch_classes();
        for n in notes.iter_mut() {
            if !pcs.contains(&n.pitch.rem_euclid(12)) {
                let target = key::nearest_in_scale(n.midi, &pcs);
                // a note sung clearly on the chromatic pitch (within 25 cents) is meant
                if (target - n.midi).abs() <= SNAP_MAX_SEMITONES {
                    n.pitch = target as i32;
                    n.key_snapped = true;
                    moved += 1;
                }
            }
        }
    }
    KeyInfo { key: est.key.clone(), confidence: est.confidence, snapped: apply, notes_snapped: moved }
}

/// Merge legato glides (portamento) of up to two semitones into the previous note; the
/// pitch movement is then carried by pitch bends.
fn merge_slides(notes: &mut Vec<RawNote>) {
    let mut out: Vec<RawNote> = Vec::with_capacity(notes.len());
    for n in notes.drain(..) {
        if let Some(prev) = out.last_mut() {
            let contiguous = (n.start_s - prev.end_s).abs() <= 0.015;
            let within = (n.pitch - prev.pitch).abs() <= 2 && n.pitch != prev.pitch;
            if contiguous && within && is_glide(prev, &n) {
                prev.end_s = n.end_s;
                prev.contour.extend(n.contour);
                prev.peak_rms = match (prev.peak_rms, n.peak_rms) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                continue;
            }
        }
        out.push(n);
    }
    *notes = out;
}

/// At least 30 ms of pitch strictly between the two notes (25 cents away from both)
/// within 100 ms of the boundary.
fn is_glide(a: &RawNote, b: &RawNote) -> bool {
    let (lo, hi) = if a.midi < b.midi { (a.midi, b.midi) } else { (b.midi, a.midi) };
    let t0 = b.start_s;
    let between = a
        .contour
        .iter()
        .chain(b.contour.iter())
        .filter(|(t, m)| (t - t0).abs() <= 0.1 && *m > lo + 0.25 && *m < hi - 0.25)
        .count();
    between >= 3
}

/// Comfortable median range (MIDI) for a GM program.
pub fn comfortable_range(program: u8) -> (i32, i32) {
    match program {
        0..=7 => (40, 84),
        8..=15 => (60, 84),
        16..=23 => (48, 79),
        24..=31 => (45, 72),
        32..=39 => (31, 50),
        40 => (62, 84),
        41 => (55, 76),
        42 => (43, 64),
        43 => (31, 50),
        44..=47 => (48, 79),
        48..=55 => (48, 76),
        56 => (58, 79),
        57 => (45, 67),
        58 => (33, 55),
        59..=63 => (50, 74),
        64 => (60, 84),
        65 => (55, 77),
        66 => (48, 72),
        67 => (40, 64),
        68 => (60, 84),
        69 => (55, 76),
        70 => (38, 62),
        71 => (52, 79),
        72..=79 => (62, 91),
        80..=87 => (55, 84),
        _ => (48, 79),
    }
}

fn auto_octave(notes: &[RawNote], program: u8) -> i32 {
    if notes.is_empty() {
        return 0;
    }
    let mut p: Vec<i32> = notes.iter().map(|n| n.pitch).collect();
    p.sort();
    let median = p[p.len() / 2];
    let (lo, hi) = comfortable_range(program);
    if median < lo {
        (lo - median + 11) / 12
    } else if median > hi {
        -((median - hi + 11) / 12)
    } else {
        0
    }
}

/// Loudness curve: the loudest note maps to 112 and velocity follows the square root of
/// amplitude (−6 dB → 79, −20 dB → 35, −40 dB → 11). Poly notes use basic-pitch's
/// `round(127 × activation)`.
fn assign_velocity(notes: &mut [RawNote], v: Velocity) {
    match v {
        Velocity::Fixed(n) => notes.iter_mut().for_each(|x| x.velocity = n),
        Velocity::FromRms => {
            let max = notes.iter().filter_map(|n| n.peak_rms).fold(0.0f64, f64::max);
            for n in notes.iter_mut() {
                n.velocity = match (n.peak_rms, n.amplitude) {
                    (Some(r), _) if max > 0.0 => {
                        let db = 20.0 * (r.max(1e-9) / max).log10();
                        (112.0 * 10f64.powf(db / 40.0)).round().clamp(1.0, 127.0) as u8
                    }
                    (_, Some(a)) => (127.0 * a).round().clamp(1.0, 127.0) as u8,
                    _ => 100,
                };
            }
        }
    }
}

/// Onset envelope (100 Hz) built from note onsets, for tempo detection on melodies whose
/// soft vocal onsets give a weak spectral-flux envelope.
fn notes_envelope(notes: &[RawNote]) -> OnsetEnvelope {
    let fr = 100.0;
    let end = notes.iter().map(|n| n.end_s).fold(0.0, f64::max) + 1.0;
    let len = (end * fr).ceil() as usize + 1;
    let mut v = vec![0.0f32; len];
    for n in notes {
        let c = n.start_s * fr;
        let w = 1.0 + 0.5 * (n.dur() / 0.5).min(1.0);
        let lo = (c - 6.0).floor().max(0.0) as usize;
        let hi = ((c + 6.0).ceil() as usize).min(len - 1);
        for (i, x) in v.iter_mut().enumerate().take(hi + 1).skip(lo) {
            let d = i as f64 - c;
            *x += (w * (-0.5 * (d / 1.5).powi(2)).exp()) as f32;
        }
    }
    let mean = v.iter().map(|x| *x as f64).sum::<f64>() / len as f64;
    let sd = (v.iter().map(|x| (*x as f64 - mean).powi(2)).sum::<f64>() / len as f64).sqrt().max(1e-9);
    let values: Vec<f32> = v.iter().map(|x| (*x as f64 / sd) as f32).collect();
    OnsetEnvelope { frame_rate: fr, low: vec![0.0; len], low_level_db: vec![-100.0; len], values }
}

fn estimate_tempo(
    buf: &AudioBuffer,
    notes: &[RawNote],
    fixed: Option<f64>,
    warnings: &mut Vec<(String, String)>,
) -> TempoInfo {
    let (bpm, source, confidence) = match fixed {
        Some(b) => (b, "fixed".to_string(), 1.0),
        None => {
            // sung and hummed ideas: prefer the beat over its subdivision (a 180 BPM line is
            // written at 90 BPM, which quantizes the same at half the grid)
            let opts = TempoOptions { min_bpm: 50.0, max_bpm: 170.0, ..TempoOptions::default() };
            let from_notes = (notes.len() >= 4).then(|| detect_tempo_from_onsets(&notes_envelope(notes), &opts));
            let from_audio = detect_tempo(buf, &opts);
            let mut best: Option<(f64, &str, f64)> = None;
            if let Some(r) = &from_notes {
                if r.bpm > 0.0 {
                    best = Some((r.bpm, "notes", r.confidence));
                }
            }
            if from_audio.bpm > 0.0 && best.map(|b| from_audio.confidence > b.2 + 0.1).unwrap_or(true) {
                best = Some((from_audio.bpm, "audio", from_audio.confidence));
            }
            match best {
                Some((b, s, c)) => (b, s.to_string(), c),
                None => {
                    warnings.push((
                        "tempo_not_detected".into(),
                        "no steady tempo was found; the file is written at 120 BPM. Pass --bpm to set it".into(),
                    ));
                    (120.0, "default".to_string(), 0.0)
                }
            }
        }
    };
    let bpm = if source == "notes" || source == "audio" { refine_bpm(notes, bpm) } else { bpm };
    let bpm = (bpm * 100.0).round() / 100.0;
    let period = 60.0 / bpm;
    // beat phase: weighted circular mean of note onsets modulo the beat period
    let (mut sx, mut sy) = (0.0, 0.0);
    for n in notes {
        let w = n.dur().clamp(0.05, 2.0);
        let a = 2.0 * std::f64::consts::PI * n.start_s / period;
        sx += w * a.cos();
        sy += w * a.sin();
    }
    let first_beat_s = if notes.is_empty() || (sx == 0.0 && sy == 0.0) {
        0.0
    } else {
        let ph = sy.atan2(sx) / (2.0 * std::f64::consts::PI) * period;
        ph.rem_euclid(period)
    };
    TempoInfo {
        bpm,
        source,
        confidence: (confidence * 1000.0).round() / 1000.0,
        first_beat_s: (first_beat_s * 1e4).round() / 1e4,
    }
}

/// Refine a detected tempo against the note onsets: within ±8% of `bpm`, pick the tempo
/// whose eighth-note grid the onsets fit best (largest resultant of the onset phases on
/// that grid, weighted by note length). Beat trackers settle within a few percent on
/// sparse melodic onsets; a grid fit over the whole take is exact to a fraction of a BPM.
fn refine_bpm(notes: &[RawNote], bpm: f64) -> f64 {
    if notes.len() < 6 {
        return bpm;
    }
    let fit = |b: f64| -> f64 {
        let sub = 30.0 / b; // eighth note
        let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
        for n in notes {
            let w = n.dur().clamp(0.05, 1.0);
            let a = 2.0 * std::f64::consts::PI * n.start_s / sub;
            sx += w * a.cos();
            sy += w * a.sin();
            sw += w;
        }
        (sx * sx + sy * sy).sqrt() / sw
    };
    let (lo, hi) = (bpm * 0.92, bpm * 1.08);
    let mut best = (bpm, fit(bpm));
    let mut b = lo;
    while b <= hi {
        let f = fit(b);
        if f > best.1 + 1e-9 {
            best = (b, f);
        }
        b += 0.05;
    }
    // fine pass around the coarse optimum
    let centre = best.0;
    let mut b = centre - 0.05;
    while b <= centre + 0.05 {
        let f = fit(b);
        if f > best.1 + 1e-12 {
            best = (b, f);
        }
        b += 0.002;
    }
    // only trust a clear grid fit
    if best.1 >= 0.5 {
        best.0
    } else {
        bpm
    }
}

/// Key signature sharps (negative = flats) for a key.
fn key_sharps(k: &Key) -> i8 {
    const MAJ: [i8; 12] = [0, -5, 2, -3, 4, -1, 6, 1, -4, 3, -2, 5];
    let rel = match k.mode {
        KeyMode::Major => k.tonic,
        KeyMode::Minor => k.tonic + 3,
    };
    MAJ[rel.rem_euclid(12) as usize]
}

/// The tempo map: constant `bpm`, preceded by a one-beat pickup segment when the first beat
/// is not at time 0, so that tick 960·k lands on the k-th beat of the recording.
fn tempo_events(t: &TempoInfo) -> Vec<Timed<Tempo>> {
    let p = 60.0 / t.bpm;
    let fb = t.first_beat_s;
    let main = Tempo { us_per_quarter: bpm_to_us(t.bpm), track: 0 };
    if fb < 0.02 || p - fb < 0.02 {
        return vec![Timed::new(0, main)];
    }
    let d0 = if fb >= 0.25 * p { fb } else { fb + p };
    vec![Timed::new(0, Tempo { us_per_quarter: (d0 * 1e6).round() as u32, track: 0 }), Timed::new(PPQ as u64, main)]
}

fn build_midi(notes: &[RawNote], tempo: &TempoInfo, o: &TranscribeOptions, key: Option<&KeyInfo>) -> (MidiFile, usize) {
    let mut f = MidiFile::new(PPQ, tempo.bpm);
    f.tempos = tempo_events(tempo);
    if let Some(k) = key.filter(|k| k.confidence > 0.6) {
        if let Some(best) = parse_key(&k.key) {
            f.key_signatures.push(Timed::new(
                0,
                KeySig { sharps: key_sharps(&best), minor: best.mode == KeyMode::Minor, track: 0 },
            ));
        }
    }
    let map = f.tempo_map();
    let mut t = Track::new();
    t.set_name("ferraus transcribe");
    t.programs.push(Timed::new(0, Program { channel: 0, program: o.program.min(127) }));
    let tick = |s: f64| map.seconds_to_tick(s.max(0.0)).round() as u64;
    let mut bends = 0usize;
    let use_bends = o.pitch_bend && notes.iter().any(|n| !n.contour.is_empty());
    if use_bends {
        // RPN 0 (pitch-bend range) = 2 semitones
        for (cc, v) in [(101u8, 0u8), (100, 0), (6, 2), (38, 0), (101, 127), (100, 127)] {
            t.controllers.push(Timed::new(0, Controller { channel: 0, controller: cc, value: v }));
        }
    }
    for n in notes {
        let s = tick(n.start_s);
        let e = tick(n.end_s).max(s + 1);
        t.notes.push(Note::new(s, e - s, n.pitch as u8, n.velocity, 0));
        if use_bends {
            let mut last: Option<i16> = None;
            for (ti, m) in &n.contour {
                let semis = (m - n.pitch as f64).clamp(-2.0, 2.0);
                let v = ((semis / 2.0) * 8192.0).round().clamp(-8192.0, 8191.0) as i16;
                // emit on changes of at least ~5 cents (205 units)
                if last.map(|l| (l as i32 - v as i32).abs() >= 205).unwrap_or(true) {
                    let at = tick(*ti).clamp(s, e.saturating_sub(1));
                    t.pitch_bends.push(Timed::new(at, PitchBend { channel: 0, value: v }));
                    last = Some(v);
                    bends += 1;
                }
            }
            if last.map(|l| l != 0).unwrap_or(false) {
                t.pitch_bends.push(Timed::new(e, PitchBend { channel: 0, value: 0 }));
                bends += 1;
            }
        }
    }
    t.notes.sort_by_key(|n| (n.tick, n.pitch));
    t.pitch_bends.sort_by_key(|b| b.tick);
    t.end_tick = t.last_event_tick();
    f.tracks = vec![t];
    (f, bends)
}

fn parse_key(s: &str) -> Option<Key> {
    let (tonic, mode) = Key::parse(s).ok()?;
    Some(Key { tonic, mode: mode.unwrap_or(KeyMode::Major) })
}

fn r4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// Describe the written notes, pairing each with the detected note it came from.
fn describe_notes(midi: &MidiFile, raw: &[RawNote]) -> Vec<TranscribedNote> {
    let map = midi.tempo_map();
    let tpb = midi.ticks_per_beat();
    let mut used = vec![false; raw.len()];
    let mut out = Vec::new();
    for t in &midi.tracks {
        for n in &t.notes {
            let s = map.tick_to_seconds(n.tick as f64);
            let e = map.tick_to_seconds(n.end() as f64);
            // nearest unused raw note of the same pitch
            let best = raw
                .iter()
                .enumerate()
                .filter(|(i, r)| !used[*i] && r.pitch == n.pitch as i32)
                .min_by(|a, b| (a.1.start_s - s).abs().total_cmp(&(b.1.start_s - s).abs()))
                .map(|(i, _)| i);
            let (conf, det, ds, de, snapped) = match best {
                Some(i) => {
                    used[i] = true;
                    let r = &raw[i];
                    (r.confidence, r.midi, r.start_s, r.end_s, r.key_snapped)
                }
                None => (0.0, n.pitch as f64, s, e, false),
            };
            out.push(TranscribedNote {
                start_s: r4(s),
                end_s: r4(e),
                duration_s: r4(e - s),
                start_beats: r4(n.tick as f64 / tpb),
                pitch: n.pitch,
                name: midi_to_name(n.pitch as i32),
                velocity: n.velocity,
                confidence: (conf * 1000.0).round() / 1000.0,
                detected_midi: (det * 100.0).round() / 100.0,
                detected_start_s: r4(ds),
                detected_end_s: r4(de),
                key_snapped: snapped,
            });
        }
    }
    out.sort_by(|a, b| a.start_s.total_cmp(&b.start_s).then(a.pitch.cmp(&b.pitch)));
    out
}

// -------------------------------------------------------------------------------------
// op
// -------------------------------------------------------------------------------------

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::custom("transcribe", Category::Midi, "Turn singing, humming, or an instrument into MIDI", transcribe_op)
        .about("Transcribes a recording to a MIDI file plus a JSON sidecar (<out>.notes.json) listing every note with its confidence. Mono mode (singing, humming, whistling, a monophonic instrument) tracks pitch every 10 ms, splits notes on pitch jumps over 80 cents or energy dips, takes each note's median pitch, maps peak loudness to velocity, merges notes shorter than --min-note, snaps to the detected key, and writes a tempo map from the detected (or given) tempo. Poly mode decodes Basic Pitch note activations (piano, guitar chords) and needs the model 'transcribe/basic-pitch'. Auto picks mono when the pitch tracker finds a single voiced line.")
        .inputs(Inputs::ONE_AUDIO)
        .param(param("mode", ParamKind::Choice).choices(&["mono", "poly", "auto"]).default("auto").help("mono: one note at a time (voice, whistle, bass); poly: chords via the Basic Pitch model; auto: mono when the pitch tracker reports a single voiced line"))
        .param(param("tracker", ParamKind::Choice).choices(&["pyin", "crepe"]).default("pyin").help("Pitch tracker for mono mode; crepe needs the model pitch/crepe"))
        .param(param("min-note", ParamKind::Time).default("60ms").help("Shortest note kept (60ms); shorter fragments merge into a neighbour or are dropped"))
        .param(param("onset-sensitivity", ParamKind::Percent).default("50%").range("0..100%").help("How readily repeated or sliding notes are split: 50% splits on 80-cent pitch jumps and 9 dB energy dips, 100% on 40 cents and 3 dB, 0% on 120 cents and 15 dB (poly: onset threshold 1 − sensitivity)"))
        .param(param("velocity", ParamKind::Str).default("from-rms").help("from-rms: each note's peak loudness through a loudness curve (loudest note 112, −6 dB 79, −20 dB 35); fixed:<1-127> gives every note the same velocity"))
        .param(param("bpm", ParamKind::Str).default("auto").help("Tempo of the written file in BPM: auto detects it from the note onsets and the recording (also set by the global --bpm)"))
        .param(param("quantize", ParamKind::Grid).default("off").help("Quantize note starts in the same run: a grid such as 1/16 or 1/8T, auto to detect the subdivision, or off"))
        .param(param("key-snap", ParamKind::Switch).default("on").help("When the detected key's confidence is above 0.6, move out-of-key notes onto the nearest key note (notes sung within 25 cents of a chromatic pitch are kept)"))
        .param(param("pitch-bend", ParamKind::Switch).default("off").help("Encode slides (portamento up to 2 semitones) and pitch drift as pitch bend (±2 semitone range) instead of discrete notes (mono mode)"))
        .param(param("octave", ParamKind::Str).default("auto").help("auto shifts by whole octaves so the median note lands in the comfortable range of --program (piano E2-C6, bass G1-D3, flute D4-G6); or a fixed shift in octaves (-1, +1, 0)"))
        .param(param("program", ParamKind::Int).default("0").range("0..127").help("General MIDI program (0-based) written into the track; also picks the --octave auto range (0 piano, 33 bass, 40 violin)"))
        .example("ferraus transcribe hum.wav -o part.mid", "Hummed melody to MIDI, with part.notes.json listing each note's confidence")
        .example("ferraus transcribe whistle.wav --bpm 96 --quantize 1/16 -o tune.mid", "Fixed tempo and sixteenth-note quantize in the same run")
        .example("ferraus transcribe bassline.wav --program 33 --velocity fixed:100 -o bass.mid", "A bass part: octave chosen for a bass, even velocities")
        .example("ferraus transcribe piano.wav --mode poly -o piano.mid", "Chords with the Basic Pitch model (ferraus models pull transcribe/basic-pitch)")]
}

/// Parse `transcribe` parameters (shared with `hum`).
pub fn options_from_args(a: &Args, ctx: &OpCtx) -> Result<TranscribeOptions> {
    let min_note_s = if a.has("min-note") { a.seconds("min-note")? } else { 0.060 };
    if !(0.0..=2.0).contains(&min_note_s) {
        return Err(Error::usage("--min-note must be between 0ms and 2s"));
    }
    let bpm = match a.raw("bpm") {
        None => ctx.bpm(),
        Some(s) if s.trim().eq_ignore_ascii_case("auto") => None,
        Some(s) => {
            let v: f64 = s
                .trim()
                .parse()
                .map_err(|_| Error::usage(format!("--bpm must be auto or a tempo in BPM, not '{s}'")))?;
            if !(20.0..=400.0).contains(&v) {
                return Err(Error::usage("--bpm must be between 20 and 400"));
            }
            Some(v)
        }
    };
    let quantize = match a.raw("quantize").map(|s| s.trim().to_ascii_lowercase()) {
        None => QuantizeSetting::Off,
        Some(s) if s == "off" => QuantizeSetting::Off,
        Some(s) if s == "auto" => QuantizeSetting::Auto,
        Some(_) => match a.grid("quantize")? {
            Some(g) => QuantizeSetting::Grid(g),
            None => QuantizeSetting::Off,
        },
    };
    let program = if a.has("program") { a.int("program")? } else { 0 };
    if !(0..=127).contains(&program) {
        return Err(Error::usage("--program must be 0-127"));
    }
    Ok(TranscribeOptions {
        mode: Mode::parse(&a.str_or("mode", "auto"))?,
        tracker: a.str_or("tracker", "pyin"),
        min_note_s,
        onset_sensitivity: if a.has("onset-sensitivity") { a.percent("onset-sensitivity")? } else { 0.5 },
        velocity: Velocity::parse(&a.str_or("velocity", "from-rms"))?,
        bpm,
        quantize,
        quantize_strength: 1.0,
        key_snap: a.switch("key-snap", true),
        pitch_bend: a.switch("pitch-bend", false),
        octave: Octave::parse(&a.str_or("octave", "auto"))?,
        program: program as u8,
        seed: ctx.seed(),
    })
}

fn wants_json(ctx: &OpCtx, path: &Path) -> bool {
    ctx.globals.format.as_deref() == Some("json")
        || path.extension().map(|e| e.eq_ignore_ascii_case("json")).unwrap_or(false)
}

/// Bytes of a MIDI file for `path` (the JSON model when the path ends in `.json`).
pub fn midi_bytes(ctx: &OpCtx, path: &Path, f: &MidiFile) -> Vec<u8> {
    if wants_json(ctx, path) {
        f.to_json_string().into_bytes()
    } else {
        f.to_smf_bytes()
    }
}

/// Record the run-time results of a transcription on the context.
pub fn record(ctx: &mut OpCtx, t: &Transcription) {
    for (c, m) in &t.warnings {
        ctx.warn(c, m.clone());
    }
    if let Some((n, v, s)) = &t.model {
        ctx.model_used(n, v, s);
    }
    ctx.note("mode", t.mode);
    ctx.note("bpm", format!("{} ({})", t.tempo.bpm, t.tempo.source));
    if let Some(k) = &t.key {
        let used = if k.snapped { "snapped" } else { "not snapped" };
        ctx.note("key", format!("{} (confidence {:.2}; {used})", k.key, k.confidence));
    }
    if t.octave_shift != 0 {
        ctx.note("octave", t.octave_shift);
    }
    ctx.metric("notes", t.notes.len());
    ctx.metric("tempo_confidence", t.tempo.confidence);
    if !t.notes.is_empty() {
        let mean = t.notes.iter().map(|n| n.confidence).sum::<f64>() / t.notes.len() as f64;
        ctx.metric("mean_note_confidence", (mean * 1000.0).round() / 1000.0);
    }
    if let Some(p) = &t.polyphony {
        ctx.metric("polyphonic_fraction", p.polyphonic_fraction);
    }
}

/// The result JSON shared by `transcribe` and `hum`.
pub fn summary(t: &Transcription) -> Value {
    let lo = t.notes.iter().map(|n| n.pitch).min();
    let hi = t.notes.iter().map(|n| n.pitch).max();
    let mean =
        if t.notes.is_empty() { 0.0 } else { t.notes.iter().map(|n| n.confidence).sum::<f64>() / t.notes.len() as f64 };
    json!({
        "mode": t.mode,
        "engine": t.engine,
        "notes": t.notes.len(),
        "tempo": t.tempo,
        "bpm": t.tempo.bpm,
        "key": t.key.as_ref().map(|k| k.key.clone()),
        "key_confidence": t.key.as_ref().map(|k| k.confidence),
        "key_snapped": t.key.as_ref().map(|k| k.snapped).unwrap_or(false),
        "notes_snapped": t.key.as_ref().map(|k| k.notes_snapped).unwrap_or(0),
        "octave_shift": t.octave_shift,
        "pitch_range": lo.zip(hi).map(|(l, h)| vec![midi_to_name(l as i32), midi_to_name(h as i32)]),
        "mean_confidence": (mean * 1000.0).round() / 1000.0,
        "pitch_bends": t.pitch_bends,
        "quantize": t.quantize,
        "polyphony": t.polyphony,
        "duration_s": (t.midi.duration_seconds() * 1e4).round() / 1e4,
    })
}

/// The notes sidecar JSON.
pub fn notes_json(t: &Transcription, source: &str) -> Value {
    json!({
        "ferraus_notes": 1,
        "source": source,
        "mode": t.mode,
        "engine": t.engine,
        "bpm": t.tempo.bpm,
        "first_beat_s": t.tempo.first_beat_s,
        "key": t.key.as_ref().map(|k| k.key.clone()),
        "key_confidence": t.key.as_ref().map(|k| k.confidence),
        "octave_shift": t.octave_shift,
        "notes": t.notes,
    })
}

fn notes_path(out: &Path) -> Option<PathBuf> {
    (out.as_os_str() != "-").then(|| out.with_extension("notes.json"))
}

fn transcribe_op(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let o = options_from_args(a, ctx)?;
    let ext = if ctx.globals.format.as_deref() == Some("json") { "json" } else { "mid" };
    let out = ctx.output_path(None, ext)?;
    let npath = notes_path(&out);
    if ctx.globals.dry_run {
        let input = ctx.input(0)?.to_path_buf();
        if input.as_os_str() != "-" && !input.exists() {
            return Err(Error::new(
                ErrorKind::InputMissing,
                "input_not_found",
                format!("input not found: {}", input.display()),
            ));
        }
        ctx.prepare_output(&out)?;
        ctx.outputs.push(OutputRecord {
            path: out.display().to_string(),
            role: None,
            duration_s: None,
            sample_rate: None,
            channels: None,
            extra: Default::default(),
        });
        if let Some(p) = &npath {
            ctx.prepare_output(p)?;
            ctx.record_output(p, Some("notes"));
        }
        return Ok(
            json!({"dry_run": true, "output": out.display().to_string(), "notes_output": npath.map(|p| p.display().to_string())}),
        );
    }
    let buf = ctx.read_audio(0)?;
    ctx.set_progress(0.05);
    let t = transcribe(&buf, &o)?;
    ctx.set_progress(0.9);
    record(ctx, &t);
    let bytes = midi_bytes(ctx, &out, &t.midi);
    ctx.write_file(&out, &bytes, None)?;
    if let Some(p) = &npath {
        let source = ctx.input(0)?.display().to_string();
        let text = serde_json::to_string_pretty(&notes_json(&t, &source))?;
        ctx.write_file(p, text.as_bytes(), Some("notes"))?;
    }
    let mut v = summary(&t);
    v["output"] = json!(out.display().to_string());
    v["notes_output"] = json!(npath.map(|p| p.display().to_string()));
    Ok(v)
}

/// Is a program name a GM program? (helper for `hum --to`)
pub fn program_for_instrument(name: &str) -> Option<u8> {
    gm::lookup_program(name).ok()
}
