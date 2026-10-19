//! # ferraus-dsp
//!
//! Pure-Rust effects for ferraus, structured as a reusable DSP library plus the commands
//! built on it.
//!
//! **Library modules** (used by other crates such as mastering and restoration):
//!
//! * [`biquad`] — RBJ cookbook biquads, Butterworth cascades of any order, Linkwitz-Riley
//!   crossovers and a flat-summing multiband splitter.
//! * [`dynamics`] — envelope followers, the log-domain soft-knee compressor with lookahead,
//!   gate with hysteresis and hold, expander, true-peak lookahead limiter, clippers.
//! * [`delayline`] — ring-buffer delay lines with fractional reads; Schroeder all-passes.
//! * [`reverb`] — Dattorro plate, 8-line Hadamard FDN, IR convolution helpers.
//! * [`modulation`] — LFOs, chorus, flanger, phaser, vibrato, tremolo.
//! * [`saturation`] — oversampled tape/tube/soft/hard waveshaping.
//! * [`vocoder`] — channel and spectral vocoders, oscillators, pitch tracker, MIDI notes.
//! * [`pan`] — pan laws, balance, position names, sample-accurate keyframes.
//! * [`fir`] — linear-phase FIR design and partitioned FFT convolution.
//! * [`stft`] — STFT analysis/synthesis, Welch spectra, fractional-octave smoothing.
//! * [`oversample`] — polyphase oversampling and true-peak measurement.
//! * [`varispeed`] — arbitrary-ratio windowed-sinc resampling (tape-style speed change).
//! * [`util`] — dB helpers, smoothing coefficients, denormal flushing, noise, DC blocker.
//!
//! Every recursive primitive flushes its state to zero below 1e-20 (f32) / 1e-30 (f64), so
//! long silences never produce denormals.
//!
//! **Commands**: [`ops()`] returns every effect as an `OpSpec::transform`, usable as a CLI
//! verb, an `fx` stage, and a pipeline stage.

#![allow(clippy::needless_range_loop)] // DSP loops index several buffers in lockstep

pub mod biquad;
pub mod delayline;
pub mod dynamics;
pub mod fir;
pub mod modulation;
pub mod oversample;
pub mod pan;
pub mod reverb;
pub mod saturation;
pub mod stft;
pub mod util;
pub mod varispeed;
pub mod vocoder;

mod ops;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    ops::all()
}
