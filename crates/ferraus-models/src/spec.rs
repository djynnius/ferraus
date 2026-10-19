//! `ModelSpec`: everything the runtime needs to run one ONNX file, written next to the
//! cached model as `spec.json`. `ferraus-ml` re-exports it as `ferraus_ml::ModelSpec`.

use crate::manifest::{Entry, License, TensorSpec};
use ferraus_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub sha256: Option<String>,
    pub sample_rate: u32,
    #[serde(default)]
    pub stems: Vec<String>,
    /// Adapter family (`waveform`, `mdx`, ...).
    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default)]
    pub inputs: Vec<TensorSpec>,
    #[serde(default)]
    pub outputs: Vec<TensorSpec>,
    #[serde(default)]
    pub chunk_seconds: Option<f64>,
    #[serde(default)]
    pub chunk_samples: Option<usize>,
    /// Fraction of each window crossfaded with its neighbour.
    #[serde(default = "default_overlap")]
    pub overlap: f64,
    #[serde(default)]
    pub context_samples: usize,
    #[serde(default)]
    pub provider_hints: Vec<String>,
    #[serde(default)]
    pub license: License,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

fn default_arch() -> String {
    "waveform".into()
}

fn default_overlap() -> f64 {
    0.25
}

impl ModelSpec {
    pub fn from_entry(e: &Entry) -> ModelSpec {
        let io = e.io.clone().unwrap_or_default();
        ModelSpec {
            name: e.name.clone(),
            version: e.version.clone(),
            task: e.task.clone(),
            sha256: e.sha256.clone(),
            sample_rate: e.sample_rate.unwrap_or(44_100),
            stems: e.stems.clone(),
            arch: e.arch.clone().unwrap_or_else(default_arch),
            inputs: io.inputs,
            outputs: io.outputs,
            chunk_seconds: e.chunk_seconds,
            chunk_samples: io.chunk_samples,
            overlap: e.overlap.unwrap_or(0.25),
            context_samples: io.context_samples,
            provider_hints: e.provider_hints.clone(),
            license: e.license.clone(),
            params: io.params,
        }
    }

    /// Window length in samples at the model rate (`None` = whole input in one pass).
    pub fn chunk_len(&self) -> Option<usize> {
        self.chunk_samples.or_else(|| self.chunk_seconds.map(|s| (s * self.sample_rate as f64).round() as usize))
    }

    pub fn load(path: &Path) -> Result<ModelSpec> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|e| Error::usage(format!("{}: invalid model spec: {e}", path.display())))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// A numeric architecture parameter.
    pub fn param_f64(&self, key: &str) -> Option<f64> {
        self.params.get(key).and_then(|v| v.as_f64())
    }

    pub fn param_usize(&self, key: &str) -> Option<usize> {
        self.params.get(key).and_then(|v| v.as_u64()).map(|v| v as usize)
    }
}
