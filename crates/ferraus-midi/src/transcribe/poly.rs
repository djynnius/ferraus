//! Polyphonic transcription seam and Basic Pitch note decoding.
//!
//! Basic Pitch (Spotify, Apache-2.0) is a small CNN that maps audio at 22 050 Hz to three
//! activation matrices at a 256-sample hop (≈ 86.13 frames per second):
//!
//! * `notes` (a.k.a. frames): n_frames × 88, probability that MIDI pitch `21 + bin` sounds;
//! * `onsets`: n_frames × 88, probability that a note of that pitch starts;
//! * `contours`: n_frames × 264, a 3-bins-per-semitone pitch salience from A0 (27.5 Hz).
//!
//! The model itself runs on ONNX Runtime in `ferraus-ml`, which implements
//! [`PolyTranscriber`] (returning the raw matrices for a whole clip, already unwrapped from
//! the model's overlapping 2-second windows) and registers a factory with
//! [`register_poly_transcriber`]. Everything after the network lives here, ported from
//! basic-pitch's `note_creation.py` (`output_to_notes_polyphonic`, `get_infered_onsets`,
//! `get_pitch_bends`, `model_frames_to_time`), so [`output_to_notes`] with
//! [`BasicPitchOptions::default`] reproduces the reference note list from the same
//! activations.

use ferraus_core::{Error, ErrorKind, Result};
use std::sync::{Mutex, OnceLock};

/// Sample rate Basic Pitch runs at.
pub const SAMPLE_RATE: u32 = 22050;
/// Hop between activation frames (samples at [`SAMPLE_RATE`]).
pub const FFT_HOP: usize = 256;
/// Number of note/onset bins (MIDI 21..=108).
pub const N_PITCH_BINS: usize = 88;
/// MIDI note of bin 0.
pub const MIDI_OFFSET: u8 = 21;
/// Contour bins per semitone.
pub const CONTOUR_BINS_PER_SEMITONE: usize = 3;
/// Number of contour bins.
pub const N_CONTOUR_BINS: usize = 264;
/// Frequency of contour bin 0 (A0).
pub const CONTOUR_BASE_HZ: f64 = 27.5;
/// Model id in the ferraus model registry.
pub const MODEL_NAME: &str = "transcribe/basic-pitch";

const ANNOTATIONS_FPS: usize = 86; // int(22050 / 256)
const AUDIO_WINDOW_LENGTH_S: usize = 2;
const ANNOT_N_FRAMES: usize = ANNOTATIONS_FPS * AUDIO_WINDOW_LENGTH_S; // 172
const AUDIO_N_SAMPLES: usize = SAMPLE_RATE as usize * AUDIO_WINDOW_LENGTH_S - FFT_HOP; // 43844

/// Activation frames per second (22050 / 256).
pub fn frames_per_second() -> f64 {
    SAMPLE_RATE as f64 / FFT_HOP as f64
}

/// A dense row-major matrix of activations: `rows` frames × `cols` bins.
#[derive(Clone, Debug, PartialEq)]
pub struct Activation {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f32>,
}

impl Activation {
    pub fn zeros(rows: usize, cols: usize) -> Activation {
        Activation { rows, cols, data: vec![0.0; rows * cols] }
    }

    /// Build from rows (every row must have the same length).
    pub fn from_rows(rows: &[Vec<f32>]) -> Result<Activation> {
        let cols = rows.first().map(|r| r.len()).unwrap_or(0);
        if rows.iter().any(|r| r.len() != cols) {
            return Err(Error::processing("activation rows have different lengths"));
        }
        Ok(Activation { rows: rows.len(), cols, data: rows.concat() })
    }

    #[inline]
    pub fn at(&self, t: usize, b: usize) -> f32 {
        self.data[t * self.cols + b]
    }

    #[inline]
    pub fn set(&mut self, t: usize, b: usize, v: f32) {
        self.data[t * self.cols + b] = v;
    }

    fn max(&self) -> f32 {
        self.data.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
    }
}

