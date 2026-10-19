//! The FFmpeg bridge: the only place in ferraus that starts an `ffmpeg` or `ffprobe`
//! process. FFmpeg is never linked; an external binary is located at run time:
//!
//! 1. `FERRAUS_FFMPEG` (set from `ffmpeg.path` in the config by the CLI),
//! 2. `<cache>/tools/ffmpeg/ffmpeg` (installed by `ferraus doctor --install-ffmpeg`),
//! 3. `ffmpeg` on `PATH`.
//!
//! `ffprobe` is looked up the same way (`FERRAUS_FFPROBE`, next to the chosen ffmpeg, the
//! tools directory, then `PATH`). `ferraus-media` builds its commands on [`run`] and
//! [`spawn`]; nothing else may call FFmpeg directly.

use ferraus_core::ctx::EncodeOptions;
use ferraus_core::{AudioBuffer, Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The hint shown whenever FFmpeg is needed but missing.
pub const INSTALL_HINT: &str = "ferraus doctor --install-ffmpeg";

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Directory where `doctor --install-ffmpeg` puts the managed build.
pub fn managed_dir() -> PathBuf {
    ferraus_core::paths::tools_dir().join("ffmpeg")
}

/// Find an executable on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let file = exe(name);
    std::env::split_paths(&path).map(|d| d.join(&file)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
        _ => false,
    }
}

fn env_bin(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| is_executable(p))
}

/// Locate the ffmpeg binary.
pub fn locate() -> Option<PathBuf> {
    if std::env::var_os("FERRAUS_NO_FFMPEG").is_some_and(|v| v == "1") {
        return None;
    }
    env_bin("FERRAUS_FFMPEG")
        .or_else(|| {
            let p = managed_dir().join(exe("ffmpeg"));
            is_executable(&p).then_some(p)
        })
        .or_else(|| which("ffmpeg"))
}

/// Locate the ffprobe binary.
pub fn locate_ffprobe() -> Option<PathBuf> {
    if std::env::var_os("FERRAUS_NO_FFMPEG").is_some_and(|v| v == "1") {
        return None;
    }
    env_bin("FERRAUS_FFPROBE")
        .or_else(|| {
            let ff = locate()?;
            let p = ff.with_file_name(exe("ffprobe"));
            is_executable(&p).then_some(p)
        })
        .or_else(|| {
            let p = managed_dir().join(exe("ffprobe"));
            is_executable(&p).then_some(p)
        })
        .or_else(|| which("ffprobe"))
}

/// The error returned when a feature needs FFmpeg and none is installed (exit code 4).
pub fn missing(what: &str) -> Error {
    Error::new(
        ErrorKind::Unsupported,
        "ffmpeg_not_found",
        format!("{what} needs the FFmpeg bridge, but no ffmpeg binary was found (FERRAUS_FFMPEG, the ferraus tools directory, or PATH)"),
    )
    .with_hint(INSTALL_HINT)
}

/// Locate ffmpeg or fail with [`missing`].
pub fn require(what: &str) -> Result<PathBuf> {
    locate().ok_or_else(|| missing(what))
}

/// Locate ffprobe or fail with [`missing`].
pub fn require_ffprobe(what: &str) -> Result<PathBuf> {
    locate_ffprobe().ok_or_else(|| missing(what))
}

/// Run a bridge binary to completion, capturing stdout and stderr.
pub fn run(bin: &Path, args: &[String]) -> Result<Output> {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::processing(format!("cannot start {}: {e}", bin.display())))
}

/// Start a bridge binary with piped stdout/stderr (for streaming pass-through).
pub fn spawn(bin: &Path, args: &[String]) -> Result<std::process::Child> {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::processing(format!("cannot start {}: {e}", bin.display())))
}

/// Last `n` non-empty lines of a process's stderr.
pub fn tail(stderr: &[u8], n: usize) -> String {
    let s = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Run ffmpeg with `args`, failing with the tail of stderr on a non-zero exit.
pub fn run_checked(what: &str, args: &[String]) -> Result<Output> {
    let bin = require(what)?;
    let out = run(&bin, args)?;
    if !out.status.success() {
        return Err(Error::processing(format!(
            "ffmpeg failed ({}) during {what}:\n{}",
            out.status,
            tail(&out.stderr, 8)
        )));
    }
    Ok(out)
}

/// `ffmpeg -version` first line and configuration, if a binary is found.
pub fn version_info(bin: &Path) -> Option<(String, String)> {
    let out = Command::new(bin).arg("-version").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    let first = s.lines().next().unwrap_or("").to_string();
    let config = s.lines().find(|l| l.starts_with("configuration:")).unwrap_or("").to_string();
    Some((first, config))
}

/// License of an FFmpeg build from its `configuration:` line.
pub fn license_from_config(config: &str) -> &'static str {
    let has = |f: &str| config.split_whitespace().any(|w| w == f);
    if has("--enable-nonfree") {
        "nonfree (not redistributable)"
    } else if has("--enable-gpl") && has("--enable-version3") {
        "GPL-3.0-or-later"
    } else if has("--enable-gpl") {
        "GPL-2.0-or-later"
    } else if has("--enable-version3") {
        "LGPL-3.0-or-later"
    } else {
        "LGPL-2.1-or-later"
    }
}

