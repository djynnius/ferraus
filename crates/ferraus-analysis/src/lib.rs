//! # ferraus-analysis
//!
//! Reusable audio analysis: every public function takes an [`AudioBuffer`] and returns a
//! serde-serialisable struct, so other crates (mastering, stretch, delay sync, scores)
//! reuse the same numbers the `analyze` commands print.

pub mod loudness;
pub mod tempo;
pub mod util;

pub use ferraus_core::AudioBuffer;
pub use tempo::{detect_tempo, TempoOptions, TempoResult};
pub use loudness::{loudness, loudness_with, true_peak, LoudnessReport, LoudnessTimeline};

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}
