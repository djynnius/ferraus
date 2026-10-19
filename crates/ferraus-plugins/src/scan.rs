//! Plugin discovery: standard per-OS folders plus extra folders, CLAP probing in a host
//! subprocess (a crashing plugin cannot kill the scan), VST3 metadata from
//! `moduleinfo.json`, and a cache at `<cache>/plugins/scan.json` keyed by path and mtime.

use crate::api::PluginRef;
use crate::client::{PluginFormat, PluginHost};
use crate::protocol::Descriptor;
use crate::resolve::infer_category;
use ferraus_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One plugin found by a scan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScannedPlugin {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub format: PluginFormat,
    /// The bundle (`.clap` / `.vst3` file or directory).
    pub path: PathBuf,
    #[serde(default)]
    pub features: Vec<String>,
    /// `instrument`, `audio-effect`, `note-effect`, `analyzer`, `other`, or `unknown`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Where the metadata came from: `probe`, `moduleinfo`, or `bundle` (name only).
    #[serde(default)]
    pub source: String,
}

impl ScannedPlugin {
    pub fn is_instrument(&self) -> bool {
        self.kind == "instrument"
    }

    pub fn is_effect(&self) -> bool {
        self.kind == "audio-effect" || self.kind == "note-effect"
    }

    pub fn plugin_ref(&self) -> PluginRef {
        PluginRef {
            format: self.format,
            path: self.path.clone(),
            plugin_id: if self.id.is_empty() { None } else { Some(self.id.clone()) },
            name: self.name.clone(),
            vendor: self.vendor.clone(),
            version: self.version.clone(),
        }
    }

