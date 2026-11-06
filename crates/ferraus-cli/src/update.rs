//! `update` (alias `self-update`): check for a newer release and update ferraus the way it was
//! installed.
//!
//! The release is read from the `dist-manifest.json` that cargo-dist attaches to every GitHub
//! release (no API token, no rate limit). A copy placed by the shell or PowerShell installer is
//! updated by running that release's own installer, pinned to the version just checked, into
//! the folder its install receipt names; the installer verifies each archive against the
//! SHA-256 it carries. Homebrew, apt, dnf, scoop, cargo, MSI, and container installs belong to
//! their package manager: `update` reports the command that updates them and changes nothing.

use ferraus_core::prelude::*;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

const RELEASES: &str = "https://github.com/djynnius/ferraus/releases";
/// Apps the installers can place next to `ferraus`, each with its own installer and receipt.
const COMPANIONS: &[&str] = &["ferraus-plughost-vst3", "ferraus-engrave"];
const MANIFEST_LIMIT: u64 = 4 << 20;

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::custom("update", Category::System, "Check for a newer release and update ferraus", update)
        .alias("self-update")
        .about(
            "Reads the latest release (or --version) from GitHub and compares it with this binary. \
             A copy installed by the release installer (curl ... | sh, irm ... | iex) is updated by \
             that release's installer, pinned to the version just checked and written to the \
             folder the install receipt names, together with the VST3 host and engraver when the \
             installers placed them too. Homebrew, apt, dnf, scoop, cargo, MSI, and container \
             installs are left to their package manager: the result names the command \
             (`command`) and nothing changes. Never runs on its own; --check only reports.",
        )
        .inputs(Inputs::NONE)
        .param(
            param("check", ParamKind::Flag)
                .help("Only report the current and newest version and how to update; change nothing"),
        )
        .param(
            param("version", ParamKind::Str)
                .help("Install this release instead of the newest, e.g. 0.1.4 (an older one goes back)"),
        )
        .example(
            "ferraus update --check",
            "Is a newer release out? Shows both versions, its notes, and the update command",
        )
        .example("ferraus update", "Update an installer copy, or print the Homebrew/apt/dnf/scoop/cargo command")
        .example("ferraus update --version 0.1.3", "Go back to (or pin) a specific release")
        .example("ferraus update --dry-run --json", "Show the installer that would run and where, without running it")]
}

/// How this copy of ferraus was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Installer,
    Homebrew,
    Scoop,
    Cargo,
    Apt,
    Dnf,
    Msi,
    Container,
    Unknown,
}

impl Method {
    fn id(self) -> &'static str {
        match self {
            Method::Installer => "installer",
            Method::Homebrew => "homebrew",
            Method::Scoop => "scoop",
            Method::Cargo => "cargo",
            Method::Apt => "apt",
            Method::Dnf => "dnf",
            Method::Msi => "msi",
            Method::Container => "container",
            Method::Unknown => "unknown",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Method::Installer => "the release installer",
            Method::Homebrew => "Homebrew",
            Method::Scoop => "scoop",
            Method::Cargo => "cargo install",
            Method::Apt => "apt",
            Method::Dnf => "dnf",
            Method::Msi => "the MSI installer",
            Method::Container => "a container image",
            Method::Unknown => "a method ferraus cannot tell (a source build?)",
        }
    }
}

/// What detection looks at; filled from the real system by [`gather`] and by hand in tests.
pub(crate) struct Facts<'a> {
    pub exe: &'a Path,
    pub install_prefix: Option<&'a Path>,
    pub cargo_bin: Option<&'a Path>,
    pub dpkg_owned: bool,
    pub rpm_present: bool,
    pub container: bool,
    pub windows: bool,
}

