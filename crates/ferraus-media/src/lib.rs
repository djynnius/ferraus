//! # ferraus-media
//!
//! The FFmpeg bridge (PRD "FFmpeg retention and the v2 video path"):
//!
//! * `ferraus ffmpeg -- <args>` / `ferraus ffprobe -- <args>`: pass-through to the managed
//!   binary, with the exact command line recorded in the result and sidecar.
//! * Container jobs as proper commands: `media info`, `media streams`,
//!   `media extract-audio`, `media replace-audio`, `media remux`.
//! * [`MediaStream`]: the stream inventory (video, audio, subtitle, data) reserved for v2.
//! * [`install_ffmpeg`]: the pinned, checksummed FFmpeg download behind
//!   `ferraus doctor --install-ffmpeg`; [`ffmpeg_path`] and [`ffmpeg_status`] for `doctor`.
//!
//! Architecture rule: no command may start FFmpeg except through the bridge
//! (`ferraus_codec::ffmpeg_bridge` for discovery and spawning, [`bridge`] for running).

pub mod bridge;
pub mod install;
mod ops;
pub mod streams;

pub use install::{install_ffmpeg, install_ffmpeg_report, FfmpegBuild, InstallReport, BUILDS};
pub use ops::ops;
pub use streams::{MediaStream, StreamKind};

use ferraus_codec::ffmpeg_bridge;
use serde_json::{json, Value};
use std::path::PathBuf;

/// The ffmpeg binary the bridge would use, if any (`FERRAUS_FFMPEG`, the managed install,
/// then `PATH`).
pub fn ffmpeg_path() -> Option<PathBuf> {
    ffmpeg_bridge::locate()
}

/// Bridge status for `ferraus doctor`: whether ffmpeg/ffprobe were found, where, the
/// version line, and the license derived from the build configuration.
pub fn ffmpeg_status() -> Value {
    let Some(path) = ffmpeg_bridge::locate() else {
        return json!({
            "found": false,
            "hint": ffmpeg_bridge::INSTALL_HINT,
            "pinned_build": install::current_build().map(|b| json!({"version": b.version, "license": b.license, "provider": b.provider})),
        });
    };
    let source = if std::env::var_os("FERRAUS_FFMPEG").is_some_and(|v| !v.is_empty() && std::path::Path::new(&v) == path) {
        "config"
    } else if path.starts_with(ffmpeg_bridge::managed_dir()) {
        "managed"
    } else {
        "path"
    };
    let (version, config) = ffmpeg_bridge::version_info(&path).unwrap_or_default();
    json!({
        "found": true,
        "path": path.display().to_string(),
        "ffprobe": ffmpeg_bridge::locate_ffprobe().map(|p| p.display().to_string()),
        "source": source,
        "version": version,
        "license": if config.is_empty() { Value::Null } else { json!(ffmpeg_bridge::license_from_config(&config)) },
        "managed_build": install::managed_build_info(),
    })
}
