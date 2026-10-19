//! ferraus-score

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}

/// Download the Audiveris OMR engine into the ferraus tools cache (implemented with the score crate).
pub fn install_audiveris(_yes: bool, _offline: bool) -> ferraus_core::Result<std::path::PathBuf> {
    Err(ferraus_core::Error::unsupported("Audiveris install is not available in this build"))
}
