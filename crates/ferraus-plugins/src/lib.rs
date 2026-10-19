//! # ferraus-plugins
//!
//! Headless CLAP and VST3 plugin hosting for ferraus, always out of process, plus the
//! installable sounds registry.
//!
//! * [`protocol`] — the wire protocol spoken with `ferraus-plughost` (CLAP) and
//!   `ferraus-plughost-vst3` (VST3); see `PROTOCOL.md`.
//! * [`host_rt`] — the host-process side of the protocol, shared by both host binaries.
//!
//! With the default `ops` feature:
//!
//! * [`client`] — spawns a host process per plugin instance, with a watchdog.
//! * [`api`] — [`api::render_instrument`] and [`api::process_effect`], the entry points used
//!   by `ferraus-synth` and other crates.
//! * [`scan`] — plugin discovery and the scan cache; [`resolve`] — the sound resolver
//!   (aliases, installed sounds, `--sound-path` folders).
//! * [`sounds`] — the signed sounds registry and `sounds ...` commands.

pub mod host_rt;
pub mod protocol;

#[cfg(feature = "ops")]
pub mod api;
#[cfg(feature = "ops")]
pub mod client;
#[cfg(feature = "ops")]
mod ops;
#[cfg(feature = "ops")]
pub mod resolve;
#[cfg(feature = "ops")]
pub mod scan;
#[cfg(feature = "ops")]
pub mod sounds;

#[cfg(feature = "ops")]
pub use api::{
    process_effect, process_effect_with, render_instrument, render_instrument_with, MidiEvent,
    MidiEventKind, PluginFormat, PluginOptions, PluginRef, PluginRun,
};

/// Ops this crate contributes to the command registry.
#[cfg(feature = "ops")]
pub fn ops() -> Vec<ferraus_core::OpSpec> {
    ops::ops()
}
