//! ferraus-ml

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}

/// Which inference devices/execution providers are available (implemented with the ML runtime).
pub fn device_report() -> serde_json::Value {
    serde_json::json!({"providers": ["cpu"], "active": "cpu"})
}