    fn from_descriptor(d: Descriptor, format: PluginFormat, path: &Path, source: &str) -> Self {
        let category = infer_category(&format!("{} {}", d.name, d.features.join(" "))).map(|s| s.to_string());
        ScannedPlugin {
            id: d.id,
            name: d.name,
            vendor: d.vendor,
            version: d.version,
            description: d.description,
            format,
            path: path.to_path_buf(),
            features: d.features,
            kind: d.kind,
            category,
            source: source.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CacheEntry {
    pub mtime: u64,
    pub format: Option<PluginFormat>,
    #[serde(default)]
    pub plugins: Vec<ScannedPlugin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `<cache>/plugins/scan.json`.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ScanCache {
    pub version: u32,
    pub entries: BTreeMap<String, CacheEntry>,
}

pub const CACHE_VERSION: u32 = 1;

impl ScanCache {
    pub fn load(path: &Path) -> ScanCache {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<ScanCache>(&b).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or(ScanCache { version: CACHE_VERSION, entries: BTreeMap::new() })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = path.with_extension("json.part");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Every cached plugin whose bundle still exists.
    pub fn plugins(&self) -> Vec<ScannedPlugin> {
        self.entries
            .iter()
            .filter(|(p, _)| Path::new(p).exists())
            .flat_map(|(_, e)| e.plugins.iter().cloned())
            .collect()
    }
}

/// Default cache location.
pub fn default_cache_path() -> PathBuf {
    ferraus_core::paths::cache_dir().join("plugins").join("scan.json")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The per-user folder `sounds install` places plugins in. `FERRAUS_PLUGIN_DIR` overrides
/// the base (`<base>/clap`, `<base>/vst3`).
pub fn user_plugin_dir(format: PluginFormat) -> PathBuf {
    if let Some(b) = std::env::var_os("FERRAUS_PLUGIN_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(b).join(format.as_str());
    }
    let h = home();
    match (format, std::env::consts::OS) {
        (PluginFormat::Clap, "macos") => h.join("Library/Audio/Plug-Ins/CLAP"),
        (PluginFormat::Vst3, "macos") => h.join("Library/Audio/Plug-Ins/VST3"),
        (PluginFormat::Clap, "windows") => local_app_data().join("Programs/Common/CLAP"),
        (PluginFormat::Vst3, "windows") => local_app_data().join("Programs/Common/VST3"),
        (PluginFormat::Clap, _) => h.join(".clap"),
        (PluginFormat::Vst3, _) => h.join(".vst3"),
    }
}

fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData/Local"))
}

/// The standard per-OS plugin folders from the PRD (plus `CLAP_PATH` for CLAP and the
/// ferraus per-user install folder). `FERRAUS_PLUGIN_STANDARD_DIRS=0` limits this to the
/// ferraus install folder (hermetic tests and sandboxes).
pub fn standard_dirs(format: PluginFormat) -> Vec<PathBuf> {
    if std::env::var("FERRAUS_PLUGIN_STANDARD_DIRS").map(|v| v == "0").unwrap_or(false) {
        return vec![user_plugin_dir(format)];
    }
    let h = home();
    let common = std::env::var_os("COMMONPROGRAMFILES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Program Files/Common Files"));
    let mut dirs: Vec<PathBuf> = match (format, std::env::consts::OS) {
        (PluginFormat::Clap, "macos") => {
            vec![h.join("Library/Audio/Plug-Ins/CLAP"), PathBuf::from("/Library/Audio/Plug-Ins/CLAP")]
        }
        (PluginFormat::Vst3, "macos") => {
            vec![h.join("Library/Audio/Plug-Ins/VST3"), PathBuf::from("/Library/Audio/Plug-Ins/VST3")]
        }
        (PluginFormat::Clap, "windows") => {
            vec![common.join("CLAP"), local_app_data().join("Programs/Common/CLAP")]
        }
        (PluginFormat::Vst3, "windows") => {
            vec![common.join("VST3"), local_app_data().join("Programs/Common/VST3")]
        }
        (PluginFormat::Clap, _) => vec![h.join(".clap"), PathBuf::from("/usr/lib/clap")],
        (PluginFormat::Vst3, _) => {
            vec![h.join(".vst3"), PathBuf::from("/usr/lib/vst3"), PathBuf::from("/usr/local/lib/vst3")]
        }
    };
    if format == PluginFormat::Clap {
        if let Some(p) = std::env::var_os("CLAP_PATH") {
            dirs.extend(std::env::split_paths(&p));
        }
    }
    let user = user_plugin_dir(format);
    if !dirs.contains(&user) {
        dirs.push(user);
    }
    dirs
}

fn is_bundle_ext(p: &Path, formats: &[PluginFormat]) -> Option<PluginFormat> {
    let f = PluginFormat::from_path(p)?;
    formats.contains(&f).then_some(f)
}

/// Every plugin bundle under `dir` (recursive; bundles themselves are not descended into).
pub fn find_bundles(dir: &Path, formats: &[PluginFormat]) -> Vec<(PluginFormat, PathBuf)> {
    let mut out = Vec::new();
    if let Some(f) = is_bundle_ext(dir, formats) {
        if dir.exists() {
            out.push((f, dir.to_path_buf()));
        }
        return out;
    }
    walk(dir, formats, 0, &mut out);
    out
}

fn walk(dir: &Path, formats: &[PluginFormat], depth: usize, out: &mut Vec<(PluginFormat, PathBuf)>) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if let Some(f) = is_bundle_ext(&p, formats) {
            out.push((f, p));
            continue;
        }
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if matches!(ext.as_str(), "component" | "app" | "lv2" | "vst" | "aaxplugin" | "clap" | "vst3") {
            continue;
        }
        if p.is_dir() {
            walk(&p, formats, depth + 1, out);
        }
    }
}

fn mtime_of(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Change stamp of a bundle: the newest mtime of the bundle and the files inside it.
pub fn bundle_mtime(p: &Path) -> u64 {
    let mut m = mtime_of(p);
    if p.is_dir() {
        fn rec(d: &Path, depth: usize, m: &mut u64) {
            if depth > 4 {
                return;
            }
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let p = e.path();
                    *m = (*m).max(mtime_of(&p));
                    if p.is_dir() {
                        rec(&p, depth + 1, m);
                    }
                }
            }
        }
        rec(p, 0, &mut m);
    }
    m
}

/// Strip `//` and `/* */` comments and trailing commas (moduleinfo.json is JSON5-ish).
fn json5_lite(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i] as char;
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == ',' {
            let mut j = i + 1;
            while j < b.len() && (b[j] as char).is_whitespace() {
                j += 1;
            }
            if j < b.len() && (b[j] == b'}' || b[j] == b']') {
                i += 1;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            // copy a full UTF-8 sequence
            let len = utf8_len(b[i]);
            out.push_str(&text[i..(i + len).min(text.len())]);
            i += len;
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Plugins declared by a VST3 bundle's `moduleinfo.json`, when it has one.
pub fn read_vst3_moduleinfo(bundle: &Path) -> Option<Vec<ScannedPlugin>> {
    let candidates = [
        bundle.join("Contents").join("Resources").join("moduleinfo.json"),
        bundle.join("Contents").join("moduleinfo.json"),
    ];
    let file = candidates.iter().find(|p| p.is_file())?;
    let text = std::fs::read_to_string(file).ok()?;
    let v: serde_json::Value = serde_json::from_str(&json5_lite(&text)).ok()?;
    let module_version = v.get("Version").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let factory_vendor = v.pointer("/Factory Info/Vendor").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let url = v.pointer("/Factory Info/URL").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let mut out = Vec::new();
    for c in v.get("Classes").and_then(|c| c.as_array()).into_iter().flatten() {
        let cat = c.get("Category").and_then(|x| x.as_str()).unwrap_or("");
        if cat != "Audio Module Class" {
            continue;
        }
        let subs: Vec<String> = match c.get("Sub Categories") {
            Some(serde_json::Value::Array(a)) => {
                a.iter().filter_map(|s| s.as_str()).map(|s| s.to_ascii_lowercase()).collect()
            }
            Some(serde_json::Value::String(s)) => s.split('|').map(|s| s.trim().to_ascii_lowercase()).collect(),
            _ => Vec::new(),
        };
        let mut features = subs.clone();
        if subs.iter().any(|s| s == "instrument") {
            features.push("instrument".into());
        } else if subs.iter().any(|s| s == "fx") {
            features.push("audio-effect".into());
        }
        let d = Descriptor {
            id: c.get("CID").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            name: c.get("Name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            vendor: c
                .get("Vendor")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| factory_vendor.clone()),
            version: c
                .get("Version")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| module_version.clone()),
            description: String::new(),
            url: url.clone(),
            kind: Descriptor::classify(&features),
            features,
        };
        out.push(ScannedPlugin::from_descriptor(d, PluginFormat::Vst3, bundle, "moduleinfo"));
    }
    Some(out)
}

/// Probe one bundle. CLAP bundles are probed in a `ferraus-plughost` subprocess; VST3
/// bundles are read from `moduleinfo.json`, else probed by `ferraus-plughost-vst3` when
/// available, else listed by bundle name.
pub fn probe_bundle(format: PluginFormat, path: &Path, watchdog: Duration) -> Result<Vec<ScannedPlugin>> {
    if format == PluginFormat::Vst3 {
        if let Some(p) = read_vst3_moduleinfo(path) {
            return Ok(p);
        }
    }
    let label = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let probed = PluginHost::spawn(format, &label, path, watchdog).and_then(|mut h| {
        let r = h.probe(path);
        h.shutdown();
        r
    });
    match (probed, format) {
        (Ok(ds), _) => Ok(ds.into_iter().map(|d| ScannedPlugin::from_descriptor(d, format, path, "probe")).collect()),
        (Err(e), PluginFormat::Vst3) if e.code != "plugin_crashed" && e.code != "plugin_timeout" => {
            // No moduleinfo.json and no usable VST3 host: still list the bundle by name.
            let d = Descriptor { id: String::new(), name: label.clone(), kind: "unknown".into(), ..Default::default() };
            let mut p = ScannedPlugin::from_descriptor(d, format, path, "bundle");
            p.description = format!("not probed: {}", e.message);
            Ok(vec![p])
        }
        (Err(e), _) => Err(e),
    }
}

/// What to scan.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub extra: Vec<PathBuf>,
    pub formats: Vec<PluginFormat>,
    pub rescan: bool,
    /// Include the standard per-OS folders.
    pub standard: bool,
    pub watchdog: Duration,
    pub cache_path: PathBuf,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            extra: Vec::new(),
            formats: vec![PluginFormat::Clap, PluginFormat::Vst3],
            rescan: false,
            standard: true,
            watchdog: Duration::from_secs(30),
            cache_path: default_cache_path(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ScanError {
    pub path: PathBuf,
    pub format: PluginFormat,
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct ScanReport {
    pub plugins: Vec<ScannedPlugin>,
    pub errors: Vec<ScanError>,
    pub dirs: Vec<PathBuf>,
    pub bundles: usize,
    pub probed: usize,
    pub cached: usize,
}

/// The folders a scan will visit.
pub fn scan_dirs(opts: &ScanOptions) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if opts.standard {
        for f in &opts.formats {
            dirs.extend(standard_dirs(*f));
        }
    }
    dirs.extend(opts.extra.iter().cloned());
    let mut seen = Vec::new();
    dirs.retain(|d| {
        if seen.contains(d) {
            false
        } else {
            seen.push(d.clone());
            true
        }
    });
    dirs
}

/// Scan, probing only bundles that are new or changed since the cached scan (or all of
/// them with `rescan`), and update the cache.
pub fn scan(opts: &ScanOptions) -> Result<ScanReport> {
    let mut cache = ScanCache::load(&opts.cache_path);
    let dirs = scan_dirs(opts);
    let mut bundles: Vec<(PluginFormat, PathBuf)> = Vec::new();
    for d in &dirs {
        for b in find_bundles(d, &opts.formats) {
            if !bundles.contains(&b) {
                bundles.push(b);
            }
        }
    }
    let mut report = ScanReport { dirs: dirs.clone(), bundles: bundles.len(), ..Default::default() };
    let mut todo: Vec<(PluginFormat, PathBuf, u64)> = Vec::new();
    for (f, p) in &bundles {
        let key = p.to_string_lossy().into_owned();
        let m = bundle_mtime(p);
        match cache.entries.get(&key) {
            Some(e) if !opts.rescan && e.mtime == m && e.format == Some(*f) => {
                report.cached += 1;
            }
            _ => todo.push((*f, p.clone(), m)),
        }
    }
    // Probe in parallel, a few host processes at a time.
    let results: Vec<(PluginFormat, PathBuf, u64, Result<Vec<ScannedPlugin>>)> = std::thread::scope(|s| {
        let mut out = Vec::new();
        for chunk in todo.chunks(4) {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(f, p, m)| {
                    let (f, p, m) = (*f, p.clone(), *m);
                    let wd = opts.watchdog;
                    s.spawn(move || {
                        let r = probe_bundle(f, &p, wd);
                        (f, p, m, r)
                    })
                })
                .collect();
            for h in handles {
                if let Ok(r) = h.join() {
                    out.push(r);
                }
            }
        }
        out
    });
    for (f, p, m, r) in results {
        report.probed += 1;
        let key = p.to_string_lossy().into_owned();
        let entry = match r {
            Ok(plugins) => CacheEntry { mtime: m, format: Some(f), plugins, error: None },
            Err(e) => CacheEntry { mtime: m, format: Some(f), plugins: Vec::new(), error: Some(e.message.clone()) },
        };
        cache.entries.insert(key, entry);
    }
    // forget bundles that were removed from disk
    cache.entries.retain(|p, _| Path::new(p).exists());
    for (f, p) in &bundles {
        if let Some(e) = cache.entries.get(&p.to_string_lossy().into_owned()) {
            report.plugins.extend(e.plugins.iter().cloned());
            if let Some(err) = &e.error {
                report.errors.push(ScanError { path: p.clone(), format: *f, error: err.clone() });
            }
        }
    }
    cache.save(&opts.cache_path)?;
    Ok(report)
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
        .unwrap_or(s)
}

/// Pick plugins matching `query` by id, name, or unique prefix/substring.
pub fn match_plugins<'a>(plugins: &'a [ScannedPlugin], query: &str) -> Vec<&'a ScannedPlugin> {
    let q = unquote(query);
    let nq = norm(q);
    let stages: [&dyn Fn(&ScannedPlugin) -> bool; 4] = [
        &|p| p.id == q,
        &|p| p.name.eq_ignore_ascii_case(q) || p.id.eq_ignore_ascii_case(q),
        &|p| !nq.is_empty() && (norm(&p.name) == nq || norm(&p.id) == nq),
        &|p| !nq.is_empty() && (norm(&p.name).contains(&nq)),
    ];
    for test in stages {
        let hits: Vec<&ScannedPlugin> = plugins.iter().filter(|p| test(p)).collect();
        if hits.is_empty() {
            continue;
        }
        // the same plugin in CLAP and VST3: prefer CLAP
        let mut hits = hits;
        hits.sort_by_key(|p| (p.format != PluginFormat::Clap, p.path.clone()));
        let first = hits[0];
        let same = hits.iter().all(|p| norm(&p.name) == norm(&first.name));
        if same {
            return vec![first];
        }
        return hits;
    }
    Vec::new()
}

