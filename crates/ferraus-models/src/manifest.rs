//! The registry manifest: a JSON document listing every model ferraus knows how to fetch,
//! plus the per-release tier pins. Remote manifests are Ed25519-signed (detached
//! signature, hex encoded); the public key ships in the binary. The manifest compiled into
//! the binary (`models/registry.json` at build time) is trusted by construction.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use ferraus_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The source manifest compiled into the binary (offline default).
pub const EMBEDDED_MANIFEST: &str = include_str!("../../../models/registry.json");

/// Hex-encoded Ed25519 public key that verifies remote and pinned manifests.
///
/// This is the development key committed to the repository. Release builds must replace
/// `models/registry.pub` with the public half of the CI signing key (a GitHub Actions
/// secret); the private key never exists outside CI.
pub const REGISTRY_PUBLIC_KEY_HEX: &str = include_str!("../../../models/registry.pub");

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Whether an entry's file and hash have been checked by the project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// `sha256` was computed from the exact file at `urls`.
    Verified,
    /// URL or hash not confirmed; downloads refuse it unless `FERRAUS_ALLOW_UNVERIFIED=1`.
    #[default]
    Unverified,
    /// Implemented inside the binary; nothing to download.
    Builtin,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct License {
    #[serde(default)]
    pub spdx: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// `true` permits commercial use, `false` forbids it (research-only / NC), `null` = no
    /// clear license.
    #[serde(default)]
    pub commercial_use: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl License {
    /// Research-only or non-commercial: listed, flagged, never in tiers, needs `--accept-license`.
    pub fn research_only(&self) -> bool {
        self.commercial_use == Some(false)
    }
    /// No clear license recorded: excluded from tiers, usable via `--model` with a warning.
    pub fn unclear(&self) -> bool {
        self.commercial_use.is_none()
    }
    /// Suitable for default tiers.
    pub fn tier_eligible(&self) -> bool {
        self.commercial_use == Some(true)
    }
    pub fn label(&self) -> String {
        let s = self.spdx.clone().unwrap_or_else(|| "unknown".into());
        match self.commercial_use {
            Some(true) => s,
            Some(false) => format!("{s} (non-commercial)"),
            None => format!("{s} (no clear license)"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// One tensor of a model's ONNX signature. `-1` marks a dynamic dimension.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TensorSpec {
    pub name: String,
    #[serde(default)]
    pub shape: Vec<i64>,
}

/// How the runtime feeds the model (ferraus extension to the PRD entry format).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IoSpec {
    #[serde(default)]
    pub inputs: Vec<TensorSpec>,
    #[serde(default)]
    pub outputs: Vec<TensorSpec>,
    /// Fixed model input length in samples (overrides `chunk_seconds`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_samples: Option<usize>,
    /// Samples discarded at each chunk edge (receptive-field context).
    #[serde(default)]
    pub context_samples: usize,
    /// Architecture-specific parameters (n_fft, hop, dim_f, ...).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

/// One registry entry. The first block of fields is exactly the PRD example; the rest are
/// ferraus extensions with defaults so plain PRD-shaped entries parse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub version: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stems: Vec<String>,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub chunk_seconds: Option<f64>,
    #[serde(default)]
    pub overlap: Option<f64>,
    #[serde(default)]
    pub license: License,
    #[serde(default)]
    pub source: Source,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub benchmarks: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ferraus_version: Option<String>,
    // ---- ferraus extensions ----
    #[serde(default)]
    pub status: Status,
    /// Adapter family: `waveform` (Demucs / ferraus exports), `mdx` (UVR MDX-Net
    /// spectrogram models), `basic-pitch`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io: Option<IoSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn default_format() -> String {
    "onnx".into()
}

impl Entry {
    /// `separate/htdemucs` → `htdemucs`.
    pub fn short_name(&self) -> &str {
        self.name
            .split_once('/')
            .map(|(_, s)| s)
            .unwrap_or(&self.name)
    }
    pub fn id(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
    pub fn is_builtin(&self) -> bool {
        self.status == Status::Builtin
    }
    pub fn file_name(&self) -> String {
        match self.format.as_str() {
            "onnx" => "model.onnx".into(),
            other => format!("model.{other}"),
        }
    }
}

/// A member of a tier: one model, optionally restricted to some stems, with a weight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    /// `name@version` (versions are pinned per ferraus release).
    pub model: String,
    #[serde(default = "one")]
    pub weight: f64,
    /// Stems this member contributes (empty = all of the model's stems).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stems: Vec<String>,
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TierSpec {
    #[serde(default)]
    pub members: Vec<Member>,
    /// Members for `--stems 6` (falls back to `members`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members_6: Vec<Member>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shifts: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub channel_swap: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl TierSpec {
    pub fn all_members(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().chain(self.members_6.iter())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub models: Vec<Entry>,
    /// command → tier name → spec. Cascade stages use pseudo-commands such as
    /// `separate.vocal-split`.
    #[serde(default)]
    pub tiers: BTreeMap<String, BTreeMap<String, TierSpec>>,
}

/// Split `name@version`.
pub fn parse_ref(s: &str) -> (&str, Option<&str>) {
    match s.rsplit_once('@') {
        Some((n, v)) if !v.is_empty() => (n, Some(v)),
        _ => (s, None),
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest> {
        let m: Manifest = serde_json::from_str(text).map_err(|e| {
            Error::new(
                ErrorKind::ModelDownload,
                "invalid_manifest",
                format!("model registry manifest is invalid: {e}"),
            )
        })?;
        if m.schema_version > MANIFEST_SCHEMA_VERSION {
            return Err(Error::new(
                ErrorKind::ModelDownload,
                "manifest_too_new",
                format!(
                    "registry manifest schema {} is newer than this ferraus understands ({})",
                    m.schema_version, MANIFEST_SCHEMA_VERSION
                ),
            )
            .with_hint("ferraus self-update"));
        }
        Ok(m)
    }

    pub fn embedded() -> Manifest {
        Manifest::parse(EMBEDDED_MANIFEST).expect("embedded registry manifest must parse")
    }

    /// Every version of `name` (accepts the short name too when unambiguous).
    pub fn versions_of(&self, name: &str) -> Vec<&Entry> {
        let exact: Vec<&Entry> = self.models.iter().filter(|e| e.name == name).collect();
        if !exact.is_empty() {
            return exact;
        }
        let short: Vec<&Entry> = self
            .models
            .iter()
            .filter(|e| e.short_name() == name)
            .collect();
        let first = short.first().map(|e| e.name.clone());
        if short.iter().all(|e| Some(&e.name) == first.as_ref()) {
            short
        } else {
            Vec::new()
        }
    }

    /// Resolve `name[@version]`. Without a version the tier-pinned version wins, then the
    /// newest version string.
    pub fn resolve(&self, reference: &str) -> Result<&Entry> {
        let (name, version) = parse_ref(reference);
        let candidates = self.versions_of(name);
        if candidates.is_empty() {
            return Err(Error::usage(format!("unknown model '{name}'"))
                .with_hint("ferraus models list"));
        }
        if let Some(v) = version {
            return candidates
                .into_iter()
                .find(|e| e.version == v)
                .ok_or_else(|| {
                    Error::usage(format!("model '{name}' has no version '{v}'"))
                        .with_hint(format!("ferraus models info {name}"))
                });
        }
        let full = candidates[0].name.clone();
        for spec in self.tiers.values().flat_map(|t| t.values()) {
            for m in spec.all_members() {
                let (n, v) = parse_ref(&m.model);
                if n == full {
                    if let Some(e) = candidates.iter().find(|e| Some(e.version.as_str()) == v) {
                        return Ok(e);
                    }
                }
            }
        }
        Ok(candidates
            .into_iter()
            .max_by(|a, b| a.version.cmp(&b.version))
            .expect("non-empty"))
    }

    /// Tier labels (`separate:fast`) that use this entry.
    pub fn tiers_using(&self, e: &Entry) -> Vec<String> {
        let mut out = Vec::new();
        for (cmd, tiers) in &self.tiers {
            for (tier, spec) in tiers {
                if spec.all_members().any(|m| {
                    let (n, v) = parse_ref(&m.model);
                    n == e.name && v.map(|v| v == e.version).unwrap_or(true)
                }) {
                    out.push(format!("{cmd}:{tier}"));
                }
            }
        }
        out
    }

    pub fn tier(&self, command: &str, tier: &str) -> Option<&TierSpec> {
        self.tiers.get(command).and_then(|t| t.get(tier))
    }

    /// Check internal consistency: tier members exist and are license-eligible.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for e in &self.models {
            if !seen.insert(e.id()) {
                problems.push(format!("duplicate entry {}", e.id()));
            }
            if !e.name.starts_with(&format!("{}/", e.task)) {
                problems.push(format!("{} must be prefixed with its task '{}'", e.name, e.task));
            }
            if e.status == Status::Verified && e.sha256.is_none() {
                problems.push(format!("{} is verified but has no sha256", e.id()));
            }
            if let Some(h) = &e.sha256 {
                if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                    problems.push(format!("{} has a malformed sha256", e.id()));
                }
            }
        }
        for (cmd, tiers) in &self.tiers {
            for (tier, spec) in tiers {
                for m in spec.all_members() {
                    let (n, v) = parse_ref(&m.model);
                    if v.is_none() {
                        problems.push(format!("{cmd}:{tier} member {n} must pin a version"));
                    }
                    match self.models.iter().find(|e| e.name == n && Some(e.version.as_str()) == v) {
                        None => problems.push(format!("{cmd}:{tier} references missing {}", m.model)),
                        Some(e) if !e.license.tier_eligible() => problems.push(format!(
                            "{cmd}:{tier} references {} whose license does not permit commercial use",
                            m.model
                        )),
                        _ => {}
                    }
                }
            }
        }
        problems
    }
}

/// Parse a hex public key.
pub fn public_key(hex_key: &str) -> Result<VerifyingKey> {
    let bytes = hex::decode(hex_key.trim())
        .map_err(|_| Error::internal("registry public key is not valid hex"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::internal("registry public key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&arr).map_err(|_| Error::internal("registry public key is invalid"))
}

/// The key compiled into this binary.
pub fn embedded_public_key() -> VerifyingKey {
    public_key(REGISTRY_PUBLIC_KEY_HEX).expect("embedded registry public key must be valid")
}

/// Verify a detached hex signature over the exact manifest bytes.
pub fn verify_signature(key: &VerifyingKey, manifest: &[u8], signature_hex: &str) -> Result<()> {
    let bad = |why: &str| {
        Error::new(
            ErrorKind::Verification,
            "manifest_signature_invalid",
            format!("registry manifest signature rejected: {why}"),
        )
        .with_hint("ferraus models registry update")
    };
    let sig_bytes = hex::decode(signature_hex.trim()).map_err(|_| bad("signature is not hex"))?;
    let arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| bad("signature must be 64 bytes"))?;
    let sig = Signature::from_bytes(&arr);
    key.verify(manifest, &sig)
        .map_err(|_| bad("signature does not match the manifest"))
}

/// Verify and parse a signed manifest.
pub fn parse_signed(key: &VerifyingKey, manifest: &[u8], signature_hex: &str) -> Result<Manifest> {
    verify_signature(key, manifest, signature_hex)?;
    let text = std::str::from_utf8(manifest)
        .map_err(|_| Error::verification("registry manifest is not UTF-8"))?;
    Manifest::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn embedded_manifest_parses_and_validates() {
        let m = Manifest::embedded();
        let problems = m.validate();
        assert!(problems.is_empty(), "{problems:#?}");
        assert!(m.models.len() >= 10);
        // every slot the PRD names exists
        for slot in [
            "denoise/dfn3",
            "denoise/rnnoise",
            "pitch/crepe",
            "pitch/rmvpe",
            "pitch/swiftf0",
            "transcribe/basic-pitch",
            "transcribe/multi-yourmt3",
            "omr/oemer",
            "omr/homr",
            "separate/htdemucs",
        ] {
            assert!(!m.versions_of(slot).is_empty(), "missing {slot}");
        }
        // research-only / unclear licenses never in tiers
        for e in &m.models {
            if !e.license.tier_eligible() {
                assert!(m.tiers_using(e).is_empty(), "{} is in a tier", e.id());
            }
            if e.status == Status::Unverified {
                assert!(e.sha256.is_none() || e.status != Status::Verified);
            }
        }
        assert!(embedded_public_key().as_bytes().len() == 32);
    }

    #[test]
    fn signature_accepts_signed_and_rejects_tampered() {
        let sk = test_key();
        let vk = sk.verifying_key();
        let manifest = EMBEDDED_MANIFEST.as_bytes();
        let sig = hex::encode(sk.sign(manifest).to_bytes());
        let m = parse_signed(&vk, manifest, &sig).unwrap();
        assert_eq!(m, Manifest::embedded());

        // tampered body
        let tampered = EMBEDDED_MANIFEST.replacen("\"MIT\"", "\"GPL-3.0\"", 1);
        assert_ne!(tampered, EMBEDDED_MANIFEST);
        let e = parse_signed(&vk, tampered.as_bytes(), &sig).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Verification);
        // tampered signature
        let mut bad = sig.clone().into_bytes();
        bad[0] = if bad[0] == b'0' { b'1' } else { b'0' };
        let e = parse_signed(&vk, manifest, std::str::from_utf8(&bad).unwrap()).unwrap_err();
        assert_eq!(e.code, "manifest_signature_invalid");
        // wrong key
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
        assert!(parse_signed(&other, manifest, &sig).is_err());
        // garbage
        assert!(parse_signed(&vk, manifest, "zz").is_err());
    }

    #[test]
    fn resolve_refs() {
        let m = Manifest::embedded();
        let e = m.resolve("separate/htdemucs").unwrap();
        assert_eq!(e.name, "separate/htdemucs");
        let e2 = m.resolve(&format!("htdemucs@{}", e.version)).unwrap();
        assert_eq!(e, e2);
        assert!(m.resolve("separate/htdemucs@nope").is_err());
        assert!(m.resolve("nope/nothing").is_err());
        assert_eq!(parse_ref("a/b@1.0"), ("a/b", Some("1.0")));
        assert_eq!(parse_ref("a/b"), ("a/b", None));
    }

    #[test]
    fn prd_example_entry_parses() {
        let text = r#"{
          "name": "separate/bs-roformer-4s", "version": "2026.07", "task": "separate",
          "stems": ["vocals", "drums", "bass", "other"], "format": "onnx",
          "size_bytes": 612000000, "sha256": "...",
          "urls": ["https://github.com/x/releases/download/y", "https://huggingface.co/x/resolve/y"],
          "sample_rate": 44100, "chunk_seconds": 11.9, "overlap": 0.25,
          "license": {"spdx": "MIT", "url": "...", "commercial_use": true},
          "source": {"paper": "https://arxiv.org/abs/2309.02612", "weights": "...", "export_script": "tools/export/bs_roformer.py"},
          "benchmarks": {"multisong_sdr": {"vocals": 11.9}},
          "min_ferraus_version": "1.0.0"
        }"#;
        let e: Entry = serde_json::from_str(text).unwrap();
        assert_eq!(e.short_name(), "bs-roformer-4s");
        assert_eq!(e.status, Status::Unverified);
        assert!(e.license.tier_eligible());
    }
}
