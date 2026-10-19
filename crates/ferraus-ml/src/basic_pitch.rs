//! Basic Pitch (Spotify, ICASSP 2022) polyphonic note activations through the official
//! ONNX export (the CQT runs inside the graph).
//!
//! The model sees 2 s windows of mono 22 050 Hz audio (43 844 samples) and returns 172
//! frames (86 frames/s, hop 256) of note (88 keys from A0 = MIDI 21), onset (88 keys) and
//! contour (264 bins, 3 per semitone from A0) activations. Windowing follows the reference
//! `run_inference`: the signal is prefixed with half the overlap (3840 samples), cut into
//! windows every 36 164 samples (30 frames of overlap), half the overlap (15 frames) is
//! dropped from both ends of every window's output, the windows are concatenated and the
//! result is trimmed to `floor(len × 86 / 22050)` frames.

use crate::session::TensorF32;
use ferraus_core::{Error, OpCtx, Result};

pub const BASIC_PITCH_MODEL: &str = "transcribe/basic-pitch";
pub const BASIC_PITCH_RATE: u32 = 22_050;
pub const BASIC_PITCH_FPS: usize = 86;
pub const BASIC_PITCH_HOP: usize = 256;
/// Window length in samples (2 s minus one hop).
pub const BASIC_PITCH_WINDOW: usize = 43_844;
pub const BASIC_PITCH_FRAMES: usize = 172;
pub const BASIC_PITCH_OVERLAP_FRAMES: usize = 30;
pub const BASIC_PITCH_OVERLAP: usize = BASIC_PITCH_OVERLAP_FRAMES * BASIC_PITCH_HOP;
pub const BASIC_PITCH_KEYS: usize = 88;
pub const BASIC_PITCH_CONTOUR_BINS: usize = 264;
/// MIDI note of key index 0.
pub const BASIC_PITCH_MIDI_OFFSET: u8 = 21;

/// `[frame][bin]` activations in 0..1.
pub type Activations = Vec<Vec<f32>>;

/// Windows of model input for a mono 22 050 Hz signal (reference windowing).
pub fn basic_pitch_windows(mono: &[f32]) -> Vec<Vec<f32>> {
    let half = BASIC_PITCH_OVERLAP / 2;
    let hop = BASIC_PITCH_WINDOW - BASIC_PITCH_OVERLAP;
    let total = half + mono.len();
    let at = |i: usize| if i < half { 0.0 } else { mono.get(i - half).copied().unwrap_or(0.0) };
    (0..total).step_by(hop).map(|s| (s..s + BASIC_PITCH_WINDOW).map(at).collect()).collect()
}

/// Number of output frames for `len` input samples.
pub fn basic_pitch_frames(len: usize) -> usize {
    len * BASIC_PITCH_FPS / BASIC_PITCH_RATE as usize
}

/// Run Basic Pitch on a mono 22 050 Hz signal. Returns `(frames, onsets, contours)`:
/// note activations `[T][88]`, onset activations `[T][88]` and pitch-contour activations
/// `[T][264]`, with `T = floor(len × 86 / 22050)` and frame `t` at `t / 86` s. Applies the
/// model download policy of `ctx` (`transcribe/basic-pitch`) and records the model.
pub fn basic_pitch_activations(ctx: &mut OpCtx, mono_22050: &[f32]) -> Result<(Activations, Activations, Activations)> {
    let (session, _) = crate::pitch::model_session(ctx, BASIC_PITCH_MODEL)?;
    let input = session.input_names.first().cloned().unwrap_or_else(|| "serving_default_input_2:0".into());
    let n_out = basic_pitch_frames(mono_22050.len());
    let (mut frames, mut onsets, mut contours) = (Vec::new(), Vec::new(), Vec::new());
    if n_out == 0 {
        return Ok((frames, onsets, contours));
    }
    let windows = basic_pitch_windows(mono_22050);
    let trim = BASIC_PITCH_OVERLAP_FRAMES / 2;
    const BATCH: usize = 8;
    let total = windows.len();
    for (bi, batch) in windows.chunks(BATCH).enumerate() {
        ctx.check_cancel()?;
        let mut data = Vec::with_capacity(batch.len() * BASIC_PITCH_WINDOW);
        for w in batch {
            data.extend_from_slice(w);
        }
        let out =
            session.run(vec![TensorF32::new(&input, vec![batch.len() as i64, BASIC_PITCH_WINDOW as i64, 1], data)])?;
        // outputs are told apart by their bin count; note and onset (both 88) by name
        let find = |bins: usize, name_hint: &str| -> Result<&TensorF32> {
            let cands: Vec<&TensorF32> = out.iter().filter(|t| t.shape.last() == Some(&(bins as i64))).collect();
            match cands.as_slice() {
                [one] => Ok(one),
                many => many
                    .iter()
                    .find(|t| t.name.ends_with(name_hint))
                    .copied()
                    .ok_or_else(|| Error::processing(format!("Basic Pitch output with {bins} bins not found"))),
            }
        };
        let contour = find(BASIC_PITCH_CONTOUR_BINS, ":0")?;
        let note = find(BASIC_PITCH_KEYS, ":1")?;
        let onset = find(BASIC_PITCH_KEYS, ":2")?;
        for (t, dst) in [(note, &mut frames), (onset, &mut onsets), (contour, &mut contours)] {
            let bins = *t.shape.last().unwrap_or(&0) as usize;
            let per = BASIC_PITCH_FRAMES * bins;
            if t.shape.len() != 3 || t.shape[1] as usize != BASIC_PITCH_FRAMES || t.data.len() != batch.len() * per {
                return Err(Error::processing(format!("unexpected Basic Pitch output shape {:?}", t.shape)));
            }
            for w in 0..batch.len() {
                for f in trim..BASIC_PITCH_FRAMES - trim {
                    let off = w * per + f * bins;
                    dst.push(t.data[off..off + bins].to_vec());
                }
            }
        }
        ctx.set_progress(((bi + 1) * BATCH).min(total) as f64 / total as f64);
    }
    frames.truncate(n_out);
    onsets.truncate(n_out);
    contours.truncate(n_out);
    Ok((frames, onsets, contours))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowing_matches_the_reference() {
        assert_eq!(BASIC_PITCH_OVERLAP, 7680);
        let x: Vec<f32> = (0..100_000).map(|i| i as f32).collect();
        let w = basic_pitch_windows(&x);
        // ceil((3840 + 100000) / 36164) windows
        assert_eq!(w.len(), 3);
        assert!(w.iter().all(|w| w.len() == BASIC_PITCH_WINDOW));
        assert_eq!(w[0][3839], 0.0);
        assert_eq!(w[0][3840], 0.0); // first sample of x is 0.0
        assert_eq!(w[0][3841], 1.0);
        assert_eq!(w[1][0], (36_164 - 3840) as f32);
        assert_eq!(*w[2].last().unwrap(), 0.0); // zero padded at the end
                                                // each window contributes 142 frames; three windows cover the 390 frames needed
        assert_eq!(basic_pitch_frames(100_000), 390);
        assert!(w.len() * (BASIC_PITCH_FRAMES - BASIC_PITCH_OVERLAP_FRAMES) >= 390);
        assert!(basic_pitch_windows(&[]).len() <= 1);
    }
}