/// Raw Basic Pitch outputs for one clip.
#[derive(Clone, Debug, PartialEq)]
pub struct PolyActivations {
    /// Note activations, n_frames × 88.
    pub notes: Activation,
    /// Onset activations, n_frames × 88.
    pub onsets: Activation,
    /// Contour activations, n_frames × 264 (`None` disables pitch-bend estimation).
    pub contours: Option<Activation>,
    /// True when the frames come from basic-pitch's windowed inference (2 s windows with
    /// 30 overlapping frames, unwrapped): frame times then get the reference per-window
    /// drift correction of `model_frames_to_time`. False means frame `k` is at
    /// `k × 256 / 22050` seconds.
    pub windowed: bool,
}

impl PolyActivations {
    pub fn n_frames(&self) -> usize {
        self.notes.rows
    }

    fn validate(&self) -> Result<()> {
        if self.notes.cols != N_PITCH_BINS || self.onsets.cols != N_PITCH_BINS {
            return Err(Error::processing(format!(
                "poly transcriber returned {} note bins and {} onset bins; expected {N_PITCH_BINS}",
                self.notes.cols, self.onsets.cols
            )));
        }
        if self.onsets.rows != self.notes.rows {
            return Err(Error::processing("poly transcriber returned note and onset matrices of different lengths"));
        }
        if let Some(c) = &self.contours {
            if c.cols != N_CONTOUR_BINS || c.rows != self.notes.rows {
                return Err(Error::processing(format!(
                    "poly transcriber returned a {}×{} contour matrix; expected {}×{N_CONTOUR_BINS}",
                    c.rows, c.cols, self.notes.rows
                )));
            }
        }
        Ok(())
    }
}

/// A polyphonic note transcriber that supplies Basic Pitch-layout activations.
pub trait PolyTranscriber: Send + Sync {
    /// Short name (`basic-pitch`).
    fn name(&self) -> &str;
    /// Model identity for provenance (name, version, sha256).
    fn model(&self) -> Option<(String, String, String)> {
        None
    }
    /// Run the model on a mono signal at `sr` Hz (implementations resample to 22 050 Hz)
    /// and return the activations for the whole clip.
    fn activations(&self, samples: &[f32], sr: u32) -> Result<PolyActivations>;
}

/// Factory for the polyphonic transcriber; returns `ModelNotCached` when weights are missing.
pub type PolyTranscriberFactory = fn() -> Result<Box<dyn PolyTranscriber>>;

fn registry() -> &'static Mutex<Option<PolyTranscriberFactory>> {
    static R: OnceLock<Mutex<Option<PolyTranscriberFactory>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(None))
}

/// Register (or replace) the polyphonic transcriber factory. Called by `ferraus-ml`.
pub fn register_poly_transcriber(factory: PolyTranscriberFactory) {
    *registry().lock().unwrap_or_else(|e| e.into_inner()) = Some(factory);
}

/// Is a polyphonic transcriber registered (its model may still be missing)?
pub fn poly_transcriber_registered() -> bool {
    registry().lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// The error for a missing Basic Pitch model.
pub fn model_not_cached() -> Error {
    Error::new(
        ErrorKind::ModelNotCached,
        "model_not_cached",
        format!("polyphonic transcription needs the Basic Pitch model {MODEL_NAME}, which is not installed"),
    )
    .with_hint(format!("ferraus models pull {MODEL_NAME}"))
}

/// Get the registered transcriber, or `ModelNotCached`.
pub fn poly_transcriber() -> Result<Box<dyn PolyTranscriber>> {
    let f = *registry().lock().unwrap_or_else(|e| e.into_inner());
    match f {
        Some(f) => f(),
        None => Err(model_not_cached()),
    }
}

/// Note decoding settings. `Default` matches basic-pitch's `predict()` defaults.
#[derive(Clone, Debug)]
pub struct BasicPitchOptions {
    /// Minimum onset activation for a note to start (0.5).
    pub onset_thresh: f32,
    /// Minimum note activation for a note to continue (0.3).
    pub frame_thresh: f32,
    /// Notes must be longer than this many frames (11 ≈ 127.7 ms).
    pub min_note_len: usize,
    /// Frames below threshold tolerated inside a note (11).
    pub energy_tol: usize,
    /// Add onsets inferred from rising note activations (true).
    pub infer_onsets: bool,
    /// Recover onset-less notes from remaining energy (true).
    pub melodia_trick: bool,
    /// Ignore pitches below / above these frequencies (Hz).
    pub min_freq: Option<f64>,
    pub max_freq: Option<f64>,
    /// Estimate per-frame pitch bends from the contour matrix (true).
    pub include_pitch_bends: bool,
}

impl Default for BasicPitchOptions {
    fn default() -> Self {
        BasicPitchOptions {
            onset_thresh: 0.5,
            frame_thresh: 0.3,
            min_note_len: 11,
            energy_tol: 11,
            infer_onsets: true,
            melodia_trick: true,
            min_freq: None,
            max_freq: None,
            include_pitch_bends: true,
        }
    }
}

/// One decoded note in frames.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameNote {
    /// First frame (inclusive).
    pub start: usize,
    /// End frame (exclusive).
    pub end: usize,
    /// MIDI pitch.
    pub pitch: u8,
    /// Mean note activation over the note (0..1).
    pub amplitude: f32,
    /// Per-frame pitch bend in 1/3 semitones (one per frame `start..end`), if estimated.
    pub bends: Option<Vec<i32>>,
}