/// Resolve a plugin name, id, or path to a [`PluginRef`] (see [`PluginRef::resolve`]).
pub fn resolve_plugin(query: &str, sound_paths: &[PathBuf], watchdog: Duration) -> Result<PluginRef> {
    resolve_plugin_with(query, sound_paths, watchdog, &default_cache_path())
}

pub fn resolve_plugin_with(
    query: &str,
    sound_paths: &[PathBuf],
    watchdog: Duration,
    cache_path: &Path,
) -> Result<PluginRef> {
    let q = unquote(query.trim().strip_prefix("plugin:").unwrap_or(query.trim()));
    if q.is_empty() {
        return Err(Error::usage("empty plugin name"));
    }
    // explicit path, optionally `path#plugin.id`
    let (path_part, id_part) = match q.rsplit_once('#') {
        Some((p, id)) if Path::new(p).exists() => (p, Some(id)),
        _ => (q, None),
    };
    let p = Path::new(path_part);
    if p.exists() && PluginFormat::from_path(p).is_some() {
        let format = PluginFormat::from_path(p).unwrap();
        let mut opts = ScanOptions {
            extra: vec![p.to_path_buf()],
            formats: vec![format],
            standard: false,
            watchdog,
            cache_path: cache_path.to_path_buf(),
            ..Default::default()
        };
        opts.rescan = false;
        let report = scan(&opts)?;
        if let Some(e) = report.errors.first() {
            return Err(Error::new(
                ErrorKind::Plugin,
                "plugin_probe_failed",
                format!("cannot read plugin bundle {}: {}", p.display(), e.error),
            ));
        }
        let chosen: Vec<&ScannedPlugin> = match id_part {
            Some(id) => report.plugins.iter().filter(|x| x.id == id).collect(),
            None => report.plugins.iter().collect(),
        };
        return match chosen.len() {
            1 => Ok(chosen[0].plugin_ref()),
            0 if id_part.is_some() => Err(Error::new(
                ErrorKind::Plugin,
                "plugin_not_found",
                format!("{} has no plugin with id '{}'", p.display(), id_part.unwrap()),
            )),
            0 => Err(Error::new(ErrorKind::Plugin, "plugin_not_found", format!("{} contains no plugins", p.display()))),
            _ => Err(ambiguous(q, &chosen, true)),
        };
    }
    // by name or id: sound paths first (scanned now, cached by mtime), then the cache
    let mut pool: Vec<ScannedPlugin> = Vec::new();
    if !sound_paths.is_empty() {
        let opts = ScanOptions {
            extra: sound_paths.to_vec(),
            standard: false,
            watchdog,
            cache_path: cache_path.to_path_buf(),
            ..Default::default()
        };
        pool.extend(scan(&opts)?.plugins);
    }
    let mut cache = ScanCache::load(cache_path);
    if cache.entries.is_empty() {
        let opts = ScanOptions { standard: true, watchdog, cache_path: cache_path.to_path_buf(), ..Default::default() };
        let _ = scan(&opts);
        cache = ScanCache::load(cache_path);
    }
    for p in cache.plugins() {
        if !pool.contains(&p) {
            pool.push(p);
        }
    }
    let hits = match_plugins(&pool, q);
    match hits.len() {
        1 => Ok(hits[0].plugin_ref()),
        0 => Err(Error::new(
            ErrorKind::Plugin,
            "plugin_not_found",
            format!("no plugin named '{q}' was found (searched the scan cache and --sound-path folders)"),
        )
        .with_hint("ferraus plugin scan --paths <folder-with-plugins>")),
        _ => Err(ambiguous(q, &hits, false)),
    }
}

