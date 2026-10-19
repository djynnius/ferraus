//! The `--json` result envelope and the provenance sidecar.

use crate::ctx::{InputRecord, ModelRecord, OutputRecord, WarningRecord};
use crate::error::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
pub const FERRAUS_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every command's structured result.
#[derive(Clone, Debug, Serialize)]
pub struct Envelope {
    pub ok: bool,
    pub command: String,
    pub ferraus_version: String,
    pub schema_version: u32,
    pub inputs: Vec<InputRecord>,
    pub outputs: Vec<OutputRecord>,
    pub params: Map<String, Value>,
    pub models: Vec<ModelRecord>,
    pub metrics: Map<String, Value>,
    pub warnings: Vec<WarningRecord>,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
    pub timing_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

impl Envelope {
    pub fn error(command: &str, e: Error, timing_ms: u64) -> Envelope {
        Envelope {
            ok: false,
            command: command.to_string(),
            ferraus_version: FERRAUS_VERSION.to_string(),
            schema_version: SCHEMA_VERSION,
            inputs: vec![],
            outputs: vec![],
            params: Map::new(),
            models: vec![],
            metrics: Map::new(),
            warnings: vec![],
            result: Value::Null,
            error: Some(e),
            timing_ms,
            dry_run: None,
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.error.as_ref().map(|e| e.exit_code).unwrap_or(0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub device: String,
    pub threads: usize,
}

impl HostInfo {
    pub fn current(device: &str, threads: usize) -> HostInfo {
        HostInfo {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            device: device.into(),
            threads,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HashedFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// `<output>.ferraus.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sidecar {
    pub ferraus_version: String,
    pub schema_version: u32,
    pub command: String,
    pub argv: Vec<String>,
    pub params: Map<String, Value>,
    pub inputs: Vec<HashedFile>,
    pub outputs: Vec<HashedFile>,
    pub models: Vec<Value>,
    pub plugins: Vec<Value>,
    pub host: HostInfo,
    /// `true`, `false`, or `"unknown"` (plugins).
    pub deterministic: Value,
    #[serde(default)]
    pub warnings: Vec<Value>,
    pub timing_ms: u64,
}

impl Sidecar {
    pub fn path_for(output: &Path) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{}.ferraus.json", output.display()))
    }
}

/// SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha256_bytes(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