/// A decoded note in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct PolyNote {
    pub start_s: f64,
    pub end_s: f64,
    pub pitch: u8,
    pub amplitude: f32,
    /// (time_s, semitones) bend points, if estimated.
    pub bends: Option<Vec<(f64, f64)>>,
}

fn hz_to_midi(hz: f64) -> f64 {
    12.0 * (hz / 440.0).log2() + 69.0
}

/// Zero activations outside `[min_freq, max_freq]` (basic-pitch `constrain_frequency`).
fn constrain_frequency(onsets: &mut Activation, frames: &mut Activation, max_freq: Option<f64>, min_freq: Option<f64>) {
    if let Some(f) = max_freq {
        let max_idx = (hz_to_midi(f).round() as i64 - MIDI_OFFSET as i64).max(0) as usize;
        for t in 0..frames.rows {
            for b in max_idx.min(N_PITCH_BINS)..N_PITCH_BINS {
                onsets.set(t, b, 0.0);
                frames.set(t, b, 0.0);
            }
        }
    }
    if let Some(f) = min_freq {
        let min_idx = (hz_to_midi(f).round() as i64 - MIDI_OFFSET as i64).max(0) as usize;
        for t in 0..frames.rows {
            for b in 0..min_idx.min(N_PITCH_BINS) {
                onsets.set(t, b, 0.0);
                frames.set(t, b, 0.0);
            }
        }
    }
}

/// Onsets inferred from note activations: the max of the predicted onsets and the
/// (rescaled) minimum of the 1- and 2-frame rises of the note activations.
pub fn inferred_onsets(onsets: &Activation, frames: &Activation, n_diff: usize) -> Activation {
    let (rows, cols) = (frames.rows, frames.cols);
    let mut diff = Activation::zeros(rows, cols);
    for t in 0..rows {
        for b in 0..cols {
            let mut m = f32::INFINITY;
            for n in 1..=n_diff {
                let prev = if t >= n { frames.at(t - n, b) } else { 0.0 };
                m = m.min(frames.at(t, b) - prev);
            }
            diff.set(t, b, if t < n_diff { 0.0 } else { m.max(0.0) });
        }
    }
    let dmax = diff.max();
    let omax = onsets.max();
    let mut out = onsets.clone();
    if dmax > 0.0 {
        for (o, d) in out.data.iter_mut().zip(&diff.data) {
            *o = o.max(omax * d / dmax);
        }
    }
    out
}

