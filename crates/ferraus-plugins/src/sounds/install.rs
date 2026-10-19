//! Download (with resume), SHA-256 verification, archive extraction, and placement of
//! registry sounds. Plugins go to the per-user plugin folders; SoundFonts, SFZ libraries,
//! and impulse responses to `<data>/ferraus/sounds/<id>/`.

use super::{Download, Entry};
use crate::client::PluginFormat;
use crate::resolve::{read_installed, write_installed, InstalledRecord, SoundKind, SoundsEnv};
use crate::scan::{self, ScanOptions};
use ferraus_core::{Error, ErrorKind, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const INSTALLERS: &[&str] = &["dmg", "pkg", "exe", "msi", "deb", "rpm", "appimage"];

#[derive(Clone, Debug)]
pub struct InstallOptions {
    /// Downloads allowed (`--yes` or auto-download in the config).
    pub yes: bool,
    pub offline: bool,
    /// `FERRAUS_ALLOW_UNVERIFIED=1`: allow entries without a pinned SHA-256.
    pub allow_unverified: bool,
    pub dry_run: bool,
    /// `macos`, `linux`, or `windows`.
    pub os: String,
    pub watchdog: Duration,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("ferraus/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn net_err(url: &str, e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Network, "download_failed", format!("cannot download {url}: {e}"))
        .with_hint("check the network connection and retry; partial downloads resume")
}

/// GET a small resource (manifests, signatures) into memory (limit 16 MB).
pub fn http_get_small(url: &str) -> Result<Vec<u8>> {
    let mut resp = agent().get(url).call().map_err(|e| net_err(url, e))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(net_err(url, format!("HTTP {status}")));
    }
    resp.body_mut().with_config().limit(16 << 20).read_to_vec().map_err(|e| net_err(url, e))
}

/// Download `url` to `dest`, resuming from an existing partial file with a Range request.
pub fn download(url: &str, dest: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<()> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    let have = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let mut req = agent().get(url);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let resp = req.call().map_err(|e| net_err(url, e))?;
    let status = resp.status().as_u16();
    let len = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let (mut file, mut done, total) = match status {
        206 if have > 0 => (std::fs::OpenOptions::new().append(true).open(dest)?, have, len.map(|l| l + have)),
        200 => (std::fs::File::create(dest)?, 0, len),
        416 if have > 0 => return Ok(()), // already complete
        s => return Err(net_err(url, format!("HTTP {s}"))),
    };
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf).map_err(|e| net_err(url, e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    file.flush()?;
    if let Some(t) = total {
        if done < t {
            return Err(net_err(url, format!("connection closed after {done} of {t} bytes")));
        }
    }
    Ok(())
}

pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p)?;
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

fn url_file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("download");
    let name: String = name.chars().map(|c| if c.is_alphanumeric() || "._-+ ".contains(c) { c } else { '_' }).collect();
    if name.is_empty() || name == "content" {
        "download".into()
    } else {
        name
    }
}

