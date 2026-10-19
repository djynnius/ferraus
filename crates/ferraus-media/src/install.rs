//! The managed FFmpeg: `ferraus doctor --install-ffmpeg` downloads a pinned static build for
//! the current platform into `<cache>/tools/ffmpeg/`, verifies its SHA-256 against the table
//! below, extracts `ffmpeg` (and `ffprobe`), and reports the build's license.
//!
//! The table is data: to move to a newer build, replace the URL and checksum together.
//! Downloads use the system `curl` and archives are unpacked with the system `tar` (bsdtar
//! on macOS and Windows reads `.zip`; GNU tar on Linux reads `.tar.xz`), so ferraus needs no
//! TLS or archive libraries of its own. An entry without a checksum is refused unless
//! `FERRAUS_ALLOW_UNVERIFIED=1` is set.

use ferraus_codec::ffmpeg_bridge;
use ferraus_core::{Error, ErrorKind, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One downloadable archive.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Download {
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the archive; `None` means "not yet pinned" (refused by default).
    pub sha256: Option<&'static str>,
}

/// A pinned FFmpeg build for one platform.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct FfmpegBuild {
    /// `std::env::consts::OS` value: `linux`, `macos`, `windows`.
    pub os: &'static str,
    /// `std::env::consts::ARCH` value: `x86_64`, `aarch64`.
    pub arch: &'static str,
    pub version: &'static str,
    /// License of this build as published by its provider.
    pub license: &'static str,
    pub provider: &'static str,
    /// Archive containing `ffmpeg` (and `ffprobe` when `ffprobe` is `None`).
    pub ffmpeg: Download,
    /// Separate archive for `ffprobe`, when the provider ships it separately.
    pub ffprobe: Option<Download>,
    pub size_mb: u32,
    pub note: &'static str,
}