/// Decode notes from note and onset activations (basic-pitch `output_to_notes_polyphonic`).
pub fn output_to_notes_polyphonic(frames: &Activation, onsets: &Activation, o: &BasicPitchOptions) -> Vec<FrameNote> {
    let n_frames = frames.rows;
    let n_bins = frames.cols;
    if n_frames == 0 || n_bins == 0 {
        return Vec::new();
    }
    let max_freq_idx = n_bins - 1;
    let mut frames = frames.clone();
    let mut onsets = onsets.clone();
    constrain_frequency(&mut onsets, &mut frames, o.max_freq, o.min_freq);
    if o.infer_onsets {
        onsets = inferred_onsets(&onsets, &frames, 2);
    }
    // strict local maxima along time (scipy.signal.argrelmax, axis 0; edges excluded)
    let mut onset_idx: Vec<(usize, usize)> = Vec::new();
    for t in 1..n_frames.saturating_sub(1) {
        for b in 0..n_bins {
            let v = onsets.at(t, b);
            if v > onsets.at(t - 1, b) && v > onsets.at(t + 1, b) && v >= o.onset_thresh {
                onset_idx.push((t, b));
            }
        }
    }
    onset_idx.reverse(); // backwards in time

    let mut remaining = frames.clone();
    let mut notes = Vec::new();
    let tol = o.energy_tol;
    let thr = o.frame_thresh;
    for (start, b) in onset_idx {
        if start >= n_frames - 1 {
            continue;
        }
        let mut i = start + 1;
        let mut k = 0;
        while i < n_frames - 1 && k < tol {
            if remaining.at(i, b) < thr {
                k += 1;
            } else {
                k = 0;
            }
            i += 1;
        }
        i -= k;
        if i - start <= o.min_note_len {
            continue;
        }
        for t in start..i {
            remaining.set(t, b, 0.0);
            if b < max_freq_idx {
                remaining.set(t, b + 1, 0.0);
            }
            if b > 0 {
                remaining.set(t, b - 1, 0.0);
            }
        }
        let amp = (start..i).map(|t| frames.at(t, b)).sum::<f32>() / (i - start) as f32;
        notes.push(FrameNote { start, end: i, pitch: b as u8 + MIDI_OFFSET, amplitude: amp, bends: None });
    }

    if o.melodia_trick {
        loop {
            // argmax in row-major order (first occurrence of the maximum)
            let (mut best, mut bi) = (f32::NEG_INFINITY, 0usize);
            for (idx, v) in remaining.data.iter().enumerate() {
                if *v > best {
                    best = *v;
                    bi = idx;
                }
            }
            if best <= thr {
                break;
            }
            let (mid, b) = (bi / n_bins, bi % n_bins);
            remaining.set(mid, b, 0.0);
            let clear = |rem: &mut Activation, t: usize| {
                rem.set(t, b, 0.0);
                if b < max_freq_idx {
                    rem.set(t, b + 1, 0.0);
                }
                if b > 0 {
                    rem.set(t, b - 1, 0.0);
                }
            };
            // forward
            let mut i = mid + 1;
            let mut k = 0;
            while i < n_frames - 1 && k < tol {
                if remaining.at(i, b) < thr {
                    k += 1;
                } else {
                    k = 0;
                }
                clear(&mut remaining, i);
                i += 1;
            }
            let i_end = i as i64 - 1 - k as i64;
            // backward
            let mut i = mid as i64 - 1;
            let mut k = 0;
            while i > 0 && k < tol {
                if remaining.at(i as usize, b) < thr {
                    k += 1;
                } else {
                    k = 0;
                }
                clear(&mut remaining, i as usize);
                i -= 1;
            }
            let i_start = i + 1 + k as i64;
            if i_end - i_start <= o.min_note_len as i64 || i_start < 0 {
                continue;
            }
            let (s, e) = (i_start as usize, i_end as usize);
            let amp = (s..e).map(|t| frames.at(t, b)).sum::<f32>() / (e - s) as f32;
            notes.push(FrameNote { start: s, end: e, pitch: b as u8 + MIDI_OFFSET, amplitude: amp, bends: None });
        }
    }
    notes
}

/// Contour bin (fractional) of a MIDI pitch.
pub fn midi_pitch_to_contour_bin(pitch: f64) -> f64 {
    let hz = 440.0 * 2f64.powf((pitch - 69.0) / 12.0);
    12.0 * CONTOUR_BINS_PER_SEMITONE as f64 * (hz / CONTOUR_BASE_HZ).log2()
}

