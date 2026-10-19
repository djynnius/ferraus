//! The sound resolver: turns an instrument name (`piano`, `strings`, `plugin:"Surge XT"`,
//! `sf2:/path/gm.sf2`) into a concrete sound, in the PRD's order:
//!
//! 1. an explicit source (`plugin:`, `sf2:`, `sfz:`, `ir:`),
//! 2. an alias from `<config>/sounds/aliases.toml` (`[aliases] piano = "<id or source>"`),
//! 3. an installed or scanned sound whose name, id, or tags match.
//!
//! General MIDI program names (`gm:48`) are left to the caller (`ferraus-synth`), which
//! gets `Ok(None)` when nothing here matches. Sounds come from the installed sounds folder,
//! `--sound-path` folders (CLAP/VST3 bundles, `.sf2`, `.sfz` libraries, impulse responses),
//! and the plugin scan cache.

use crate::api::PluginRef;
use crate::client::PluginFormat;
use crate::scan::{self, ScanCache, ScanOptions};
use ferraus_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Kind of sound source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum SoundKind {
    Clap,
    Vst3,
    Sf2,
    Sfz,
    Ir,
}

impl SoundKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SoundKind::Clap => "clap",
            SoundKind::Vst3 => "vst3",
            SoundKind::Sf2 => "sf2",
            SoundKind::Sfz => "sfz",
            SoundKind::Ir => "ir",
        }
    }

    pub fn parse(s: &str) -> Option<SoundKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "clap" => Some(SoundKind::Clap),
            "vst3" => Some(SoundKind::Vst3),
            "sf2" | "soundfont" => Some(SoundKind::Sf2),
            "sfz" => Some(SoundKind::Sfz),
            "ir" | "impulse" => Some(SoundKind::Ir),
            _ => None,
        }
    }

    pub fn is_plugin(self) -> bool {
        matches!(self, SoundKind::Clap | SoundKind::Vst3)
    }
}

/// One usable sound.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sound {
    /// Stable identifier: the plugin id, or a slug of the file name.
    pub id: String,
    pub name: String,
    pub kind: SoundKind,
    /// The bundle, `.sf2`, `.sfz`, or IR file.
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Plays notes (instrument plugins, SoundFonts, SFZ); IRs and effects do not.
    pub instrument: bool,
    /// One of keys, strings, guitar, bass, drums, synth, pad, brass, winds, vocal, fx.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `installed`, `sound-path`, or `plugin-scan`.
    pub origin: String,
    /// The registry entry this came from, when installed from the registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_id: Option<String>,
}

impl Sound {
    /// The explicit source string for this sound (`sf2:/path`, `plugin:/path#id`), usable
    /// anywhere an instrument name is accepted.
    pub fn source(&self) -> String {
        match self.kind {
            SoundKind::Clap | SoundKind::Vst3 => match &self.plugin_id {
                Some(id) => format!("plugin:{}#{id}", self.path.display()),
                None => format!("plugin:{}", self.path.display()),
            },
            k => format!("{}:{}", k.as_str(), self.path.display()),
        }
    }

    /// For plugin sounds, the reference to hand to [`crate::render_instrument`].
    pub fn plugin_ref(&self) -> Option<PluginRef> {
        let format = match self.kind {
            SoundKind::Clap => PluginFormat::Clap,
            SoundKind::Vst3 => PluginFormat::Vst3,
            _ => return None,
        };
        Some(PluginRef {
            format,
            path: self.path.clone(),
            plugin_id: self.plugin_id.clone(),
            name: self.name.clone(),
            vendor: String::new(),
            version: String::new(),
        })
    }
}

/// Where the resolver and the sounds registry keep their files. [`SoundsEnv::from_env`]
/// uses the standard ferraus folders; tests build one on a temp dir.
#[derive(Clone, Debug)]
pub struct SoundsEnv {
    pub config_dir: PathBuf,
    /// `<data>/sounds`: installed SoundFonts, SFZ libraries, IRs, and `installed.json`.
    pub sounds_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub clap_dir: PathBuf,
    pub vst3_dir: PathBuf,
}