pub(crate) fn detect(f: &Facts) -> Method {
    let dir = f.exe.parent();
    if let (Some(d), Some(p)) = (dir, f.install_prefix) {
        if same_path(d, p) {
            return Method::Installer;
        }
    }
    if f.container {
        return Method::Container;
    }
    let s = f.exe.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    if s.contains("/cellar/ferraus/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        return Method::Homebrew;
    }
    if s.contains("/scoop/apps/ferraus/") || s.contains("/scoop/shims/") {
        return Method::Scoop;
    }
    if let (Some(d), Some(c)) = (dir, f.cargo_bin) {
        if same_path(d, c) {
            return Method::Cargo;
        }
    }
    if f.windows && (s.contains("/program files/") || s.contains("/program files (x86)/")) {
        return Method::Msi;
    }
    if !f.windows && s == "/usr/bin/ferraus" {
        if f.dpkg_owned {
            return Method::Apt;
        }
        if f.rpm_present {
            return Method::Dnf;
        }
    }
    Method::Unknown
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// The command that updates a non-installer copy to `target`.
pub(crate) fn manual_command(m: Method, target: &str, base: &str, windows: bool) -> String {
    let arch = if std::env::consts::ARCH == "aarch64" { "aarch64" } else { "x86_64" };
    match m {
        Method::Installer => "ferraus update".into(),
        Method::Homebrew => "brew upgrade djynnius/tap/ferraus".into(),
        Method::Scoop => "scoop update ferraus".into(),
        Method::Cargo => format!("cargo install ferraus --locked --version {target}"),
        Method::Apt => "sudo apt update && sudo apt install --only-upgrade ferraus".into(),
        Method::Dnf => "sudo dnf upgrade --refresh ferraus".into(),
        Method::Msi => format!("msiexec /i {base}/download/v{target}/ferraus-{arch}-pc-windows-msvc.msi"),
        Method::Container => format!("docker pull ghcr.io/djynnius/ferraus:v{target}"),
        Method::Unknown if windows => format!(
            "powershell -ExecutionPolicy ByPass -c \"irm {base}/download/v{target}/ferraus-installer.ps1 | iex\""
        ),
        Method::Unknown => {
            format!("curl --proto '=https' --tlsv1.2 -LsSf {base}/download/v{target}/ferraus-installer.sh | sh")
        }
    }
}

/// A release as `dist-manifest.json` describes it.
#[derive(Debug, PartialEq)]
pub(crate) struct Release {
    pub version: String,
    pub tag: String,
    pub title: String,
    pub notes: String,
    /// Version of every app in the release (ferraus and its companions).
    pub apps: Vec<(String, String)>,
}

pub(crate) fn parse_release(bytes: &[u8]) -> Result<Release> {
    let bad = |m: String| Error::new(ErrorKind::Network, "bad_release_manifest", m);
    let v: Value = serde_json::from_slice(bytes).map_err(|e| bad(format!("the release manifest is not JSON: {e}")))?;
    let apps: Vec<(String, String)> = v["releases"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| Some((r["app_name"].as_str()?.to_string(), r["app_version"].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let version = apps
        .iter()
        .find(|(n, _)| n == "ferraus")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| bad("the release manifest lists no ferraus release".into()))?;
    if parse_version(&version).is_none() {
        return Err(bad(format!("the release manifest names an unreadable version '{version}'")));
    }
    Ok(Release {
        tag: v["announcement_tag"].as_str().map(str::to_string).unwrap_or_else(|| format!("v{version}")),
        title: v["announcement_title"].as_str().unwrap_or("").to_string(),
        notes: v["announcement_changelog"].as_str().unwrap_or("").to_string(),
        version,
        apps,
    })
}

/// `1.2.3` or `1.2.3-rc.1` (a leading `v` is accepted) as numbers plus a pre-release flag.
pub(crate) fn parse_version(s: &str) -> Option<(u64, u64, u64, bool)> {
    let s = s.trim().trim_start_matches('v');
    let (core, pre) = match s.split_once(['-', '+']) {
        Some((c, rest)) => (c, s[c.len()..].starts_with('-') && !rest.is_empty()),
        None => (s, false),
    };
    let mut it = core.split('.');
    let v = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    if it.next().is_some() {
        return None;
    }
    Some((v.0, v.1, v.2, pre))
}

/// Order two versions; a pre-release sorts before the release with the same numbers.
pub(crate) fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let (x, y) = (parse_version(a)?, parse_version(b)?);
    Some((x.0, x.1, x.2).cmp(&(y.0, y.1, y.2)).then_with(|| y.3.cmp(&x.3)))
}

/// An install receipt written by a cargo-dist installer.
#[derive(Debug)]
struct Receipt {
    prefix: PathBuf,
    version: String,
    modify_path: bool,
}

/// Where the installers keep `<app>-receipt.json`: `$XDG_CONFIG_HOME` or `~/.config` on macOS
/// and Linux, `$XDG_CONFIG_HOME` or `%LOCALAPPDATA%` on Windows, in a folder named after the app.
fn receipt_path(app: &str) -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let root = env("XDG_CONFIG_HOME").or_else(|| {
        if cfg!(windows) {
            env("LOCALAPPDATA")
        } else {
            env("HOME").map(|h| h.join(".config"))
        }
    })?;
    Some(root.join(app).join(format!("{app}-receipt.json")))
}

fn read_receipt(app: &str) -> Option<Receipt> {
    let text = std::fs::read_to_string(receipt_path(app)?).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(Receipt {
        prefix: PathBuf::from(v["install_prefix"].as_str()?),
        version: v["version"].as_str().unwrap_or("").to_string(),
        modify_path: v["modify_path"].as_bool().unwrap_or(true),
    })
}

fn releases_base() -> String {
    std::env::var("FERRAUS_UPDATE_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| RELEASES.to_string())
}

fn exe_name(app: &str) -> String {
    format!("{app}{}", std::env::consts::EXE_SUFFIX)
}

fn installer_url(base: &str, app: &str, version: &str) -> String {
    let ext = if cfg!(windows) { "ps1" } else { "sh" };
    format!("{base}/download/v{version}/{app}-installer.{ext}")
}

/// `FERRAUS_PLUGHOST_VST3` for `ferraus-plughost-vst3`: the installers' per-app variable prefix.
fn env_prefix(app: &str) -> String {
    app.to_ascii_uppercase().replace('-', "_")
}

/// The installer run for one app: its URL, folder, and environment.
fn plan_entry(base: &str, app: &str, version: &str, r: &Receipt) -> Value {
    let p = env_prefix(app);
    let mut env = serde_json::Map::new();
    env.insert(format!("{p}_INSTALL_DIR"), json!(r.prefix.display().to_string()));
    if !r.modify_path {
        env.insert(format!("{p}_NO_MODIFY_PATH"), json!("1"));
    }
    json!({
        "app": app,
        "from": r.version,
        "to": version,
        "installer": installer_url(base, app, version),
        "install_dir": r.prefix.display().to_string(),
        "env": env,
    })
}

fn gather(exe: &Path, receipt: Option<&Receipt>) -> Method {
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        })
        .map(|c| c.join("bin"));
    detect(&Facts {
        exe,
        install_prefix: receipt.map(|r| r.prefix.as_path()),
        cargo_bin: cargo_bin.as_deref(),
        dpkg_owned: Path::new("/var/lib/dpkg/info/ferraus.list").exists(),
        rpm_present: Path::new("/usr/bin/rpm").exists() || Path::new("/bin/rpm").exists(),
        container: Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists(),
        windows: cfg!(windows),
    })
}

