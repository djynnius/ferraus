//! # ferraus-analysis
//!
//! Reusable audio analysis. Every public function takes an [`AudioBuffer`] and returns a
//! serde-serialisable struct, so other crates (mastering, stretch, delay sync,
//! transcription, scores) reuse exactly the numbers the `analyze` commands print.
//!
//! | Module | Main entry points |
//! | --- | --- |
//! | [`tempo`] | [`detect_tempo`] → [`TempoResult`]; [`tempo::onset_envelope`], [`tempo::transient_density`] |
//! | [`loudness`] | [`loudness()`] / [`loudness_with`] → [`LoudnessReport`]; [`true_peak`], [`loudness::integrated_lufs`], [`loudness::loudness_outliers`] |
//! | [`spectrum`] | [`spectrum::spectrum`], [`spectrum::third_octave_levels`], [`spectrum::tone_balance`], [`spectrum::tone_balance_vs_curve`] |
//! | [`stereo`] | [`stereo::stereo_analysis`] → [`stereo::StereoReport`]; [`stereo::correlation`], [`stereo::low_correlation`] |
//! | [`levels`] | [`levels::detect_silence`], [`levels::detect_clipping`], [`levels::stats`], [`levels::crest_factor_db`] |
//! | [`chords`] | [`detect_chords`] → [`ChordResult`]; [`chords::chromagram`] |
//! | [`compare`] | [`compare::compare`] → [`compare::CompareReport`]; [`compare::similarity`], [`compare::mfcc`] |
//! | [`problems`] | [`problems::diagnose`] → [`problems::ProblemsReport`] and the individual detectors |
//! | [`mastering`] | [`mastering::dynamics`], [`mastering::looks_mastered`], [`mastering::mastering_analysis`] |
//!
//! Loudness values that cannot be measured are `-inf` in Rust and `null` in JSON.

pub mod chords;
pub mod compare;
pub mod levels;
pub mod loudness;
pub mod mastering;
mod ops;
pub mod problems;
pub mod spectrum;
pub mod stereo;
pub mod tempo;
pub mod util;

pub use chords::{detect_chords, ChordOptions, ChordResult, ChordSegment};
pub use compare::{compare, similarity, CompareOptions, CompareReport, SimilarityReport};
pub use ferraus_core::AudioBuffer;
pub use levels::{detect_clipping, detect_silence, stats, ClippingReport, SilenceReport, StatsReport};
pub use loudness::{loudness, loudness_with, true_peak, LoudnessReport, LoudnessTimeline};
pub use mastering::{mastering_analysis, DynamicsReport, MasteredHeuristic, MasteringAnalysis};
pub use problems::{diagnose, Problem, ProblemsReport, Severity};
pub use spectrum::{BandLevel, Resolution, SpectrumReport, ToneBalance};
pub use stereo::{stereo_analysis, StereoReport};
pub use tempo::{detect_tempo, TempoOptions, TempoResult};

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry: `analyze tempo|loudness|silence|
/// spectrum|clipping|chords|similarity|problems|stats|stereo` and `compare`.
pub fn ops() -> Vec<OpSpec> {
    ops::ops()
}