/// Per-frame pitch bends (1/3 semitones) from the contour matrix (basic-pitch `get_pitch_bends`).
pub fn add_pitch_bends(contours: &Activation, notes: &mut [FrameNote], n_bins_tolerance: usize) {
    let window_length = n_bins_tolerance * 2 + 1;
    // scipy.signal.windows.gaussian(window_length, std=5)
    let gauss: Vec<f32> = (0..window_length)
        .map(|n| {
            let x = n as f64 - (window_length as f64 - 1.0) / 2.0;
            (-0.5 * (x / 5.0).powi(2)).exp() as f32
        })
        .collect();
    let nb = N_CONTOUR_BINS.min(contours.cols) as i64;
    let tol = n_bins_tolerance as i64;
    for n in notes.iter_mut() {
        let freq_idx = midi_pitch_to_contour_bin(n.pitch as f64).round() as i64;
        let f_start = (freq_idx - tol).max(0);
        let f_end = nb.min(freq_idx + tol + 1);
        let g_start = (tol - freq_idx).max(0);
        let pb_shift = tol - (tol - freq_idx).max(0);
        let mut bends = Vec::with_capacity(n.end.saturating_sub(n.start));
        for t in n.start..n.end.min(contours.rows) {
            let (mut best, mut arg) = (f32::NEG_INFINITY, 0i64);
            for (j, f) in (f_start..f_end).enumerate() {
                let v = contours.at(t, f as usize) * gauss[(g_start as usize) + j];
                if v > best {
                    best = v;
                    arg = j as i64;
                }
            }
            bends.push((arg - pb_shift) as i32);
        }
        n.bends = Some(bends);
    }
}

/// Time (seconds) of every activation frame (basic-pitch `model_frames_to_time` when
/// `windowed`, otherwise `k × hop / sr`).
pub fn model_frames_to_time(n_frames: usize, windowed: bool) -> Vec<f64> {
    let hop_s = FFT_HOP as f64 / SAMPLE_RATE as f64;
    let window_offset = hop_s * (ANNOT_N_FRAMES as f64 - AUDIO_N_SAMPLES as f64 / FFT_HOP as f64) + 0.0018;
    (0..n_frames)
        .map(|k| {
            let t = k as f64 * hop_s;
            if windowed {
                t - window_offset * (k / ANNOT_N_FRAMES) as f64
            } else {
                t
            }
        })
        .collect()
}