/// Unpack `archive` (of type `kind`) into `dest`. Raw files are copied as `file_name`.
pub fn extract(archive: &Path, kind: &str, dest: &Path, file_name: &str) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let bad = |e: &dyn std::fmt::Display| {
        Error::new(ErrorKind::Unsupported, "bad_archive", format!("cannot unpack {}: {e}", archive.display()))
    };
    match kind {
        "zip" => {
            let f = std::fs::File::open(archive)?;
            let mut z = zip::ZipArchive::new(f).map_err(|e| bad(&e))?;
            for i in 0..z.len() {
                let mut entry = z.by_index(i).map_err(|e| bad(&e))?;
                let Some(rel) = entry.enclosed_name() else {
                    continue; // path traversal attempt: skip
                };
                let out = dest.join(rel);
                if entry.is_dir() {
                    std::fs::create_dir_all(&out)?;
                    continue;
                }
                if let Some(p) = out.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let mut w = std::fs::File::create(&out)?;
                std::io::copy(&mut entry, &mut w)?;
                #[cfg(unix)]
                if let Some(mode) = entry.unix_mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777));
                }
            }
        }
        "tar.gz" | "tgz" | "tar.xz" | "txz" | "tar" => {
            let f = std::fs::File::open(archive)?;
            let r: Box<dyn Read> = match kind {
                "tar.gz" | "tgz" => Box::new(flate2::read::GzDecoder::new(f)),
                "tar.xz" | "txz" => Box::new(lzma_rust2::XzReader::new(f, true)),
                _ => Box::new(f),
            };
            let mut t = tar::Archive::new(r);
            for entry in t.entries().map_err(|e| bad(&e))? {
                let mut entry = entry.map_err(|e| bad(&e))?;
                // unpack_in refuses paths that escape `dest`
                entry.unpack_in(dest).map_err(|e| bad(&e))?;
            }
        }
        k if INSTALLERS.contains(&k) => {
            return Err(Error::unsupported(format!(
                "{} is an installer ({k}); ferraus never runs installers",
                archive.display()
            )))
        }
        _ => {
            std::fs::copy(archive, dest.join(file_name))?;
        }
    }
    Ok(())
}

fn move_path(from: &Path, to: &Path) -> Result<()> {
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    if to.exists() {
        remove_path(to)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_recursive(from, to)?;
    remove_path(from)
}

fn copy_recursive(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

fn remove_path(p: &Path) -> Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)?;
    } else if p.exists() || p.symlink_metadata().is_ok() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

/// If `dir` holds exactly one folder and nothing else, that folder (archives usually wrap
/// their content in one top-level directory).
fn content_root(dir: &Path) -> PathBuf {
    let entries: Vec<PathBuf> =
        std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    let visible: Vec<&PathBuf> = entries
        .iter()
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            !n.starts_with('.') && n != "__MACOSX"
        })
        .collect();
    if visible.len() == 1 && visible[0].is_dir() && PluginFormat::from_path(visible[0]).is_none() {
        visible[0].clone()
    } else {
        dir.to_path_buf()
    }
}

/// Move unpacked content from `staging` into place; returns the created paths.
fn place(env: &SoundsEnv, kind: SoundKind, id: &str, staging: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if kind.is_plugin() {
        let bundles = scan::find_bundles(staging, &[PluginFormat::Clap, PluginFormat::Vst3]);
        if bundles.is_empty() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "no_plugin_in_download",
                format!("the download for '{id}' contains no .clap or .vst3 bundle"),
            ));
        }
        for (f, b) in bundles {
            let dest = env.plugin_dir(f).join(b.file_name().unwrap());
            move_path(&b, &dest)?;
            paths.push(dest);
        }
    } else {
        let root = content_root(staging);
        let dest = env.sounds_dir.join(id);
        move_path(&root, &dest)?;
        paths.push(dest);
    }
    Ok(paths)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn record(env: &SoundsEnv, rec: InstalledRecord) -> Result<()> {
    let mut all = read_installed(env);
    all.retain(|r| r.id != rec.id);
    all.push(rec);
    all.sort_by(|a, b| a.id.cmp(&b.id));
    write_installed(env, &all)
}

fn rescan(env: &SoundsEnv, watchdog: Duration) {
    let opts = ScanOptions {
        extra: vec![env.clap_dir.clone(), env.vst3_dir.clone()],
        standard: false,
        watchdog,
        cache_path: env.scan_cache(),
        ..Default::default()
    };
    let _ = scan::scan(&opts);
}

