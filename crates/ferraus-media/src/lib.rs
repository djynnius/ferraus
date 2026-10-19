//! ferraus-media

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    Vec::new()
}

/// Placeholder until the bridge lands: locate ffmpeg on PATH.
pub fn ffmpeg_path() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|c| c.is_file()))
}

/// Placeholder status report.
pub fn ffmpeg_status() -> serde_json::Value {
    serde_json::json!({"found": ffmpeg_path().is_some(), "path": ffmpeg_path()})
}

/// Placeholder installer.
pub fn install_ffmpeg(_yes: bool, _offline: bool) -> ferraus_core::Result<std::path::PathBuf> {
    Err(ferraus_core::Error::unsupported("managed FFmpeg install is not available in this build"))
}