/// Full decoding: activations → notes in seconds, sorted by start time then pitch.
pub fn output_to_notes(act: &PolyActivations, o: &BasicPitchOptions) -> Result<Vec<PolyNote>> {
    act.validate()?;
    let mut notes = output_to_notes_polyphonic(&act.notes, &act.onsets, o);
    if o.include_pitch_bends {
        if let Some(c) = &act.contours {
            add_pitch_bends(c, &mut notes, 25);
        }
    }
    let n = act.n_frames();
    // one extra frame so `end == n_frames` still has a time
    let times = model_frames_to_time(n + 1, act.windowed);
    let mut out: Vec<PolyNote> = notes
        .into_iter()
        .map(|fnote| {
            let bends = fnote.bends.as_ref().map(|b| {
                b.iter().enumerate().map(|(i, v)| (times[fnote.start + i], *v as f64 / 3.0)).collect::<Vec<_>>()
            });
            PolyNote {
                start_s: times[fnote.start],
                end_s: times[fnote.end.min(n)],
                pitch: fnote.pitch,
                amplitude: fnote.amplitude,
                bends,
            }
        })
        .collect();
    out.sort_by(|a, b| a.start_s.total_cmp(&b.start_s).then(a.pitch.cmp(&b.pitch)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(n: usize) -> PolyActivations {
        PolyActivations {
            notes: Activation::zeros(n, N_PITCH_BINS),
            onsets: Activation::zeros(n, N_PITCH_BINS),
            contours: None,
            windowed: false,
        }
    }

    fn bin(m: u8) -> usize {
        (m - MIDI_OFFSET) as usize
    }

    fn put_note(a: &mut PolyActivations, pitch: u8, s: usize, e: usize, level: f32, onset: Option<f32>) {
        for t in s..e {
            a.notes.set(t, bin(pitch), level);
        }
        if let Some(o) = onset {
            a.onsets.set(s, bin(pitch), o);
            if s > 0 {
                a.onsets.set(s - 1, bin(pitch), o * 0.4);
            }
            a.onsets.set(s + 1, bin(pitch), o * 0.4);
        }
    }

    fn no_infer() -> BasicPitchOptions {
        BasicPitchOptions { infer_onsets: false, melodia_trick: false, ..Default::default() }
    }

    #[test]
    fn single_note_from_onset_and_frames() {
        let mut a = act(100);
        put_note(&mut a, 60, 10, 40, 0.8, Some(0.9));
        let notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &no_infer());
        assert_eq!(notes.len(), 1, "{notes:?}");
        let n = &notes[0];
        assert_eq!((n.start, n.end, n.pitch), (10, 40, 60));
        assert!((n.amplitude - 0.8).abs() < 1e-6);
    }

    #[test]
    fn thresholds_and_minimum_length() {
        let mut a = act(100);
        // onset too weak
        put_note(&mut a, 62, 10, 40, 0.8, Some(0.45));
        // frames too weak to sustain
        put_note(&mut a, 64, 10, 40, 0.25, Some(0.9));
        // too short: 11 frames is not longer than min_note_len = 11
        put_note(&mut a, 67, 50, 61, 0.8, Some(0.9));
        // 12 frames passes
        put_note(&mut a, 69, 70, 82, 0.8, Some(0.9));
        let notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &no_infer());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!((notes[0].pitch, notes[0].start, notes[0].end), (69, 70, 82));
    }

    #[test]
    fn chord_and_repeated_note() {
        let mut a = act(200);
        for p in [60, 64, 67] {
            put_note(&mut a, p, 20, 80, 0.7, Some(0.8));
        }
        // a repeated note: continuous frames with a second onset at 140
        put_note(&mut a, 72, 100, 180, 0.6, Some(0.9));
        a.onsets.set(140, bin(72), 0.9);
        a.onsets.set(139, bin(72), 0.3);
        a.onsets.set(141, bin(72), 0.3);
        let mut notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &no_infer());
        notes.sort_by_key(|n| (n.start, n.pitch));
        let got: Vec<(usize, usize, u8)> = notes.iter().map(|n| (n.start, n.end, n.pitch)).collect();
        assert_eq!(got, vec![(20, 80, 60), (20, 80, 64), (20, 80, 67), (100, 140, 72), (140, 180, 72)]);
    }

    #[test]
    fn energy_tolerance_bridges_short_dropouts() {
        let mut a = act(120);
        put_note(&mut a, 60, 10, 30, 0.8, Some(0.9));
        // 5 frames below threshold, then the note continues
        put_note(&mut a, 60, 35, 60, 0.8, None);
        let notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &no_infer());
        assert_eq!(notes.len(), 1);
        assert_eq!((notes[0].start, notes[0].end), (10, 60));
        // a gap longer than energy_tol ends the note
        let mut a = act(120);
        put_note(&mut a, 60, 10, 30, 0.8, Some(0.9));
        put_note(&mut a, 60, 45, 70, 0.8, None);
        let notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &no_infer());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].end, 30);
    }

    #[test]
    fn inferred_onsets_find_notes_without_onset_activations() {
        let mut a = act(100);
        put_note(&mut a, 60, 20, 60, 0.8, None);
        // another note with a real onset sets the rescaling maximum
        put_note(&mut a, 72, 10, 50, 0.8, Some(0.9));
        let o = BasicPitchOptions { melodia_trick: false, ..Default::default() };
        let mut notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &o);
        notes.sort_by_key(|n| n.pitch);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!((notes[0].pitch, notes[0].start, notes[0].end), (60, 20, 60));
        // without inference only the onset-driven note remains
        assert_eq!(output_to_notes_polyphonic(&a.notes, &a.onsets, &no_infer()).len(), 1);
    }

    #[test]
    fn melodia_trick_recovers_onsetless_energy() {
        let mut a = act(100);
        // ramps in gently (no rise strong enough for onset inference to matter since disabled)
        for t in 20..70 {
            a.notes.set(t, bin(65), 0.6);
        }
        a.notes.set(45, bin(65), 0.9);
        let o = BasicPitchOptions { infer_onsets: false, ..Default::default() };
        let notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &o);
        assert_eq!(notes.len(), 1, "{notes:?}");
        // the reference forward pass ends one frame early (i_end = i - 1 - k)
        assert_eq!((notes[0].pitch, notes[0].start, notes[0].end), (65, 20, 69));
        // short blobs are ignored
        let mut a = act(100);
        for t in 20..28 {
            a.notes.set(t, bin(65), 0.6);
        }
        assert!(output_to_notes_polyphonic(&a.notes, &a.onsets, &o).is_empty());
    }

    #[test]
    fn neighbouring_bins_are_consumed() {
        // leakage into the semitone above must not become a second note via melodia
        let mut a = act(100);
        put_note(&mut a, 60, 10, 50, 0.8, Some(0.9));
        for t in 10..50 {
            a.notes.set(t, bin(61), 0.5);
        }
        let o = BasicPitchOptions { infer_onsets: false, ..Default::default() };
        let notes = output_to_notes_polyphonic(&a.notes, &a.onsets, &o);
        assert_eq!(notes.len(), 1, "{notes:?}");
    }

    #[test]
    fn frequency_limits() {
        let mut a = act(100);
        put_note(&mut a, 40, 10, 50, 0.8, Some(0.9));
        put_note(&mut a, 90, 10, 50, 0.8, Some(0.9));
        let o = BasicPitchOptions { min_freq: Some(100.0), max_freq: Some(1000.0), ..no_infer() };
        assert!(output_to_notes_polyphonic(&a.notes, &a.onsets, &o).is_empty());
        let o = BasicPitchOptions { min_freq: Some(60.0), ..no_infer() };
        assert_eq!(output_to_notes_polyphonic(&a.notes, &a.onsets, &o).len(), 2);
    }

    #[test]
    fn pitch_bends_from_contours() {
        let mut a = act(60);
        put_note(&mut a, 69, 10, 40, 0.8, Some(0.9));
        let mut c = Activation::zeros(60, N_CONTOUR_BINS);
        let centre = midi_pitch_to_contour_bin(69.0).round() as usize;
        assert_eq!(centre, 3 * (69 - 21));
        for t in 10..40 {
            // first half in tune, second half a semitone (3 bins) sharp
            let b = if t < 25 { centre } else { centre + 3 };
            c.set(t, b, 0.9);
        }
        a.contours = Some(c);
        let notes = output_to_notes(&a, &no_infer()).unwrap();
        let bends = notes[0].bends.as_ref().unwrap();
        assert_eq!(bends.len(), 30);
        assert!(bends[..15].iter().all(|b| b.1 == 0.0));
        assert!(bends[15..].iter().all(|b| (b.1 - 1.0).abs() < 1e-9), "{bends:?}");
    }

    #[test]
    fn frame_times_follow_reference() {
        let t = model_frames_to_time(400, false);
        assert!((t[86] - 86.0 * 256.0 / 22050.0).abs() < 1e-12);
        let w = model_frames_to_time(400, true);
        let offset = 256.0 / 22050.0 * (172.0 - 43844.0 / 256.0) + 0.0018;
        assert_eq!(w[171], t[171]);
        assert!((w[172] - (t[172] - offset)).abs() < 1e-12);
        assert!((w[344] - (t[344] - 2.0 * offset)).abs() < 1e-12);
        assert!((offset - 0.010322).abs() < 1e-5, "{offset}");
    }

    #[test]
    fn seam_reports_missing_model() {
        // no transcriber is registered in the unit-test binary
        let e = poly_transcriber().err().unwrap();
        assert_eq!(e.kind, ErrorKind::ModelNotCached);
        assert_eq!(e.hint.as_deref(), Some("ferraus models pull transcribe/basic-pitch"));
    }

    #[test]
    fn validates_shapes() {
        let mut a = act(10);
        a.onsets = Activation::zeros(10, 12);
        assert!(output_to_notes(&a, &BasicPitchOptions::default()).is_err());
        assert!(Activation::from_rows(&[vec![0.0; 3], vec![0.0; 2]]).is_err());
    }
}