fn fetch_release(base: &str, pinned: Option<&str>) -> Result<Release> {
    let url = match pinned {
        Some(v) => format!("{base}/download/v{v}/dist-manifest.json"),
        None => format!("{base}/latest/download/dist-manifest.json"),
    };
    let bytes = ferraus_models::download::get_bytes(&url, MANIFEST_LIMIT).map_err(|e| match pinned {
        Some(v) if e.message.contains("HTTP 404") => Error::usage(format!("there is no ferraus release {v}"))
            .with_hint(format!("open {RELEASES} for the list of releases")),
        _ => e,
    })?;
    parse_release(&bytes)
}

fn update(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    if ctx.globals.offline {
        return Err(Error::new(ErrorKind::Network, "offline", "update needs the network and --offline is set"));
    }
    let base = releases_base();
    let current = ferraus_core::FERRAUS_VERSION;
    let pinned = a.str("version").map(|v| v.trim().trim_start_matches('v').to_string()).filter(|v| !v.is_empty());
    if let Some(v) = &pinned {
        if parse_version(v).is_none() {
            return Err(Error::usage(format!("--version {v} is not a version like 0.1.4")));
        }
    }
    let release = fetch_release(&base, pinned.as_deref())?;
    let target = release.version.clone();
    let order = compare_versions(current, &target).unwrap_or(Ordering::Less);
    let newer = order == Ordering::Less;

    let exe = std::env::current_exe().map(|p| p.canonicalize().unwrap_or(p)).unwrap_or_default();
    let receipt = read_receipt("ferraus");
    let method = gather(&exe, receipt.as_ref());
    let command = manual_command(method, &target, &base, cfg!(windows));
    let release_url = format!("{base}/tag/{}", release.tag);

    let mut out = json!({
        "current": current,
        "target": target,
        "update_available": newer,
        "pinned": pinned.is_some(),
        "release": {"tag": release.tag, "title": release.title, "url": release_url, "notes": release.notes},
        "install": {"method": method.id(), "path": exe.display().to_string()},
        "command": command,
    });

    let wanted = match &pinned {
        Some(_) => order != Ordering::Equal,
        None => newer,
    };
    if a.flag("check") || !wanted {
        out["action"] = json!("none");
        let text = if !wanted {
            format!("ferraus {current} is up to date (newest release: {target}).")
        } else if newer {
            format!(
                "ferraus {target} is available; this is {current}, installed with {}.\nUpdate: {command}\nRelease notes: {release_url}",
                method.label()
            )
        } else {
            format!(
                "ferraus {target} is older than this {current}; `ferraus update --version {target}` goes back to it."
            )
        };
        out["text"] = json!(text);
        return Ok(out);
    }

    let receipt = match (method, receipt) {
        (Method::Installer, Some(r)) => r,
        _ => {
            return Err(Error::new(
                ErrorKind::Usage,
                "managed_install",
                format!(
                    "ferraus {current} at {} was installed with {}; update it with that tool ({target} is the {} release)",
                    exe.display(),
                    method.label(),
                    if pinned.is_some() { "requested" } else { "newest" }
                ),
            )
            .with_hint(command));
        }
    };

    // The installer copy, then any companion the installers placed.
    let mut plan = vec![("ferraus".to_string(), plan_entry(&base, "ferraus", &target, &receipt), receipt)];
    for app in COMPANIONS {
        let Some(r) = read_receipt(app) else { continue };
        let Some(version) = release.apps.iter().find(|(n, _)| n == app).map(|(_, v)| v.clone()) else { continue };
        if r.version != version {
            plan.push((app.to_string(), plan_entry(&base, app, &version, &r), r));
        }
    }
    out["plan"] = json!(plan.iter().map(|(_, e, _)| e.clone()).collect::<Vec<_>>());
    if ctx.globals.dry_run {
        out["action"] = json!("would_update");
        out["text"] = json!(format!(
            "Would update ferraus {current} to {target} with {} into {}.",
            installer_url(&base, "ferraus", &target),
            plan[0].2.prefix.display()
        ));
        return Ok(out);
    }

    remove_stale_old(&plan[0].2.prefix);
    let mut updated = Vec::new();
    for (app, entry, r) in &plan {
        run_installer(app, entry, r, ctx)?;
        updated.push(app.clone());
    }
    let installed = installed_version(&plan[0].2.prefix)?;
    if compare_versions(&installed, &target) != Some(Ordering::Equal) {
        return Err(Error::new(
            ErrorKind::Verification,
            "update_unverified",
            format!(
                "the installer finished but {} reports {installed}, not {target}",
                plan[0].2.prefix.join(exe_name("ferraus")).display()
            ),
        )
        .with_hint(installer_command(&installer_url(&base, "ferraus", &target))));
    }
    out["action"] = json!("updated");
    out["installed"] = json!(installed);
    out["updated"] = json!(updated);
    out["text"] = json!(format!(
        "Updated ferraus {current} -> {installed} in {}.\nRelease notes: {release_url}",
        plan[0].2.prefix.display()
    ));
    Ok(out)
}

