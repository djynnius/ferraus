//! ferraus-restore: restoration, Audition-parity filters, automation, generators, and
//! one-knob enhancement chains.

pub mod dsp;
pub mod ops;

#[cfg(test)]
pub(crate) mod testutil;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    let mut v = Vec::new();
    v.extend(ops::clicks::ops());
    v.extend(ops::hum::ops());
    v.extend(ops::spectral::ops());
    v.extend(ops::stereo::ops());
    v.extend(ops::level::ops());
    v.extend(ops::filters::ops());
    v.extend(ops::space::ops());
    v.extend(ops::automation::ops());
    v.extend(ops::generate::ops());
    v
}
