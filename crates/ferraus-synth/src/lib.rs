//! # ferraus-synth
//!
//! MIDI rendering for ferraus: the `render` command, SoundFont playback through
//! `rustysynth` (MIT), a native SFZ sampler, and a built-in preview synth that needs no
//! files. Other crates use:
//!
//! * [`render()`] / [`render_with_report`] — `MidiFile` + [`InstrumentSource`] + [`RenderOptions`] → `AudioBuffer`.
//! * [`Resolver`] — instrument names (`piano`, `"String Ensemble 1"`, `gm:48`, aliases,
//!   scanned sounds, explicit sources) → [`InstrumentSource`].
//! * [`scan_sound_folder`] — SF2/SFZ files in a folder with inferred categories.
//! * [`Engine`] and [`set_plugin_engine_factory`] — how a plugin host plugs in instruments.

pub mod builtin;
pub mod ops;
pub mod render;
pub mod sf2;
pub mod sfz;
pub mod source;

pub use render::{
    render, render_with_report, set_plugin_engine_factory, set_plugin_offline_renderer, Engine, PluginEngineFactory,
    PluginOfflineRenderer, RenderOptions, RenderReport, Scheduled, Selector, VelocityCurve,
};
pub use source::{
    parse_source, read_aliases, scan_sound_folder, InstrumentSource, Resolved, Resolver, Sf2Preset, SoundEntry,
    SoundKind,
};

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    ops::ops()
}