fn installer_command(url: &str) -> String {
    if cfg!(windows) {
        format!("powershell -ExecutionPolicy ByPass -c \"irm {url} | iex\"")
    } else {
        format!("curl --proto '=https' --tlsv1.2 -LsSf {url} | sh")
    }
}

/// Windows cannot overwrite a running executable but can rename it: the old `ferraus.exe`
/// moves aside before the installer runs and is removed on the next update.
fn remove_stale_old(prefix: &Path) {
    if cfg!(windows) {
        let _ = std::fs::remove_file(prefix.join(format!("{}.old", exe_name("ferraus"))));
    }
}

fn run_installer(app: &str, entry: &Value, r: &Receipt, ctx: &mut OpCtx) -> Result<()> {
    let url = entry["installer"].as_str().unwrap_or_default();
    let failed = |m: String| Error::new(ErrorKind::Network, "update_failed", m).with_hint(installer_command(url));
    let script = ferraus_models::download::get_bytes(url, MANIFEST_LIMIT)?;
    let dir = std::env::temp_dir().join(format!("ferraus-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(if cfg!(windows) { "installer.ps1" } else { "installer.sh" });
    std::fs::write(&path, &script)?;

    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("powershell");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(&path);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg(&path);
        c
    };
    if let Some(env) = entry["env"].as_object() {
        for (k, v) in env {
            cmd.env(k, v.as_str().unwrap_or_default());
        }
    }

    // Move the running ferraus.exe aside on Windows so the installer can write the new one.
    let running = r.prefix.join(exe_name("ferraus"));
    let aside = r.prefix.join(format!("{}.old", exe_name("ferraus")));
    let moved = cfg!(windows) && app == "ferraus" && running.exists() && std::fs::rename(&running, &aside).is_ok();

    ctx.note("installer", url);
    let output = cmd.output();
    let _ = std::fs::remove_dir_all(&dir);
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            if moved {
                let _ = std::fs::rename(&aside, &running);
            }
            return Err(failed(format!("could not run the {app} installer: {e}")));
        }
    };
    // The installer's messages are logs: stderr, so stdout keeps only the result.
    {
        use std::io::Write;
        let _ = std::io::stderr().write_all(&output.stdout);
        let _ = std::io::stderr().write_all(&output.stderr);
    }
    if !output.status.success() {
        if moved && !running.exists() {
            let _ = std::fs::rename(&aside, &running);
        }
        let tail: String = String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(failed(format!("the {app} installer exited with {}: {tail}", output.status)));
    }
    Ok(())
}

