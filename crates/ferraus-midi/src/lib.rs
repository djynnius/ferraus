//! # ferraus-midi
//!
//! MIDI for ferraus: a strict Standard MIDI File codec ([`smf`]), a lossless in-memory
//! model with an agent-facing JSON form ([`model`]), tempo-map conversions ([`tempo`]),
//! editing algorithms ([`edit`]: quantize, groove, transpose, velocity, legato, tempo,
//! split, merge, trim), General MIDI tables ([`gm`]), and the `midi ...` commands.
//!
//! Helpers for other crates (transcription, hum-to-MIDI, scores, rendering):
//!
//! * [`MidiFile::from_notes`] — build a file from notes in seconds at a tempo.
//! * [`MidiFile::notes_seconds`] — read every note back in seconds.
//! * [`MidiFile::tempo_map`] → [`TempoMap`] — ticks ↔ seconds ↔ beats.
//! * [`MidiFile::read`] / [`MidiFile::to_smf_bytes`] / [`MidiFile::to_json_string`].

pub mod edit;
pub mod gm;
pub mod model;
pub mod ops;
pub mod smf;
pub mod tempo;

pub use edit::{Groove, QuantizeOptions, QuantizeWhat};
pub use model::{MidiFile, Note, SecondsNote, Timed, Track};
pub use smf::Timing;
pub use tempo::TempoMap;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    ops::ops()
}
