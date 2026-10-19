//! Engraving (SVG, PDF, PNG).
//!
//! The default engine is Verovio, which is LGPL-3.0 and therefore runs in a separate
//! `ferraus-engrave` helper executable (Open question #16). Protocol:
//!
//! ```text
//! ferraus-engrave --to svg|pdf|png [--page N] [--scale 1.0] [--input musicxml|mei] < score.musicxml > out
//! ferraus-engrave --count < score.musicxml      # prints the page count
//! ferraus-engrave --version                     # "ferraus-engrave <v> verovio <v>"
//! ```
//!
//! The helper is found through `FERRAUS_ENGRAVE`, next to the running executable, or on
//! `PATH`. When it is missing, LilyPond on `PATH` is used (publication quality, GPL,
//! external). Otherwise an `unsupported` error explains how to get one.

use crate::model::Score;
use ferraus_core::{Error, ErrorKind, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub enum Engine {
    Helper(PathBuf),
    Lilypond(PathBuf),
}

impl Engine {
    pub fn name(&self) -> &'static str {
        match self {
            Engine::Helper(_) => "verovio",
            Engine::Lilypond(_) => "lilypond",
        }
    }
}

fn exe_name(n: &str) -> String {
    if cfg!(windows) {
        format!("{n}.exe")
    } else {
        n.to_string()
    }
}

/// Search `PATH`-like strings for an executable.
pub fn find_in(path_var: Option<&std::ffi::OsStr>, name: &str) -> Option<PathBuf> {
    let exe = exe_name(name);
    path_var.and_then(|p| std::env::split_paths(p).map(|d| d.join(&exe)).find(|c| c.is_file()))
}

/// Locate the Verovio helper: `$FERRAUS_ENGRAVE`, beside the current executable (and its
/// parent, for `target/debug/deps` test binaries), then `PATH`.
pub fn find_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FERRAUS_ENGRAVE").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for d in [Some(dir), dir.parent()].into_iter().flatten() {
                let c = d.join(exe_name("ferraus-engrave"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    find_in(std::env::var_os("PATH").as_deref(), "ferraus-engrave")
}

pub fn find_lilypond() -> Option<PathBuf> {
    find_in(std::env::var_os("PATH").as_deref(), "lilypond")
}

/// Pick an engine: `auto` (helper, then LilyPond), `verovio`, or `lilypond`.
pub fn choose(pref: &str) -> Result<Engine> {
    let helper = || find_helper().map(Engine::Helper);
    let lily = || find_lilypond().map(Engine::Lilypond);
    let found = match pref {
        "verovio" => helper(),
        "lilypond" => lily(),
        _ => helper().or_else(lily),
    };
    found.ok_or_else(|| missing_error(pref))
}

fn missing_error(pref: &str) -> Error {
    let what = match pref {
        "verovio" => "the ferraus-engrave helper (Verovio) was not found",
        "lilypond" => "LilyPond was not found on PATH",
        _ => "no engraver found: neither the ferraus-engrave helper (Verovio) nor LilyPond is available",
    };
    Error::new(ErrorKind::Unsupported, "engraver_missing", format!("{what}; SVG, PDF, and PNG output need one"))
        .with_hint("install ferraus-engrave beside ferraus (release packages include it; from source: cargo build -p ferraus-engrave --release), set FERRAUS_ENGRAVE=/path/to/ferraus-engrave, or install LilyPond; MusicXML/ABC/LilyPond/solfa outputs work without it")
}

/// Engraved pages (one entry per page; a PDF is a single multi-page document).
pub struct Engraved {
    pub engine: String,
    pub pages: Vec<Vec<u8>>,
}

/// Engrave a score to `svg`, `pdf`, or `png`.
pub fn engrave(score: &Score, to: &str, pref: &str) -> Result<Engraved> {
    let engine = choose(pref)?;
    match &engine {
        Engine::Helper(p) => {
            let xml = crate::musicxml::write(score);
            let pages = run_helper(p, &xml, to)?;
            Ok(Engraved { engine: "verovio".into(), pages })
        }
        Engine::Lilypond(p) => {
            let ly = crate::lily::write(score);
            let pages = run_lilypond(p, &ly, to)?;
            Ok(Engraved { engine: "lilypond".into(), pages })
        }
    }
}

fn helper_call(exe: &Path, args: &[&str], input: &str) -> Result<Vec<u8>> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(ErrorKind::Unsupported, "engraver_failed", format!("cannot start {}: {e}", exe.display())))?;
    {
        let mut stdin = child.stdin.take().unwrap();
        // the helper may exit early on bad input; ignore broken pipes here and report its stderr
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::processing(format!("engraver failed: {e}")))?;
    if !out.status.success() {
        return Err(Error::processing(format!(
            "ferraus-engrave failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

pub fn run_helper(exe: &Path, xml: &str, to: &str) -> Result<Vec<Vec<u8>>> {
    if to == "pdf" {
        return Ok(vec![helper_call(exe, &["--to", "pdf"], xml)?]);
    }
    let count = String::from_utf8_lossy(&helper_call(exe, &["--count"], xml)?).trim().parse::<u32>().unwrap_or(1).max(1);
    let mut pages = Vec::new();
    for p in 1..=count {
        let ps = p.to_string();
        pages.push(helper_call(exe, &["--to", to, "--page", &ps], xml)?);
    }
    Ok(pages)
}

fn run_lilypond(exe: &Path, ly: &str, to: &str) -> Result<Vec<Vec<u8>>> {
    let dir = std::env::temp_dir().join(format!("ferraus-ly-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(&dir)?;
    let src = dir.join("score.ly");
    std::fs::write(&src, ly)?;
    let flag = match to {
        "svg" => "--svg",
        "png" => "--png",
        _ => "--pdf",
    };
    let out = Command::new(exe)
        .arg(flag)
        .arg("-dno-point-and-click")
        .arg("-o")
        .arg(dir.join("score"))
        .arg(&src)
        .current_dir(&dir)
        .output()
        .map_err(|e| Error::processing(format!("cannot run lilypond: {e}")))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(Error::processing(format!("lilypond failed: {}", String::from_utf8_lossy(&out.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | "))));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == to).unwrap_or(false))
        .collect();
    files.sort();
    let pages = files.iter().map(std::fs::read).collect::<std::io::Result<Vec<_>>>()?;
    let _ = std::fs::remove_dir_all(&dir);
    if pages.is_empty() {
        return Err(Error::processing("lilypond produced no output"));
    }
    Ok(pages)
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// Output paths for engraved pages: page 1 at `path`, later pages as `stem-2.ext`.
pub fn page_paths(path: &Path, n: usize) -> Vec<PathBuf> {
    let mut v = vec![path.to_path_buf()];
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    for i in 2..=n {
        v.push(path.with_file_name(format!("{stem}-{i}.{ext}")));
    }
    v
}

/// Status for diagnostics.
pub fn status() -> serde_json::Value {
    let helper = find_helper();
    let version = helper.as_ref().and_then(|h| {
        Command::new(h)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    serde_json::json!({
        "helper": helper.map(|p| p.display().to_string()),
        "helper_version": version,
        "lilypond": find_lilypond().map(|p| p.display().to_string()),
    })
}