/// Pinned builds. Checksums for the BtbN entries come from the release's published
/// `checksums.sha256`; the evermeet.cx checksums were computed from the downloaded archives.
pub const BUILDS: &[FfmpegBuild] = &[
    FfmpegBuild {
        os: "linux",
        arch: "x86_64",
        version: "8.1.3",
        license: "LGPL-2.1-or-later",
        provider: "BtbN/FFmpeg-Builds (static, lgpl)",
        ffmpeg: Download {
            url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-18-13-01/ffmpeg-n8.1.3-18-g46d899d7a0-linux64-lgpl-8.1.tar.xz",
            sha256: Some("746a270f8f6bdaca09a5caee3a9303e2e42cd4006891541f629179d162927516"),
        },
        ffprobe: None,
        size_mb: 137,
        note: "",
    },
    FfmpegBuild {
        os: "linux",
        arch: "aarch64",
        version: "8.1.3",
        license: "LGPL-2.1-or-later",
        provider: "BtbN/FFmpeg-Builds (static, lgpl)",
        ffmpeg: Download {
            url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-18-13-01/ffmpeg-n8.1.3-18-g46d899d7a0-linuxarm64-lgpl-8.1.tar.xz",
            sha256: Some("56f57d8e57a9b484d34e751ca92540a095795a19da08bcfa0069bd376f4b8585"),
        },
        ffprobe: None,
        size_mb: 116,
        note: "",
    },
    FfmpegBuild {
        os: "windows",
        arch: "x86_64",
        version: "8.1.3",
        license: "LGPL-2.1-or-later",
        provider: "BtbN/FFmpeg-Builds (static, lgpl)",
        ffmpeg: Download {
            url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-18-13-01/ffmpeg-n8.1.3-18-g46d899d7a0-win64-lgpl-8.1.zip",
            sha256: Some("0bfeae2d03bce73d58ab5c7ff8cb0f50cba524091fd47a9ba6f3504e7dd5eafa"),
        },
        ffprobe: None,
        size_mb: 171,
        note: "",
    },
    FfmpegBuild {
        os: "windows",
        arch: "aarch64",
        version: "8.1.3",
        license: "LGPL-2.1-or-later",
        provider: "BtbN/FFmpeg-Builds (static, lgpl)",
        ffmpeg: Download {
            url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-18-13-01/ffmpeg-n8.1.3-18-g46d899d7a0-winarm64-lgpl-8.1.zip",
            sha256: Some("f405f31ce2c9f7549c9340f34446cf28570e82d3463012bd018d501e198a08ea"),
        },
        ffprobe: None,
        size_mb: 116,
        note: "",
    },
    FfmpegBuild {
        os: "macos",
        arch: "x86_64",
        version: "9.0.2",
        license: "GPL-3.0-or-later",
        provider: "evermeet.cx (static)",
        ffmpeg: Download {
            url: "https://evermeet.cx/ffmpeg/ffmpeg-9.0.2.zip",
            sha256: Some("4acc0be580f9b2788029eb7bd4d645ff87968911b0a62aeeb3940d42d54558d5"),
        },
        ffprobe: Some(Download {
            url: "https://evermeet.cx/ffmpeg/ffprobe-9.0.2.zip",
            sha256: Some("24a9c968cd4da72d99c7245e914b921815835eb6dff01d99868031aebaf1d439"),
        }),
        size_mb: 52,
        note: "",
    },
    FfmpegBuild {
        os: "macos",
        arch: "aarch64",
        version: "9.0.2",
        license: "GPL-3.0-or-later",
        provider: "evermeet.cx (static, x86_64)",
        ffmpeg: Download {
            url: "https://evermeet.cx/ffmpeg/ffmpeg-9.0.2.zip",
            sha256: Some("4acc0be580f9b2788029eb7bd4d645ff87968911b0a62aeeb3940d42d54558d5"),
        },
        ffprobe: Some(Download {
            url: "https://evermeet.cx/ffmpeg/ffprobe-9.0.2.zip",
            sha256: Some("24a9c968cd4da72d99c7245e914b921815835eb6dff01d99868031aebaf1d439"),
        }),
        size_mb: 52,
        // TODO: pin a native arm64 static build once a provider with stable, checksummed
        // URLs is chosen; `brew install ffmpeg` is the native alternative today.
        note: "x86_64 build; runs on Apple silicon through Rosetta 2 (softwareupdate --install-rosetta)",
    },
];

/// The URL of a download.
pub fn url_of(d: &Download) -> String {
    d.url.to_string()
}

/// The pinned build for an OS/arch pair.
pub fn build_for(os: &str, arch: &str) -> Option<&'static FfmpegBuild> {
    BUILDS.iter().find(|b| b.os == os && b.arch == arch)
}

/// The pinned build for this machine.
pub fn current_build() -> Option<&'static FfmpegBuild> {
    build_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// What an installation produced.
#[derive(Clone, Debug, Serialize)]
pub struct InstallReport {
    pub path: PathBuf,
    pub ffprobe: Option<PathBuf>,
    pub version: String,
    /// License declared for the build, confirmed from `ffmpeg -version` when possible.
    pub license: String,
    pub provider: String,
    pub urls: Vec<String>,
    pub sha256: Vec<String>,
    pub verified: bool,
    pub note: String,
}

fn allow_unverified() -> bool {
    std::env::var("FERRAUS_ALLOW_UNVERIFIED").map(|v| v == "1").unwrap_or(false)
}

/// Check that every archive of a build can be verified (or that the user opted out).
pub fn check_verifiable(b: &FfmpegBuild) -> Result<bool> {
    let all = std::iter::once(&b.ffmpeg).chain(b.ffprobe.iter()).all(|d| d.sha256.is_some());
    if all {
        return Ok(true);
    }
    if allow_unverified() {
        return Ok(false);
    }
    Err(Error::new(
        ErrorKind::Verification,
        "unverifiable_download",
        format!(
            "the pinned FFmpeg build for {}-{} has no SHA-256 checksum yet, so it cannot be verified",
            b.os, b.arch
        ),
    )
    .with_hint(
        "set FERRAUS_ALLOW_UNVERIFIED=1 to accept it anyway, or point ffmpeg.path in the config at your own ffmpeg",
    ))
}

