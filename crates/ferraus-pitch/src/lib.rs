//! # ferraus-pitch
//!
//! Pitch and time for ferraus, in pure Rust:
//!
//! * [`pyin`]: the pYIN pitch tracker (FFT-based YIN difference function, Beta threshold
//!   prior, HMM/Viterbi decoding) → per-frame f0, voicing confidence, voiced flag, RMS.
//! * [`tracker`]: the [`tracker::PitchTracker`] trait and `--tracker` selection; model
//!   trackers (CREPE, RMVPE, SwiftF0) plug in from `ferraus-ml`.
//! * [`notes`]: note segmentation from a pitch track (shared with `transcribe`).
//! * [`key`]: key/scale detection (chroma × Krumhansl-Schmuckler), key and scale parsing.
//! * [`tune`]: per-note targets and the correction curve used by `tune`.
//! * [`psola`]: TD-PSOLA pitch shifting with a time-varying ratio (formants preserved).
//! * [`vocoder`]: phase vocoder with identity phase locking and cepstral formant correction.
//! * [`wsola`]: WSOLA time stretching; [`stretch`]: the hybrid, transient-anchored stretcher.
//! * [`timemap`]: piecewise-linear time maps shared by the stretchers.
//!
//! Ops: `tune`, `pitch`, `stretch`, `analyze pitch`, `analyze key`.

// DSP loops index several buffers with the same counter; iterator chains read worse.
#![allow(clippy::needless_range_loop)]

pub mod key;
pub mod notes;
pub mod psola;
pub mod pyin;
pub mod stretch;
pub mod timemap;
pub mod tracker;
pub mod tune;
pub mod vocoder;
pub mod wsola;

mod ops;
mod util;

#[cfg(test)]
mod acceptance;
#[cfg(test)]
mod fixtures;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    ops::ops()
}
