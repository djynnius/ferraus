//! The optional Audiveris OMR bridge (Java, AGPL-3.0): installation into the ferraus
//! tools cache and batch transcription of PDFs and images to MusicXML.

use ferraus_core::{paths, Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned Audiveris release.
pub const VERSION: &str = "5.11.0";

/// (platform key, asset file name, sha256)
const ASSETS: &[(&str, &str, &str)] = &[
    (
        "macos-aarch64",
        "Audiveris-5.11.0-macosx-arm64.dmg",
        "17491af8b6d40153b031dd1f0815e37213f0999ef23f10586af46706e59b2eb6",
    ),
    (
        "macos-x86_64",
        "Audiveris-5.11.0-macosx-x86_64.dmg",
        "11794424c6f1698617836a77d2c5818c46405a3fe7388281c623c4e383fa33cb",
    ),
    (
        "linux-x86_64",
        "Audiveris-5.11.0-ubuntu22.04-x86_64.deb",
        "ae714594f40e54b1a4951fc3f914f08ae38fe5d07b7f2283b1a904fdb6e0a318",
    ),
    (
        "windows-x86_64",
        "Audiveris-5.11.0-windowsConsole-x86_64.msi",
        "5f1b4e96a12c53c7da426814b76e599363c4181e291855996e0a6878dda95f71",
    ),
];

pub const LICENSE_NOTICE: &str =
    "Audiveris (https://github.com/Audiveris/audiveris) is free software licensed under the \
GNU Affero General Public License v3.0 (AGPL-3.0). ferraus runs it as a separate program and does not link it; the \
download comes from the Audiveris GitHub releases and includes its own Java runtime. By installing it you accept the \
AGPL-3.0 terms: https://www.gnu.org/licenses/agpl-3.0.html";

/// The release asset for a platform (`macos-aarch64`, `linux-x86_64`, ...).
pub fn asset_for(platform: &str) -> Option<(&'static str, &'static str)> {
    ASSETS.iter().find(|a| a.0 == platform).map(|a| (a.1, a.2))
}

pub fn current_platform() -> String {
    format!("{}-{}", if cfg!(target_os = "macos") { "macos" } else { std::env::consts::OS }, std::env::consts::ARCH)
}

pub fn install_dir() -> PathBuf {
    paths::tools_dir().join("audiveris")
}

/// The Audiveris launcher (or jar) if installed or on PATH.
pub fn find() -> Option<(PathBuf, bool)> {
    let dir = install_dir();
    let candidates = [
        dir.join("Audiveris.app/Contents/MacOS/Audiveris"),
        dir.join("opt/audiveris/bin/Audiveris"),
        dir.join("Audiveris/Audiveris.exe"),
        dir.join("Audiveris.exe"),
        dir.join("bin/Audiveris"),
    ];
    for c in candidates {
        if c.is_file() {
            return Some((c, false));
        }
    }
    // a bare jar: run with `java -jar`
    if let Some(j) = find_jar(&dir) {
        return Some((j, true));
    }
    for name in ["audiveris", "Audiveris"] {
        if let Some(p) = crate::engrave::find_in(std::env::var_os("PATH").as_deref(), name) {
            return Some((p, false));
        }
    }
    None
}

fn find_jar(dir: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(j) = find_jar(&p) {
                return Some(j);
            }
        } else if p
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .map(|n| n.starts_with("audiveris") && n.ends_with(".jar"))
            .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

/// Download and unpack the pinned Audiveris release into `tools_dir()/audiveris`.
pub fn install(yes: bool, offline: bool) -> Result<PathBuf> {
    if let Some((p, _)) = find().filter(|(p, _)| p.starts_with(install_dir())) {
        return Ok(p);
    }
    if offline {
        return Err(Error::new(
            ErrorKind::Network,
            "offline",
            "installing Audiveris needs the network and --offline is set",
        ));
    }
    let platform = current_platform();
    let (asset, sha) = asset_for(&platform).ok_or_else(|| {
        Error::unsupported(format!("no Audiveris {VERSION} build for {platform}")).with_hint(
            "install Audiveris yourself and put its launcher on PATH (https://github.com/Audiveris/audiveris/releases)",
        )
    })?;
    if !yes {
        return Err(Error::usage(format!(
            "{LICENSE_NOTICE}\n\nRe-run with --yes to accept the license and download {asset} (about 80 MB)."
        ))
        .with_hint("ferraus doctor --install-audiveris --yes"));
    }
    eprintln!("{LICENSE_NOTICE}");
    let dir = install_dir();
    let dl = dir.join("download");
    std::fs::create_dir_all(&dl)?;
    let file = dl.join(asset);
    let url = format!("https://github.com/Audiveris/audiveris/releases/download/{VERSION}/{asset}");
    if !file.exists() || ferraus_core::envelope::sha256_file(&file).ok().as_deref() != Some(sha) {
        let status = Command::new("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-fL", "--retry", "3", "-o"])
            .arg(&file)
            .arg(&url)
            .status()
            .map_err(|e| Error::new(ErrorKind::Network, "download_failed", format!("cannot run curl: {e}")))?;
        if !status.success() {
            return Err(Error::new(
                ErrorKind::Network,
                "download_failed",
                format!("downloading {url} failed ({status})"),
            ));
        }
    }
    let got = ferraus_core::envelope::sha256_file(&file)?;
    if got != sha {
        let _ = std::fs::remove_file(&file);
        return Err(Error::new(
            ErrorKind::ModelDownload,
            "checksum_mismatch",
            format!("{asset}: sha256 {got} does not match the pinned {sha}"),
        ));
    }
    extract(&file, &dir)?;
    std::fs::write(dir.join("LICENSE-NOTICE.txt"), LICENSE_NOTICE)?;
    std::fs::write(dir.join("VERSION"), VERSION)?;
    let _ = std::fs::remove_dir_all(&dl);
    find().map(|(p, _)| p).ok_or_else(|| {
        Error::processing(format!("Audiveris was unpacked into {} but its launcher was not found", dir.display()))
    })
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd.output().map_err(|e| Error::processing(format!("{what}: {e}")))?;
    if !out.status.success() {
        return Err(Error::processing(format!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(())
}

fn extract(file: &Path, dir: &Path) -> Result<()> {
    let name = file.to_string_lossy().to_string();
    if name.ends_with(".dmg") {
        let mnt = dir.join("mnt");
        std::fs::create_dir_all(&mnt)?;
        run(
            Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-mountpoint"]).arg(&mnt).arg(file),
            "hdiutil attach",
        )?;
        let res = run(Command::new("cp").arg("-R").arg(mnt.join("Audiveris.app")).arg(dir), "copy Audiveris.app");
        let _ = run(Command::new("hdiutil").arg("detach").arg(&mnt), "hdiutil detach");
        let _ = std::fs::remove_dir_all(&mnt);
        res
    } else if name.ends_with(".deb") {
        let tmp = dir.join("deb");
        std::fs::create_dir_all(&tmp)?;
        run(Command::new("ar").arg("x").arg(file).current_dir(&tmp), "ar x")?;
        let data = std::fs::read_dir(&tmp)?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("data.tar")).unwrap_or(false))
            .ok_or_else(|| Error::processing("the .deb has no data archive"))?;
        run(Command::new("tar").arg("-xf").arg(&data).arg("-C").arg(dir), "tar")?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(())
    } else if name.ends_with(".msi") {
        let target = dir.join("Audiveris");
        run(
            Command::new("msiexec").arg("/a").arg(file).arg("/qn").arg(format!("TARGETDIR={}", target.display())),
            "msiexec administrative install",
        )
    } else {
        Err(Error::unsupported(format!("cannot unpack {name}")))
    }
}

/// Run Audiveris in batch mode on a PDF or image and return the MusicXML of every
/// exported movement.
pub fn transcribe(input: &Path, pages: Option<&str>) -> Result<Vec<Vec<u8>>> {
    let (exe, jar) = find().ok_or_else(|| {
        Error::new(ErrorKind::Unsupported, "audiveris_missing", "Audiveris is not installed")
            .with_hint("ferraus doctor --install-audiveris --yes")
    })?;
    let out = std::env::temp_dir().join(format!(
        "ferraus-audiveris-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    std::fs::create_dir_all(&out)?;
    let mut cmd = if jar {
        let java = crate::engrave::find_in(std::env::var_os("PATH").as_deref(), "java").ok_or_else(|| {
            Error::new(ErrorKind::Unsupported, "java_missing", "the Audiveris jar needs Java on PATH")
        })?;
        let mut c = Command::new(java);
        c.arg("-jar").arg(&exe);
        c
    } else {
        Command::new(&exe)
    };
    cmd.arg("-batch").arg("-export").arg("-output").arg(&out);
    if let Some(p) = pages {
        cmd.arg("-sheets").args(p.split(',').map(|s| s.trim().to_string()));
    }
    cmd.arg("--").arg(input);
    let res = cmd.output().map_err(|e| Error::processing(format!("cannot run Audiveris: {e}")))?;
    let mut files = Vec::new();
    collect_mxl(&out, &mut files);
    files.sort();
    if files.is_empty() {
        let _ = std::fs::remove_dir_all(&out);
        return Err(Error::processing(format!(
            "Audiveris exported no MusicXML ({}): {}",
            res.status,
            String::from_utf8_lossy(&res.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | ")
        )));
    }
    let data = files.iter().map(std::fs::read).collect::<std::io::Result<Vec<_>>>()?;
    let _ = std::fs::remove_dir_all(&out);
    Ok(data)
}

fn collect_mxl(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_mxl(&p, out);
            } else if matches!(p.extension().and_then(|e| e.to_str()), Some("mxl") | Some("musicxml") | Some("xml")) {
                out.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_pinned() {
        for p in ["macos-aarch64", "macos-x86_64", "linux-x86_64", "windows-x86_64"] {
            let (a, sha) = asset_for(p).unwrap();
            assert!(a.contains(VERSION));
            assert_eq!(sha.len(), 64);
        }
        assert!(asset_for("plan9-mips").is_none());
    }

    #[test]
    fn install_refuses_offline_and_without_yes() {
        if find().is_some() {
            return; // already installed on this machine
        }
        let e = install(true, true).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Network);
        if asset_for(&current_platform()).is_some() {
            let e = install(false, false).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Usage);
            assert!(e.message.contains("AGPL"));
        }
    }
}
