//! ferraus-restore: restoration, Audition-parity filters, automation, generators, and
//! one-knob enhancement chains.

pub mod dsp;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}