impl SoundsEnv {
    pub fn from_env() -> SoundsEnv {
        SoundsEnv {
            config_dir: ferraus_core::paths::config_dir(),
            sounds_dir: ferraus_core::paths::sounds_dir(),
            cache_dir: ferraus_core::paths::cache_dir(),
            clap_dir: scan::user_plugin_dir(PluginFormat::Clap),
            vst3_dir: scan::user_plugin_dir(PluginFormat::Vst3),
        }
    }

    /// Everything under one root (for tests and sandboxes).
    pub fn under(root: &Path) -> SoundsEnv {
        SoundsEnv {
            config_dir: root.join("config"),
            sounds_dir: root.join("data").join("sounds"),
            cache_dir: root.join("cache"),
            clap_dir: root.join("plugins").join("clap"),
            vst3_dir: root.join("plugins").join("vst3"),
        }
    }

    pub fn aliases_file(&self) -> PathBuf {
        self.config_dir.join("sounds").join("aliases.toml")
    }

    pub fn installed_file(&self) -> PathBuf {
        self.sounds_dir.join("installed.json")
    }

    pub fn scan_cache(&self) -> PathBuf {
        self.cache_dir.join("plugins").join("scan.json")
    }

    pub fn index_dir(&self) -> PathBuf {
        self.cache_dir.join("sounds").join("index")
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.cache_dir.join("sounds").join("downloads")
    }

    pub fn plugin_dir(&self, f: PluginFormat) -> &Path {
        match f {
            PluginFormat::Clap => &self.clap_dir,
            PluginFormat::Vst3 => &self.vst3_dir,
        }
    }
}

// ---------------------------------------------------------------------------------------
// categories

const CATEGORY_WORDS: &[(&str, &[&str])] = &[
    (
        "drums",
        &[
            "drum",
            "drums",
            "kit",
            "percussion",
            "perc",
            "snare",
            "kick",
            "hihat",
            "hi-hat",
            "cymbal",
            "timpani",
            "tom",
            "808",
            "909",
            "conga",
            "bongo",
        ],
    ),
    (
        "keys",
        &[
            "piano",
            "keys",
            "keyboard",
            "rhodes",
            "wurli",
            "epiano",
            "e-piano",
            "electric-piano",
            "organ",
            "harpsichord",
            "clavinet",
            "celesta",
            "grand",
            "upright",
            "dx7",
        ],
    ),
    ("bass", &["bass", "contrabass", "upright-bass"]),
    ("guitar", &["guitar", "nylon", "acoustic-guitar", "strat", "les paul", "banjo", "ukulele", "mandolin"]),
    (
        "strings",
        &["strings", "string", "violin", "viola", "cello", "ensemble", "orchestra", "harp", "pizzicato", "fiddle"],
    ),
    ("brass", &["brass", "trumpet", "trombone", "horn", "tuba", "cornet", "flugelhorn"]),
    (
        "winds",
        &[
            "flute",
            "clarinet",
            "oboe",
            "bassoon",
            "sax",
            "saxophone",
            "woodwind",
            "woodwinds",
            "winds",
            "piccolo",
            "recorder",
            "pan flute",
        ],
    ),
    ("vocal", &["vocal", "voice", "choir", "vox", "aah", "ooh", "singer"]),
    ("pad", &["pad", "pads", "atmosphere", "ambient", "texture", "drone"]),
    (
        "synth",
        &["synth", "synthesizer", "lead", "wavetable", "fm", "analog", "modular", "polysynth", "monosynth", "arp"],
    ),
    (
        "fx",
        &[
            "fx",
            "effect",
            "audio-effect",
            "reverb",
            "delay",
            "impulse",
            "ir",
            "chorus",
            "distortion",
            "compressor",
            "eq",
            "filter",
            "sfx",
        ],
    ),
];