/// Install a registry entry. Returns `None` for a dry run (nothing downloaded).
pub fn install_entry(
    env: &SoundsEnv,
    e: &Entry,
    opts: &InstallOptions,
    progress: &mut dyn FnMut(f64),
) -> Result<Option<InstalledRecord>> {
    if e.is_vendor_only() {
        let url = e.install_from.as_ref().map(|i| i.url.clone()).unwrap_or_default();
        return Err(Error::new(
            ErrorKind::Usage,
            "vendor_install_required",
            format!("{} is only distributed by its vendor ({url}); download it there, then register the file", e.name),
        )
        .with_hint(format!("ferraus sounds install {} --from <downloaded file>", e.id)));
    }
    let parts: Vec<&Download> = e.downloads_for(&opts.os);
    if parts.is_empty() {
        let mut err = Error::unsupported(format!("{} has no download for {}", e.name, opts.os));
        if let Some(i) = &e.install_from {
            err =
                err.with_hint(format!("get it from {} and run: ferraus sounds install {} --from <file>", i.url, e.id));
        }
        return Err(err);
    }
    if let Some(d) = parts.iter().find(|d| INSTALLERS.contains(&d.archive.as_str())) {
        return Err(Error::unsupported(format!(
            "{} is only available as an installer ({}) on {}; ferraus never runs installers",
            e.name, d.archive, opts.os
        ))
        .with_hint(format!(
            "install it yourself from {} , or download an archive and run: ferraus sounds install {} --from <file>",
            if e.homepage.is_empty() { &d.url } else { &e.homepage },
            e.id
        )));
    }
    if let Some(existing) = read_installed(env).into_iter().find(|r| r.id == e.id) {
        if existing.paths.iter().all(|p| p.exists()) {
            return Ok(Some(existing));
        }
    }
    let unverified = parts.iter().any(|d| d.sha256.as_deref().map(|s| s.len() != 64).unwrap_or(true));
    if unverified && !opts.allow_unverified {
        return Err(Error::new(
            ErrorKind::Verification,
            "sound_unverified",
            format!("{} has no pinned SHA-256 for {} in the registry, so ferraus will not install it", e.name, opts.os),
        )
        .with_hint(format!("FERRAUS_ALLOW_UNVERIFIED=1 ferraus sounds install {} -y", e.id)));
    }
    if opts.dry_run {
        return Ok(None);
    }
    if opts.offline {
        return Err(Error::new(
            ErrorKind::Network,
            "offline",
            format!("{} must be downloaded, but --offline is set", e.name),
        ));
    }
    if !opts.yes {
        let size: Option<u64> = e.size_for(&opts.os);
        return Err(Error::new(
            ErrorKind::Usage,
            "download_needs_consent",
            format!(
                "installing {} downloads {}; pass --yes to allow downloads (or enable auto_download in the config)",
                e.name,
                size.map(|s| format!("{:.1} MB", s as f64 / 1048576.0)).unwrap_or_else(|| "an unknown amount".into())
            ),
        )
        .with_hint(format!("ferraus sounds install {} -y", e.id)));
    }
    let total: u64 = parts.iter().filter_map(|d| d.size_bytes).sum();
    let mut base = 0u64;
    let staging = env.downloads_dir().join(format!("staging-{}-{}", e.id, std::process::id()));
    let _ = remove_path(&staging);
    let result = (|| -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for (i, d) in parts.iter().enumerate() {
            let tag = d.sha256.clone().unwrap_or_else(|| format!("{}-{i}", e.id));
            let part = env.downloads_dir().join(format!("{tag}.part"));
            let mut last_err = None;
            let mut ok = false;
            for url in std::iter::once(&d.url).chain(d.mirrors.iter()) {
                let mut cb = |done: u64, t: Option<u64>| {
                    let denom = if total > 0 { total } else { t.unwrap_or(0) };
                    if denom > 0 {
                        progress(((base + done) as f64 / denom as f64).min(1.0));
                    }
                };
                match download(url, &part, &mut cb) {
                    Ok(()) => {
                        ok = true;
                        break;
                    }
                    Err(err) => last_err = Some(err),
                }
            }
            if !ok {
                return Err(last_err.unwrap_or_else(|| net_err(&d.url, "no URL")));
            }
            let got = sha256_file(&part)?;
            if let Some(want) = &d.sha256 {
                if !got.eq_ignore_ascii_case(want) {
                    let _ = std::fs::remove_file(&part);
                    return Err(Error::new(
                        ErrorKind::Verification,
                        "checksum_mismatch",
                        format!(
                            "SHA-256 of {} does not match the registry (expected {want}, got {got}); the file was deleted and nothing was installed",
                            d.url
                        ),
                    ));
                }
            }
            base += std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            extract(&part, &d.archive, &staging, &url_file_name(&d.url))?;
            files.push(part);
        }
        let placed = place(env, e.kind, &e.id, &staging)?;
        for f in files {
            let _ = std::fs::remove_file(f);
        }
        Ok(placed)
    })();
    let _ = remove_path(&staging);
    let paths = result?;
    let rec = InstalledRecord {
        id: e.id.clone(),
        name: e.name.clone(),
        kind: e.kind,
        version: e.version.clone().unwrap_or_default(),
        license: e.license.spdx.clone(),
        paths,
        tags: e.tags.clone(),
        category: e.category.clone(),
        source: "registry".into(),
        installed_at: now(),
        sha256: parts.first().and_then(|d| d.sha256.clone()),
    };
    record(env, rec.clone())?;
    if e.kind.is_plugin() {
        rescan(env, opts.watchdog);
    }
    Ok(Some(rec))
}