/// Ask the freshly installed binary for its version.
fn installed_version(prefix: &Path) -> Result<String> {
    let bin = prefix.join(exe_name("ferraus"));
    let out = std::process::Command::new(&bin).arg("--version").output().map_err(|e| {
        Error::new(ErrorKind::Verification, "update_unverified", format!("could not run {}: {e}", bin.display()))
    })?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(1).map(str::to_string).ok_or_else(|| {
        Error::new(ErrorKind::Verification, "update_unverified", format!("{} printed no version", bin.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(exe: &str) -> Facts<'_> {
        Facts {
            exe: Path::new(exe),
            install_prefix: None,
            cargo_bin: None,
            dpkg_owned: false,
            rpm_present: false,
            container: false,
            windows: false,
        }
    }

    #[test]
    fn versions_order_numerically_with_prereleases_first() {
        assert_eq!(compare_versions("0.1.2", "0.1.4"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.1.10", "0.1.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("v1.0.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.0.0-rc.1", "1.0.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("1.0.0+build.5", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.0", "1.0.0"), None);
        assert_eq!(compare_versions("1.0.0.0", "1.0.0"), None);
    }

    #[test]
    fn detects_each_install_method() {
        let prefix = Path::new("/home/a/.ferraus/bin");
        let f = Facts { install_prefix: Some(prefix), ..facts("/home/a/.ferraus/bin/ferraus") };
        assert_eq!(detect(&f), Method::Installer);
        // a receipt for another folder does not make this copy an installer copy
        let f = Facts { install_prefix: Some(prefix), ..facts("/opt/homebrew/Cellar/ferraus/0.1.4/bin/ferraus") };
        assert_eq!(detect(&f), Method::Homebrew);
        assert_eq!(detect(&facts("/home/linuxbrew/.linuxbrew/Cellar/ferraus/0.1.4/bin/ferraus")), Method::Homebrew);
        let f = Facts { windows: true, ..facts("C:\\Users\\a\\scoop\\apps\\ferraus\\current\\ferraus.exe") };
        assert_eq!(detect(&f), Method::Scoop);
        let cargo = Path::new("/home/a/.cargo/bin");
        let f = Facts { cargo_bin: Some(cargo), ..facts("/home/a/.cargo/bin/ferraus") };
        assert_eq!(detect(&f), Method::Cargo);
        let f = Facts { dpkg_owned: true, rpm_present: true, ..facts("/usr/bin/ferraus") };
        assert_eq!(detect(&f), Method::Apt);
        let f = Facts { rpm_present: true, ..facts("/usr/bin/ferraus") };
        assert_eq!(detect(&f), Method::Dnf);
        let f = Facts { windows: true, ..facts("C:\\Program Files\\ferraus\\bin\\ferraus.exe") };
        assert_eq!(detect(&f), Method::Msi);
        let f = Facts { container: true, ..facts("/usr/local/bin/ferraus") };
        assert_eq!(detect(&f), Method::Container);
        assert_eq!(detect(&facts("/home/a/src/ferraus/target/release/ferraus")), Method::Unknown);
        assert_eq!(detect(&facts("/usr/bin/ferraus")), Method::Unknown);
    }

    #[test]
    fn commands_name_the_right_tool() {
        let b = RELEASES;
        assert_eq!(manual_command(Method::Homebrew, "0.1.5", b, false), "brew upgrade djynnius/tap/ferraus");
        assert_eq!(manual_command(Method::Cargo, "0.1.5", b, false), "cargo install ferraus --locked --version 0.1.5");
        assert!(
            manual_command(Method::Msi, "0.1.5", b, true)
                .ends_with("/download/v0.1.5/ferraus-x86_64-pc-windows-msvc.msi")
                || manual_command(Method::Msi, "0.1.5", b, true)
                    .ends_with("/download/v0.1.5/ferraus-aarch64-pc-windows-msvc.msi")
        );
        assert_eq!(manual_command(Method::Container, "0.1.5", b, false), "docker pull ghcr.io/djynnius/ferraus:v0.1.5");
        assert!(
            manual_command(Method::Unknown, "0.1.5", b, false).contains("/download/v0.1.5/ferraus-installer.sh | sh")
        );
        assert!(
            manual_command(Method::Unknown, "0.1.5", b, true).contains("/download/v0.1.5/ferraus-installer.ps1 | iex")
        );
    }

    #[test]
    fn reads_the_dist_manifest() {
        let m = br####"{"announcement_tag":"v0.1.5","announcement_title":"0.1.5 - 2026-12-01",
            "announcement_changelog":"### Added\n- update","releases":[
            {"app_name":"ferraus","app_version":"0.1.5"},{"app_name":"ferraus-engrave","app_version":"0.1.5"}]}"####;
        let r = parse_release(m).unwrap();
        assert_eq!(r.version, "0.1.5");
        assert_eq!(r.tag, "v0.1.5");
        assert_eq!(r.notes, "### Added\n- update");
        assert_eq!(r.apps.len(), 2);
        assert_eq!(parse_release(br#"{"releases":[]}"#).unwrap_err().code, "bad_release_manifest");
        assert_eq!(parse_release(b"<html>").unwrap_err().code, "bad_release_manifest");
    }

    #[test]
    fn installer_env_uses_the_per_app_variables() {
        let r = Receipt { prefix: PathBuf::from("/opt/ferraus/bin"), version: "0.1.2".into(), modify_path: false };
        let e = plan_entry("https://example.test/releases", "ferraus-plughost-vst3", "0.1.5", &r);
        assert_eq!(e["env"]["FERRAUS_PLUGHOST_VST3_INSTALL_DIR"], "/opt/ferraus/bin");
        assert_eq!(e["env"]["FERRAUS_PLUGHOST_VST3_NO_MODIFY_PATH"], "1");
        assert!(e["installer"]
            .as_str()
            .unwrap()
            .starts_with("https://example.test/releases/download/v0.1.5/ferraus-plughost-vst3-installer."));
        let r = Receipt { modify_path: true, ..r };
        assert!(plan_entry("b", "ferraus", "0.1.5", &r)["env"].get("FERRAUS_NO_MODIFY_PATH").is_none());
    }
}