/// The category ferraus infers from a name, tags, or path text (first match wins in the
/// order drums, keys, guitar, bass, strings, brass, winds, vocal, pad, synth, fx).
pub fn infer_category(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let words: Vec<&str> =
        lower.split(|c: char| !(c.is_alphanumeric() || c == '-')).filter(|w| !w.is_empty()).collect();
    for (cat, keys) in CATEGORY_WORDS {
        for k in *keys {
            if k.contains(' ') {
                if lower.contains(k) {
                    return Some(cat);
                }
            } else if words.iter().any(|w| w == k || (k.len() >= 5 && w.starts_with(k))) {
                return Some(cat);
            }
        }
    }
    None
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

fn words_of(p: &Path, root: &Path) -> Vec<String> {
    let rel = p.strip_prefix(root).unwrap_or(p);
    let mut tags = Vec::new();
    for c in rel.components() {
        let s = c.as_os_str().to_string_lossy();
        let s = s.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or_else(|| s.to_string());
        for w in s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1) {
            let w = w.to_ascii_lowercase();
            if !tags.contains(&w) {
                tags.push(w);
            }
        }
    }
    tags
}

fn is_ir_context(p: &Path, root: &Path) -> bool {
    let rel = p.strip_prefix(root).unwrap_or(p);
    let mut texts: Vec<String> =
        rel.components().map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase()).collect();
    if let Some(n) = root.file_name() {
        texts.push(n.to_string_lossy().to_ascii_lowercase());
    }
    texts.iter().any(|t| {
        t.split(|c: char| !c.is_alphanumeric()).any(|w| {
            matches!(
                w,
                "ir" | "irs" | "impulse" | "impulses" | "reverb" | "cab" | "cabinet" | "convolution" | "rir" | "rirs"
            )
        })
    })
}

fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension().map(|e| exts.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())).unwrap_or(false)
}

/// Files (not plugins) under `root`: `.sf2`, `.sfz`, and IR audio files. Samples inside a
/// folder that holds `.sfz` files belong to that library and are not listed as IRs.
pub fn discover_files(root: &Path, origin: &str) -> Vec<Sound> {
    discover_files_with(root, origin, false)
}

/// Like [`discover_files`]; with `all_ir`, every audio file outside SFZ libraries is an
/// impulse response (used for installed IR packs).
pub fn discover_files_with(root: &Path, origin: &str, all_ir: bool) -> Vec<Sound> {
    let mut out = Vec::new();
    if root.is_file() {
        if let Some(s) = file_sound(root, root.parent().unwrap_or(root), origin, true) {
            out.push(s);
        }
        return out;
    }
    walk_files(root, root, origin, 0, false, all_ir, &mut out);
    out
}

fn file_sound(p: &Path, root: &Path, origin: &str, ir_ok: bool) -> Option<Sound> {
    let kind = if has_ext(p, &["sf2", "sf3"]) {
        SoundKind::Sf2
    } else if has_ext(p, &["sfz"]) {
        SoundKind::Sfz
    } else if ir_ok && has_ext(p, &["wav", "aif", "aiff", "flac"]) {
        SoundKind::Ir
    } else {
        return None;
    };
    let name = p.file_stem()?.to_string_lossy().into_owned();
    let mut tags = words_of(p, root);
    if let Some(rn) = root.file_name() {
        for w in rn.to_string_lossy().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1) {
            let w = w.to_ascii_lowercase();
            if !tags.contains(&w) {
                tags.push(w);
            }
        }
    }
    let category = if kind == SoundKind::Ir {
        Some("fx".to_string())
    } else {
        infer_category(&format!("{name} {}", tags.join(" "))).map(|s| s.to_string())
    };
    Some(Sound {
        id: slug(&name),
        name,
        kind,
        path: p.to_path_buf(),
        plugin_id: None,
        instrument: kind != SoundKind::Ir,
        category,
        tags,
        origin: origin.into(),
        registry_id: None,
    })
}

