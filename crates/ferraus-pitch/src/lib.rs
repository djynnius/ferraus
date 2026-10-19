//! ferraus-pitch

pub mod key;
pub mod notes;
pub mod psola;
pub mod pyin;
pub mod stretch;
pub mod timemap;
pub mod vocoder;
pub mod wsola;
mod util;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod probe;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}
