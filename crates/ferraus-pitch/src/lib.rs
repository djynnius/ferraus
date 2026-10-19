//! ferraus-pitch

pub mod key;
pub mod notes;
pub mod pyin;
mod util;

#[cfg(test)]
mod fixtures;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}