fn walk_files(dir: &Path, root: &Path, origin: &str, depth: usize, in_sfz: bool, all_ir: bool, out: &mut Vec<Sound>) {
    if depth > 8 || out.len() > 20000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    let here_sfz = in_sfz || entries.iter().any(|p| p.is_file() && has_ext(p, &["sfz"]));
    for p in &entries {
        if p.is_dir() {
            if PluginFormat::from_path(p).is_some() || has_ext(p, &["component", "app", "lv2"]) {
                continue;
            }
            walk_files(p, root, origin, depth + 1, here_sfz, all_ir, out);
        } else {
            let ir_ok = !here_sfz && (all_ir || is_ir_context(p, root));
            if let Some(s) = file_sound(p, root, origin, ir_ok) {
                out.push(s);
            }
        }
    }
}

/// Everything in one folder: sound files plus plugins (probed in a host subprocess, cached).
pub fn discover_folder(env: &SoundsEnv, dir: &Path, watchdog: Duration) -> Result<Vec<Sound>> {
    if !dir.exists() {
        return Err(Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("folder not found: {}", dir.display()),
        ));
    }
    let mut out = discover_files(dir, "sound-path");
    let opts = ScanOptions {
        extra: vec![dir.to_path_buf()],
        standard: false,
        watchdog,
        cache_path: env.scan_cache(),
        ..Default::default()
    };
    let report = scan::scan(&opts)?;
    out.extend(report.plugins.iter().map(|p| plugin_sound(p, "sound-path")));
    Ok(out)
}

fn plugin_sound(p: &scan::ScannedPlugin, origin: &str) -> Sound {
    Sound {
        id: if p.id.is_empty() { slug(&p.name) } else { p.id.clone() },
        name: p.name.clone(),
        kind: match p.format {
            PluginFormat::Clap => SoundKind::Clap,
            PluginFormat::Vst3 => SoundKind::Vst3,
        },
        path: p.path.clone(),
        plugin_id: if p.id.is_empty() { None } else { Some(p.id.clone()) },
        instrument: p.is_instrument(),
        category: p.category.clone(),
        tags: p.features.clone(),
        origin: origin.into(),
        registry_id: None,
    }
}

/// A record in `installed.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InstalledRecord {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: SoundKind,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub license: String,
    /// Files and folders this install created (removed by `sounds remove`).
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `registry` or `user` (`sounds install --from`).
    pub source: String,
    #[serde(default)]
    pub installed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

