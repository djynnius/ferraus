//! ferraus-codec: decoding and encoding for every supported container.
//!
//! Currently a WAV-only placeholder delegating to `ferraus_core::wav`; the full
//! Symphonia-based implementation replaces the internals of [`CodecIo`].

use ferraus_core::ctx::{AudioIo, EncodeOptions, SourceInfo};
use ferraus_core::{AudioBuffer, OpSpec, Result};
use std::path::Path;

/// The production `AudioIo` used by the CLI and MCP server.
pub struct CodecIo;

impl AudioIo for CodecIo {
    fn read(&self, path: &Path) -> Result<(AudioBuffer, SourceInfo)> {
        ferraus_core::wav::SimpleIo.read(path)
    }
    fn read_bytes(&self, bytes: &[u8], hint: Option<&str>) -> Result<(AudioBuffer, SourceInfo)> {
        ferraus_core::wav::SimpleIo.read_bytes(bytes, hint)
    }
    fn probe(&self, path: &Path) -> Result<SourceInfo> {
        ferraus_core::wav::SimpleIo.probe(path)
    }
    fn write(&self, path: &Path, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<()> {
        ferraus_core::wav::SimpleIo.write(path, buf, opts)
    }
    fn write_bytes(&self, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<Vec<u8>> {
        ferraus_core::wav::SimpleIo.write_bytes(buf, opts)
    }
    fn can_encode(&self, ext: &str) -> bool {
        ext == "wav"
    }
}

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}