fn infer_kind(p: &Path) -> Option<SoundKind> {
    let ext = p.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "clap" => Some(SoundKind::Clap),
        "vst3" => Some(SoundKind::Vst3),
        "sf2" | "sf3" => Some(SoundKind::Sf2),
        "sfz" => Some(SoundKind::Sfz),
        "wav" | "aif" | "aiff" | "flac" => Some(SoundKind::Ir),
        _ => None,
    }
}

fn archive_kind(p: &Path) -> Option<&'static str> {
    let n = p.file_name()?.to_string_lossy().to_ascii_lowercase();
    if n.ends_with(".zip") {
        Some("zip")
    } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        Some("tar.gz")
    } else if n.ends_with(".tar.xz") || n.ends_with(".txz") {
        Some("tar.xz")
    } else if n.ends_with(".tar") {
        Some("tar")
    } else {
        None
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// What a staged folder contains.
fn detect_kind(dir: &Path) -> Option<SoundKind> {
    let b = scan::find_bundles(dir, &[PluginFormat::Clap, PluginFormat::Vst3]);
    if let Some((f, _)) = b.first() {
        return Some(if *f == PluginFormat::Clap { SoundKind::Clap } else { SoundKind::Vst3 });
    }
    let files = crate::resolve::discover_files(dir, "user");
    if files.iter().any(|s| s.kind == SoundKind::Sf2) {
        Some(SoundKind::Sf2)
    } else if files.iter().any(|s| s.kind == SoundKind::Sfz) {
        Some(SoundKind::Sfz)
    } else {
        let any_audio = walkdir_has_audio(dir, 0);
        any_audio.then_some(SoundKind::Ir)
    }
}

fn walkdir_has_audio(d: &Path, depth: usize) -> bool {
    if depth > 6 {
        return false;
    }
    std::fs::read_dir(d)
        .map(|rd| {
            rd.flatten().any(|e| {
                let p = e.path();
                if p.is_dir() {
                    walkdir_has_audio(&p, depth + 1)
                } else {
                    infer_kind(&p) == Some(SoundKind::Ir)
                }
            })
        })
        .unwrap_or(false)
}

/// Register a file or folder the user downloaded: a bundle, `.sf2`, `.sfz` library folder,
/// IR folder, or a zip/tar archive of any of these. Never runs installers.
pub fn install_from(
    env: &SoundsEnv,
    from: &Path,
    entry: Option<&Entry>,
    name: Option<&str>,
    dry_run: bool,
) -> Result<InstalledRecord> {
    if !from.exists() {
        return Err(Error::new(ErrorKind::InputMissing, "input_not_found", format!("not found: {}", from.display())));
    }
    let ext = from.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if INSTALLERS.contains(&ext.as_str()) {
        return Err(Error::unsupported(format!(
            "{} is an installer; ferraus never runs installers. Run it yourself, then 'ferraus plugin scan' finds the plugin",
            from.display()
        ))
        .with_hint("ferraus plugin scan"));
    }
    let display = name.map(|s| s.to_string()).or_else(|| entry.map(|e| e.name.clone())).unwrap_or_else(|| {
        let n = from.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        n.split('.').next().unwrap_or(&n).to_string()
    });
    let id = entry.map(|e| e.id.clone()).unwrap_or_else(|| slug(&display));
    let staging = env.downloads_dir().join(format!("staging-from-{id}-{}", std::process::id()));
    let _ = remove_path(&staging);
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> Result<(SoundKind, Vec<PathBuf>)> {
        if let Some(k) = archive_kind(from) {
            extract(from, k, &staging, "")?;
        } else {
            let target = staging.join(from.file_name().unwrap());
            copy_recursive(from, &target)?;
        }
        let kind = entry
            .map(|e| e.kind)
            .filter(|k| !k.is_plugin() || detect_kind(&staging).map(|d| d.is_plugin()).unwrap_or(false))
            .or_else(|| detect_kind(&staging))
            .ok_or_else(|| {
                Error::unsupported(format!(
                    "{} contains no .clap/.vst3 bundle, .sf2, .sfz, or audio files",
                    from.display()
                ))
            })?;
        if dry_run {
            return Ok((kind, Vec::new()));
        }
        let paths = place(env, kind, &id, &staging)?;
        Ok((kind, paths))
    })();
    let _ = remove_path(&staging);
    let (kind, paths) = result?;
    let rec = InstalledRecord {
        id: id.clone(),
        name: display,
        kind,
        version: entry.and_then(|e| e.version.clone()).unwrap_or_default(),
        license: entry.map(|e| e.license.spdx.clone()).unwrap_or_else(|| "unknown".into()),
        paths,
        tags: entry.map(|e| e.tags.clone()).unwrap_or_default(),
        category: entry
            .and_then(|e| e.category.clone())
            .or_else(|| crate::resolve::infer_category(&id).map(|s| s.to_string())),
        source: "user".into(),
        installed_at: now(),
        sha256: if from.is_file() { sha256_file(from).ok() } else { None },
    };
    if !dry_run {
        record(env, rec.clone())?;
        if kind.is_plugin() {
            rescan(env, crate::client::default_watchdog());
        }
    }
    Ok(rec)
}