fn ambiguous(q: &str, hits: &[&ScannedPlugin], by_path: bool) -> Error {
    let list: Vec<String> = hits
        .iter()
        .take(10)
        .map(|p| {
            if by_path {
                format!("{} ({}#{})", p.name, p.path.display(), p.id)
            } else {
                format!("{} [{} {}] id {}", p.name, p.format, p.vendor, p.id)
            }
        })
        .collect();
    let first = hits[0];
    let hint = if by_path {
        format!("ferraus plugin info \"{}#{}\"", first.path.display(), first.id)
    } else {
        format!("ferraus plugin info \"{}\"", first.id)
    };
    Error::new(ErrorKind::Usage, "plugin_ambiguous", format!("'{q}' matches several plugins: {}", list.join("; ")))
        .with_hint(hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moduleinfo_is_parsed_with_comments_and_trailing_commas() {
        let dir = ferraus_core::testing::TempDir::new("vst3mi");
        let b = dir.path("Fake Synth.vst3");
        std::fs::create_dir_all(b.join("Contents/Resources")).unwrap();
        std::fs::write(
            b.join("Contents/Resources/moduleinfo.json"),
            r#"{
  // generated
  "Name": "Fake Synth", "Version": "1.2.0",
  "Factory Info": { "Vendor": "Acme", "URL": "https://acme.test", },
  "Classes": [
    { "CID": "ABCDEF0123456789ABCDEF0123456789", "Category": "Audio Module Class",
      "Name": "Fake Synth", "Sub Categories": ["Instrument", "Synth"], },
    { "CID": "00", "Category": "Component Controller Class", "Name": "Fake Synth Controller" },
  ],
}"#,
        )
        .unwrap();
        let p = read_vst3_moduleinfo(&b).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "Fake Synth");
        assert_eq!(p[0].vendor, "Acme");
        assert_eq!(p[0].kind, "instrument");
        assert_eq!(p[0].category.as_deref(), Some("synth"));
        let found = find_bundles(&dir.0, &[PluginFormat::Vst3]);
        assert_eq!(found.len(), 1);
    }
}
