//! # ferraus-master
//!
//! `ferraus master`: turn a finished mix into a release-ready master in one command, where
//! every stage is also an ordinary ferraus command, so an agent can accept the automatic
//! result or run its own analyze → plan → edit → run → verify loop.
//!
//! | Command | Module |
//! | --- | --- |
//! | `master <mix>` | [`chain`] (planner that simulates the chain), [`plan`] (editable plans) |
//! | `master analyze` | [`analyze`] |
//! | `master plan` | [`chain`], [`plan`] |
//! | `master verify` | [`verify`] |
//! | `master deliver` | [`deliver`] |
//!
//! Data in the binary: [`targets::TARGETS`] (the PRD delivery table) and
//! [`genres::GENRES`] (tonal curves and crest targets — hand-designed starting points, see
//! the provenance note in [`genres`]).
//!
//! The chain composes registered ops at run time (`dc-remove`, `silence trim`, `eq`,
//! `highpass`, `multiband`, `compress`, `widen`, `saturate`, `gain`, `limit`, and when
//! present `declip`, `leveler`, `separate`), so the registry must contain ferraus-dsp and
//! ferraus-edit.

#![allow(clippy::needless_range_loop)]

pub mod analyze;
pub mod chain;
pub mod deliver;
pub mod genres;
mod ops;
pub mod plan;
mod stems;
pub mod targets;
mod util;
pub mod verify;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    ops::ops()
}