/// Remove an installed sound. Only paths inside ferraus' sounds folder or the per-user
/// plugin folders are deleted.
pub fn remove(env: &SoundsEnv, id: &str, dry_run: bool, watchdog: Duration) -> Result<Vec<PathBuf>> {
    let mut all = read_installed(env);
    let Some(i) = all.iter().position(|r| r.id == id || r.name.eq_ignore_ascii_case(id)) else {
        return Err(Error::usage(format!("'{id}' is not installed")).with_hint("ferraus sounds list"));
    };
    let rec = all[i].clone();
    let roots = [&env.sounds_dir, &env.clap_dir, &env.vst3_dir];
    let mut removed = Vec::new();
    for p in &rec.paths {
        let abs = ferraus_core::ctx::absolutize(p);
        if !roots.iter().any(|r| abs.starts_with(ferraus_core::ctx::absolutize(r))) {
            return Err(Error::usage(format!(
                "refusing to delete {}: it is outside ferraus' sounds and plugin folders",
                p.display()
            )));
        }
        if !dry_run {
            remove_path(p)?;
        }
        removed.push(p.clone());
    }
    if !dry_run {
        all.remove(i);
        write_installed(env, &all)?;
        if rec.kind.is_plugin() {
            rescan(env, watchdog);
        }
    }
    Ok(removed)
}