/// Audio codec arguments for an output extension.
fn codec_args(ext: &str, opts: &EncodeOptions) -> Vec<String> {
    let br = opts.bitrate.map(crate::encode::normalize_bitrate).unwrap_or(192_000);
    let mut a: Vec<String> = Vec::new();
    match ext {
        "m4a" | "aac" | "mp4" => {
            a.extend(["-c:a".into(), "aac".into(), "-b:a".into(), br.to_string()]);
            if ext == "aac" {
                a.extend(["-f".into(), "adts".into()]);
            }
        }
        "alac" | "caf" => {
            a.extend(["-c:a".into(), "alac".into()]);
            if ext == "alac" {
                a.extend(["-f".into(), "ipod".into()]);
            }
        }
        "ac3" => a.extend(["-c:a".into(), "ac3".into(), "-b:a".into(), br.to_string()]),
        "wma" => a.extend(["-c:a".into(), "wmav2".into(), "-b:a".into(), br.to_string()]),
        "mp3" => a.extend(["-c:a".into(), "libmp3lame".into(), "-b:a".into(), br.to_string()]),
        "opus" => a.extend(["-c:a".into(), "libopus".into(), "-b:a".into(), br.to_string()]),
        _ => {}
    }
    a
}

/// Encode `buf` to `path` with ffmpeg: a temporary 32-bit float WAV is written next to the
/// output and converted. Used for AAC/ALAC (`.m4a`) and other formats without a native encoder.
pub fn encode_file(buf: &AudioBuffer, path: &Path, opts: &EncodeOptions) -> Result<()> {
    let ext = opts
        .format
        .clone()
        .or_else(|| path.extension().map(|e| e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let bin = require(&format!("encoding .{ext}"))?;
    let tmp = path.with_file_name(format!(
        ".{}.ferraus-bridge.{}.wav",
        path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, ferraus_core::wav::encode(buf, 32, true)?)?;
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        tmp.display().to_string(),
    ];
    args.extend(codec_args(&ext, opts));
    for (k, v) in &opts.tags {
        args.push("-metadata".into());
        args.push(format!("{k}={v}"));
    }
    if ext == "m4a" || ext == "mp4" {
        args.extend(["-f".into(), "mp4".into()]);
    }
    args.push(path.display().to_string());
    let res = run(&bin, &args);
    let _ = std::fs::remove_file(&tmp);
    let out = res?;
    if !out.status.success() {
        return Err(Error::processing(format!("ffmpeg could not encode .{ext}:\n{}", tail(&out.stderr, 8))));
    }
    Ok(())
}

/// Decode any file ffmpeg understands to a float buffer (fallback for codecs Symphonia
/// lacks). `track` selects the n-th audio stream.
pub fn decode_file(path: &Path, track: Option<usize>) -> Result<AudioBuffer> {
    let bin = require(&format!("decoding {}", path.display()))?;
    let args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        path.display().to_string(),
        "-map".into(),
        format!("0:a:{}", track.unwrap_or(0)),
        "-vn".into(),
        "-c:a".into(),
        "pcm_f32le".into(),
        "-f".into(),
        "wav".into(),
        "-".into(),
    ];
    let out = run(&bin, &args)?;
    if !out.status.success() {
        return Err(Error::unsupported(format!(
            "ffmpeg could not decode {}:\n{}",
            path.display(),
            tail(&out.stderr, 6)
        )));
    }
    ferraus_core::wav::decode(&out.stdout).map(|r| r.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_detection() {
        assert_eq!(license_from_config("configuration: --enable-gpl --enable-version3"), "GPL-3.0-or-later");
        assert_eq!(license_from_config("configuration: --enable-gpl"), "GPL-2.0-or-later");
        assert_eq!(license_from_config("configuration: --prefix=/x"), "LGPL-2.1-or-later");
        assert!(license_from_config("--enable-nonfree --enable-gpl").starts_with("nonfree"));
    }

    #[test]
    fn tail_lines() {
        assert_eq!(tail(b"a\n\nb\nc\n", 2), "b\nc");
    }
}
