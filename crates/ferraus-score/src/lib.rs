//! ferraus-score: scores and sheet music.
//!
//! * [`model`] — the agent-facing score model (`score to-json` / `from-json`).
//! * [`notate`] — MIDI → notation (quantize, voices, ties, key, spelling, drums, chords).
//! * Writers and readers: [`musicxml`], [`abc`], [`lily`], [`mei`], [`solfa`], [`midi`].
//! * [`perform`] — notation → MIDI performance (`score play`).
//! * [`check`] — validation and review; [`fix`] — edit lists.
//! * [`engrave`] — SVG/PDF/PNG through the `ferraus-engrave` helper (Verovio) or LilyPond.
//! * [`omr`] — `score read` engines; [`audio`] — audio → notation orchestration.

// `is_multiple_of` needs Rust 1.87; the workspace MSRV is 1.85.
#![allow(clippy::manual_is_multiple_of)]

pub mod abc;
pub mod audio;
pub mod audiveris;
pub mod check;
pub mod chords;
pub mod drums;
pub mod engrave;
pub mod fix;
pub mod formats;
pub mod lily;
pub mod mei;
pub mod midi;
pub mod model;
pub mod musicxml;
pub mod notate;
pub mod omr;
mod ops;
pub mod perform;
pub mod pitch;
pub mod solfa;

pub use model::Score;
pub use ops::{ops, transpose_score};

/// Download the Audiveris OMR engine into the ferraus tools cache.
pub fn install_audiveris(yes: bool, offline: bool) -> ferraus_core::Result<std::path::PathBuf> {
    audiveris::install(yes, offline)
}