pub fn read_installed(env: &SoundsEnv) -> Vec<InstalledRecord> {
    std::fs::read(env.installed_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn write_installed(env: &SoundsEnv, recs: &[InstalledRecord]) -> Result<()> {
    std::fs::create_dir_all(&env.sounds_dir)?;
    let f = env.installed_file();
    let tmp = f.with_extension("json.part");
    std::fs::write(&tmp, serde_json::to_vec_pretty(recs)?)?;
    std::fs::rename(&tmp, &f)?;
    Ok(())
}

/// Every sound ferraus can use without a download: installed sounds (with their registry
/// tags), `sound_paths` folders, and plugins in the scan cache.
pub fn discover(env: &SoundsEnv, sound_paths: &[PathBuf], watchdog: Duration) -> Result<Vec<Sound>> {
    let installed = read_installed(env);
    let mut out: Vec<Sound> = Vec::new();
    for rec in &installed {
        for p in &rec.paths {
            if rec.kind.is_plugin() {
                continue;
            }
            for mut s in discover_files_with(p, "installed", rec.kind == SoundKind::Ir) {
                s.registry_id = Some(rec.id.clone());
                for t in &rec.tags {
                    if !s.tags.contains(t) {
                        s.tags.push(t.clone());
                    }
                }
                if rec.category.is_some() && s.kind != SoundKind::Ir {
                    s.category = rec.category.clone();
                }
                // a single-sound install answers to the registry id and name too
                if rec.paths.len() == 1 && discover_files_with(p, "installed", rec.kind == SoundKind::Ir).len() == 1 {
                    s.id = rec.id.clone();
                    s.name = rec.name.clone();
                }
                out.push(s);
            }
        }
    }
    // the sounds folder itself (files copied there by hand)
    for s in discover_files(&env.sounds_dir, "installed") {
        if !out.iter().any(|o| o.path == s.path) {
            out.push(s);
        }
    }
    for d in sound_paths {
        if d.exists() {
            for s in discover_files(d, "sound-path") {
                if !out.iter().any(|o| o.path == s.path) {
                    out.push(s);
                }
            }
        }
    }
    // plugins: sound paths and the per-user install folders are scanned (cached by mtime);
    // the standard folders come from the last full `plugin scan`.
    let mut dirs: Vec<PathBuf> = sound_paths.iter().filter(|d| d.exists()).cloned().collect();
    for d in [&env.clap_dir, &env.vst3_dir] {
        if d.exists() {
            dirs.push(d.clone());
        }
    }
    if !dirs.is_empty() {
        let opts =
            ScanOptions { extra: dirs, standard: false, watchdog, cache_path: env.scan_cache(), ..Default::default() };
        for p in scan::scan(&opts)?.plugins {
            let s = plugin_sound(&p, "sound-path");
            if !out.iter().any(|o| o.path == s.path && o.plugin_id == s.plugin_id) {
                out.push(s);
            }
        }
    }
    for p in ScanCache::load(&env.scan_cache()).plugins() {
        let s = plugin_sound(&p, "plugin-scan");
        if !out.iter().any(|o| o.path == s.path && o.plugin_id == s.plugin_id) {
            out.push(s);
        }
    }
    for s in out.iter_mut() {
        if let Some(rec) = installed.iter().find(|r| r.paths.iter().any(|p| s.path.starts_with(p))) {
            s.registry_id = Some(rec.id.clone());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// aliases

#[derive(Serialize, Deserialize, Default)]
struct AliasFile {
    #[serde(default)]
    aliases: BTreeMap<String, String>,
}

/// `[aliases]` from `<config>/sounds/aliases.toml`.
pub fn read_aliases(env: &SoundsEnv) -> BTreeMap<String, String> {
    std::fs::read_to_string(env.aliases_file())
        .ok()
        .and_then(|t| toml::from_str::<AliasFile>(&t).ok())
        .map(|f| f.aliases)
        .unwrap_or_default()
}

pub fn write_aliases(env: &SoundsEnv, aliases: &BTreeMap<String, String>) -> Result<()> {
    let f = env.aliases_file();
    if let Some(p) = f.parent() {
        std::fs::create_dir_all(p)?;
    }
    let body = toml::to_string(&AliasFile { aliases: aliases.clone() })
        .map_err(|e| Error::internal(format!("cannot write aliases: {e}")))?;
    let text = format!(
        "# Instrument aliases for ferraus (`ferraus sounds alias name=<id or source>`).\n# Values are sound ids, registry ids, or explicit sources such as plugin:\"Surge XT\" or sf2:/path/gm.sf2.\n{body}"
    );
    std::fs::write(&f, text)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// resolution

/// How a query was resolved.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Resolved {
    pub query: String,
    /// `explicit`, `alias`, `name`, or `tag`.
    pub via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_target: Option<String>,
    pub sound: Sound,
}

fn explicit(env: &SoundsEnv, q: &str, sound_paths: &[PathBuf], watchdog: Duration) -> Result<Option<Sound>> {
    let Some((scheme, rest)) = q.split_once(':') else {
        return Ok(None);
    };
    let rest = rest.trim().trim_matches('"');
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "plugin" => {
            let r = scan::resolve_plugin_with(rest, sound_paths, watchdog, &env.scan_cache())?;
            let p = scan::ScanCache::load(&env.scan_cache())
                .plugins()
                .into_iter()
                .find(|p| p.path == r.path && Some(&p.id) == r.plugin_id.as_ref());
            return Ok(Some(match p {
                Some(p) => plugin_sound(&p, "explicit"),
                None => Sound {
                    id: r.plugin_id.clone().unwrap_or_else(|| slug(&r.name)),
                    name: r.name.clone(),
                    kind: if r.format == PluginFormat::Clap { SoundKind::Clap } else { SoundKind::Vst3 },
                    path: r.path.clone(),
                    plugin_id: r.plugin_id.clone(),
                    instrument: true,
                    category: None,
                    tags: Vec::new(),
                    origin: "explicit".into(),
                    registry_id: None,
                },
            }));
        }
        "sf2" => SoundKind::Sf2,
        "sfz" => SoundKind::Sfz,
        "ir" => SoundKind::Ir,
        _ => return Ok(None),
    };
    let p = PathBuf::from(rest);
    if !p.is_file() {
        return Err(Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("{} file not found: {}", kind.as_str(), p.display()),
        ));
    }
    let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Some(Sound {
        id: slug(&name),
        category: infer_category(&name).map(|s| s.to_string()),
        name,
        kind,
        path: p,
        plugin_id: None,
        instrument: kind != SoundKind::Ir,
        tags: Vec::new(),
        origin: "explicit".into(),
        registry_id: None,
    }))
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn candidates_error(q: &str, hits: &[&Sound]) -> Error {
    let list: Vec<String> = hits
        .iter()
        .take(10)
        .map(|s| format!("{} [{}] → --instrument '{}'", s.name, s.kind.as_str(), s.source()))
        .collect();
    let hint = format!("ferraus sounds alias {}={}", slug(q), hits[0].id);
    Error::new(
        ErrorKind::Usage,
        "sound_ambiguous",
        format!(
            "'{q}' matches {} sounds:\n  {}\nPick one with an explicit source or set an alias.",
            hits.len(),
            list.join("\n  ")
        ),
    )
    .with_hint(hint)
}

/// Resolve an instrument (or IR) name. `Ok(None)` when nothing matches, so callers can
/// fall back to General MIDI names; an ambiguous name is an error listing candidates.
pub fn resolve_sound(
    env: &SoundsEnv,
    query: &str,
    sound_paths: &[PathBuf],
    watchdog: Duration,
) -> Result<Option<Resolved>> {
    resolve_inner(env, query, sound_paths, watchdog, 0)
}

fn resolve_inner(
    env: &SoundsEnv,
    query: &str,
    sound_paths: &[PathBuf],
    watchdog: Duration,
    depth: usize,
) -> Result<Option<Resolved>> {
    let q = query.trim();
    if q.is_empty() {
        return Err(Error::usage("empty instrument name"));
    }
    if let Some(s) = explicit(env, q, sound_paths, watchdog)? {
        return Ok(Some(Resolved { query: q.into(), via: "explicit".into(), alias_target: None, sound: s }));
    }
    if depth == 0 {
        let aliases = read_aliases(env);
        let target = aliases.get(q).or_else(|| aliases.iter().find(|(k, _)| k.eq_ignore_ascii_case(q)).map(|(_, v)| v));
        if let Some(t) = target {
            if let Some(mut r) = resolve_inner(env, t, sound_paths, watchdog, depth + 1)? {
                r.alias_target = Some(t.clone());
                r.query = q.into();
                r.via = "alias".into();
                return Ok(Some(r));
            }
            return Err(Error::new(
                ErrorKind::Usage,
                "alias_unresolved",
                format!("alias '{q}' points to '{t}', which is not installed or found"),
            )
            .with_hint(format!("ferraus sounds install {t}")));
        }
    }
    let all = discover(env, sound_paths, watchdog)?;
    let nq = norm(q);
    let exact: Vec<&Sound> = all
        .iter()
        .filter(|s| {
            s.id.eq_ignore_ascii_case(q)
                || s.name.eq_ignore_ascii_case(q)
                || norm(&s.name) == nq
                || s.registry_id.as_deref().map(|r| r.eq_ignore_ascii_case(q)).unwrap_or(false)
        })
        .collect();
    let pick = |hits: Vec<&Sound>, via: &str| -> Result<Option<Resolved>> {
        // prefer things that play notes
        let inst: Vec<&Sound> = hits.iter().copied().filter(|s| s.instrument).collect();
        let hits = if inst.is_empty() { hits } else { inst };
        match hits.len() {
            0 => Ok(None),
            1 => Ok(Some(Resolved { query: q.into(), via: via.into(), alias_target: None, sound: hits[0].clone() })),
            _ => {
                // the same sound from one install (several .sfz variants) still counts as ambiguous
                Err(candidates_error(q, &hits))
            }
        }
    };
    if !exact.is_empty() {
        return pick(exact, "name");
    }
    let words: Vec<String> = q
        .to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    let tagged: Vec<&Sound> = all
        .iter()
        .filter(|s| {
            words.iter().all(|w| {
                s.tags.iter().any(|t| t == w)
                    || s.category.as_deref() == Some(w.as_str())
                    || s.name.to_ascii_lowercase().split(|c: char| !c.is_alphanumeric()).any(|n| n == w)
            })
        })
        .collect();
    pick(tagged, "tag")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(infer_category("Salamander Grand Piano"), Some("keys"));
        assert_eq!(infer_category("Nylon Guitar"), Some("guitar"));
        assert_eq!(infer_category("Violin Section"), Some("strings"));
        assert_eq!(infer_category("TR-808 kit"), Some("drums"));
        assert_eq!(infer_category("Warm Pad"), Some("pad"));
        assert_eq!(infer_category("Surge XT synthesizer"), Some("synth"));
        assert_eq!(infer_category("Hall reverb IR"), Some("fx"));
        assert_eq!(infer_category("zzz"), None);
    }

    #[test]
    fn sfz_samples_are_not_irs() {
        let d = ferraus_core::testing::TempDir::new("sfzlib");
        let lib = d.path("Grand Piano");
        std::fs::create_dir_all(lib.join("samples")).unwrap();
        std::fs::write(lib.join("Grand Piano.sfz"), "<region> sample=samples/c4.wav").unwrap();
        std::fs::write(lib.join("samples/c4.wav"), b"RIFF").unwrap();
        std::fs::create_dir_all(d.path("irs")).unwrap();
        std::fs::write(d.path("irs/hall.wav"), b"RIFF").unwrap();
        let s = discover_files(&d.0, "sound-path");
        let kinds: Vec<(SoundKind, String)> = s.iter().map(|s| (s.kind, s.name.clone())).collect();
        assert!(kinds.contains(&(SoundKind::Sfz, "Grand Piano".into())));
        assert!(kinds.contains(&(SoundKind::Ir, "hall".into())));
        assert_eq!(s.len(), 2, "{kinds:?}");
    }

    #[test]
    fn aliases_roundtrip() {
        let d = ferraus_core::testing::TempDir::new("alias");
        let env = SoundsEnv::under(&d.0);
        let mut a = BTreeMap::new();
        a.insert("piano".to_string(), "salamander-grand-piano".to_string());
        write_aliases(&env, &a).unwrap();
        assert_eq!(read_aliases(&env), a);
        let text = std::fs::read_to_string(env.aliases_file()).unwrap();
        assert!(text.contains("[aliases]"));
    }
}
