//! ferraus-models: the model registry (Ed25519-signed manifest), resumable verified
//! downloads with mirror fallback, the cache layout and `manifest.lock`, license policy,
//! and the `models ...` commands.
//!
//! Other crates call [`ensure_model`] to get a model file, which applies the download
//! policy of ADR 0004 and records the model in the provenance sidecar.

pub mod cache;
pub mod download;
pub mod ensure;
pub mod manifest;
pub mod ops;
pub mod spec;

pub use cache::Cache;
pub use ensure::{
    active_registry, downloads_allowed, ensure_model, ensure_model_with, not_cached_error, offline, CachedModel,
    EnsureOptions,
};
pub use manifest::{Entry, License, Manifest, Member, Status, TensorSpec, TierSpec};
pub use spec::ModelSpec;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    ops::ops()
}