/// SHA-256 of a file as lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
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

fn download(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(["-fL", "--retry", "3", "--proto", "=https", "--tlsv1.2", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| {
            Error::new(ErrorKind::Network, "curl_missing", format!("cannot run curl to download FFmpeg: {e}"))
                .with_hint("install curl, or install ffmpeg yourself and set ffmpeg.path in the config")
        })?;
    if !status.success() {
        return Err(Error::new(ErrorKind::Network, "download_failed", format!("downloading {url} failed ({status})")));
    }
    Ok(())
}

fn extract(archive: &Path, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .status()
        .map_err(|e| Error::processing(format!("cannot run tar to extract {}: {e}", archive.display())))?;
    if !status.success() {
        return Err(Error::processing(format!("extracting {} failed ({status})", archive.display())));
    }
    Ok(())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
    }
    subdirs.into_iter().find_map(|d| find_file(&d, name))
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn confirm(b: &FfmpegBuild, dest: &Path) -> Result<()> {
    let msg = format!(
        "Download FFmpeg {} ({}, about {} MB) from {} into {}?",
        b.version,
        b.license,
        b.size_mb,
        b.provider,
        dest.display()
    );
    if !std::io::stdin().is_terminal() {
        return Err(Error::usage(format!("{msg} Confirmation needed: pass --yes"))
            .with_hint("ferraus doctor --install-ffmpeg --yes"));
    }
    eprint!("{msg} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(Error::cancelled("FFmpeg installation declined"))
    }
}

/// Download, verify, and install the pinned FFmpeg build; returns the `ffmpeg` path. The
/// build's license is printed to stderr (see [`install_ffmpeg_report`] for the details).
pub fn install_ffmpeg(yes: bool, offline: bool) -> Result<PathBuf> {
    let r = install_ffmpeg_report(yes, offline)?;
    eprintln!(
        "installed FFmpeg {} at {} — license: {} ({}){}",
        r.version,
        r.path.display(),
        r.license,
        r.provider,
        if r.note.is_empty() { String::new() } else { format!("; {}", r.note) }
    );
    Ok(r.path)
}

/// Like [`install_ffmpeg`], returning the full report.
pub fn install_ffmpeg_report(yes: bool, offline: bool) -> Result<InstallReport> {
    let b = current_build().ok_or_else(|| {
        Error::unsupported(format!("no pinned FFmpeg build for {}-{}", std::env::consts::OS, std::env::consts::ARCH))
            .with_hint("install ffmpeg with your package manager and set ffmpeg.path in the config")
    })?;
    install_build(b, yes, offline, &ffmpeg_bridge::managed_dir())
}

/// Install a specific build into `dest` (exposed for tests and alternate tables).
pub fn install_build(b: &FfmpegBuild, yes: bool, offline: bool, dest: &Path) -> Result<InstallReport> {
    if offline || std::env::var("FERRAUS_OFFLINE").map(|v| v == "1").unwrap_or(false) {
        return Err(Error::new(ErrorKind::Network, "offline", "--offline is set, so FFmpeg cannot be downloaded")
            .with_hint("install ffmpeg yourself and set ffmpeg.path in the config, or drop --offline"));
    }
    let verified = check_verifiable(b)?;
    if !yes {
        confirm(b, dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let work = dest.join(format!(".download-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let result = (|| -> Result<InstallReport> {
        let mut urls = Vec::new();
        let mut sums = Vec::new();
        for (i, d) in std::iter::once(&b.ffmpeg).chain(b.ffprobe.iter()).enumerate() {
            let url = url_of(d);
            let name = url.rsplit('/').next().unwrap_or("archive");
            let archive = work.join(format!("{i}-{name}"));
            download(&url, &archive)?;
            let got = sha256_file(&archive)?;
            if let Some(want) = d.sha256 {
                if !got.eq_ignore_ascii_case(want) {
                    return Err(Error::new(
                        ErrorKind::Verification,
                        "checksum_mismatch",
                        format!("SHA-256 mismatch for {url}: expected {want}, got {got}"),
                    )
                    .with_hint("the provider may have replaced the file; update ferraus or install ffmpeg yourself"));
                }
            }
            extract(&archive, &work.join(format!("x{i}")))?;
            urls.push(url);
            sums.push(got);
        }
        let ff = find_file(&work, &exe("ffmpeg"))
            .ok_or_else(|| Error::processing("the FFmpeg archive did not contain an ffmpeg binary"))?;
        let probe = find_file(&work, &exe("ffprobe"));
        let ff_dest = dest.join(exe("ffmpeg"));
        let _ = std::fs::remove_file(&ff_dest);
        std::fs::rename(&ff, &ff_dest).or_else(|_| std::fs::copy(&ff, &ff_dest).map(|_| ()))?;
        let probe_dest = if let Some(p) = probe {
            let pd = dest.join(exe("ffprobe"));
            let _ = std::fs::remove_file(&pd);
            std::fs::rename(&p, &pd).or_else(|_| std::fs::copy(&p, &pd).map(|_| ()))?;
            Some(pd)
        } else {
            None
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in std::iter::once(&ff_dest).chain(probe_dest.iter()) {
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        // confirm the license from the binary itself
        let license = match ffmpeg_bridge::version_info(&ff_dest) {
            Some((_, cfg)) if !cfg.is_empty() => ffmpeg_bridge::license_from_config(&cfg).to_string(),
            _ => b.license.to_string(),
        };
        let report = InstallReport {
            path: ff_dest,
            ffprobe: probe_dest,
            version: b.version.into(),
            license,
            provider: b.provider.into(),
            urls,
            sha256: sums,
            verified,
            note: b.note.into(),
        };
        let meta = serde_json::to_vec_pretty(&report).unwrap_or_default();
        std::fs::write(dest.join("BUILD.json"), meta)?;
        Ok(report)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Metadata written by the last managed install, if any.
pub fn managed_build_info() -> Option<serde_json::Value> {
    let p = ffmpeg_bridge::managed_dir().join("BUILD.json");
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_well_formed() {
        for b in BUILDS {
            for d in std::iter::once(&b.ffmpeg).chain(b.ffprobe.iter()) {
                let url = url_of(d);
                assert!(url.starts_with("https://"), "{url}");
                assert!(url.ends_with(".zip") || url.ends_with(".tar.xz"), "{url}");
                if let Some(s) = d.sha256 {
                    assert_eq!(s.len(), 64, "{url}");
                    assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
                }
            }
            assert!(!b.license.is_empty() && !b.version.is_empty());
        }
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
        ] {
            assert!(build_for(os, arch).is_some(), "{os}-{arch}");
        }
    }

    #[test]
    fn unverifiable_builds_are_refused() {
        let b = FfmpegBuild {
            os: "test",
            arch: "test",
            version: "0",
            license: "LGPL-2.1-or-later",
            provider: "test",
            ffmpeg: Download { url: "https://example.invalid/ffmpeg.zip", sha256: None },
            ffprobe: None,
            size_mb: 1,
            note: "",
        };
        if !allow_unverified() {
            let e = check_verifiable(&b).unwrap_err();
            assert_eq!(e.exit_code, 9);
            assert!(e.hint.unwrap().contains("FERRAUS_ALLOW_UNVERIFIED"));
        }
        assert!(check_verifiable(BUILDS.first().unwrap()).unwrap());
    }

    #[test]
    fn offline_refuses_before_downloading() {
        let dir = std::env::temp_dir().join(format!("ferraus-ffinstall-{}", std::process::id()));
        let e = install_build(&BUILDS[0], true, true, &dir).unwrap_err();
        assert_eq!(e.exit_code, 10);
        assert!(!dir.exists());
    }

    #[test]
    fn sha256_of_file() {
        let p = std::env::temp_dir().join(format!("ferraus-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(sha256_file(&p).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_file(&p);
    }
}
