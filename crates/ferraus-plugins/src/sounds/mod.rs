//! The installable sounds registry: a signed JSON manifest of free instruments, SoundFonts,
//! SFZ libraries, and impulse responses, and the `sounds ...` commands.
//!
//! The manifest (`sounds/registry.json`) is embedded in the binary together with its
//! Ed25519 signature (`registry.json.sig`); `sounds update` fetches a newer signed copy into
//! the cache. Entries without a verified SHA-256 are refused unless
//! `FERRAUS_ALLOW_UNVERIFIED=1`. ferraus never runs vendor installers and writes only to
//! per-user folders.

pub mod install;

use crate::client::default_watchdog;
use crate::resolve::{self, SoundKind, SoundsEnv};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The embedded manifest and its detached signature.
pub const EMBEDDED_REGISTRY: &[u8] = include_bytes!("../../sounds/registry.json");
pub const EMBEDDED_SIGNATURE: &str = include_str!("../../sounds/registry.json.sig");
/// Public half of the registry signing key (base64, 32 bytes). This is the development
/// key; release builds are signed in CI with a key held only as a CI secret.
pub const REGISTRY_PUBLIC_KEY: &str = include_str!("../../sounds/registry-public-key.txt");
/// Where `sounds update` looks for a newer manifest (`<url>` and `<url>.sig`).
pub const DEFAULT_REGISTRY_URL: &str = "https://djynnius.github.io/ferraus/sounds/registry.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct License {
    pub spdx: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Download {
    /// `macos`, `linux`, `windows`, or `any`.
    pub platform: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    /// `zip`, `tar.gz`, `tar.xz`, `tar`, `sf2`, `sfz`, `wav`, `none`, or an installer type
    /// (`dmg`, `pkg`, `exe`, `deb`) that ferraus refuses to run.
    pub archive: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
    /// Where the useful files sit inside the archive (informational).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub contents: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InstallFrom {
    pub url: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: SoundKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub description: String,
    pub license: License,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub downloads: Vec<Download>,
    /// `verified` (every download hashed), `unverified`, or `vendor` (install --from only).
    pub status: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Instrument names this entry is a good default for (`sounds alias piano=<id>`).
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub install_from: Option<InstallFrom>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

impl Entry {
    /// The downloads to fetch on this platform (all parts), or `None` when the entry has
    /// nothing for it.
    pub fn downloads_for(&self, os: &str) -> Vec<&Download> {
        let own: Vec<&Download> = self.downloads.iter().filter(|d| d.platform == os).collect();
        if !own.is_empty() {
            return own;
        }
        self.downloads.iter().filter(|d| d.platform == "any").collect()
    }

    pub fn size_for(&self, os: &str) -> Option<u64> {
        let d = self.downloads_for(os);
        if d.is_empty() || d.iter().any(|d| d.size_bytes.is_none()) {
            return None;
        }
        Some(d.iter().filter_map(|d| d.size_bytes).sum())
    }

    pub fn verified_for(&self, os: &str) -> bool {
        let d = self.downloads_for(os);
        !d.is_empty() && d.iter().all(|d| d.sha256.as_deref().map(|s| s.len() == 64).unwrap_or(false))
    }

    pub fn is_vendor_only(&self) -> bool {
        self.install_from.is_some() && self.downloads.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    /// ISO date; a cached manifest replaces the embedded one only when newer.
    pub updated: String,
    pub entries: Vec<Entry>,
}

impl Manifest {
    /// Look up by id, then name, then a suggested alias (`piano`), preferring entries
    /// that are verified for this platform.
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .or_else(|| self.entries.iter().find(|e| e.id.eq_ignore_ascii_case(id)))
            .or_else(|| self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(id)))
            .or_else(|| {
                let os = current_os();
                let mut hits: Vec<&Entry> =
                    self.entries.iter().filter(|e| e.aliases.iter().any(|a| a.eq_ignore_ascii_case(id))).collect();
                hits.sort_by_key(|e| (!e.verified_for(os), e.is_vendor_only()));
                hits.first().copied()
            })
    }

    /// Entries matching every word of `query` in id, name, tags, category, aliases, type,
    /// or license; exact id/name/alias hits first.
    pub fn search(&self, query: &str) -> Vec<&Entry> {
        let words: Vec<String> = query.to_ascii_lowercase().split_whitespace().map(|s| s.to_string()).collect();
        let mut scored: Vec<(u32, &Entry)> = Vec::new();
        for e in &self.entries {
            let hay = format!(
                "{} {} {} {} {} {} {} {}",
                e.id,
                e.name,
                e.tags.join(" "),
                e.category.clone().unwrap_or_default(),
                e.aliases.join(" "),
                e.kind.as_str(),
                e.formats.join(" "),
                e.license.spdx
            )
            .to_ascii_lowercase();
            if !words.iter().all(|w| hay.contains(w.as_str())) {
                continue;
            }
            let q = query.trim().to_ascii_lowercase();
            let score = if e.id == q || e.name.to_ascii_lowercase() == q {
                0
            } else if e.aliases.iter().any(|a| a == &q) {
                1
            } else if e.tags.iter().any(|t| t == &q) || e.category.as_deref() == Some(q.as_str()) {
                2
            } else {
                3
            };
            scored.push((score, e));
        }
        scored.sort_by_key(|(s, e)| (*s, e.id.clone()));
        scored.into_iter().map(|(_, e)| e).collect()
    }
}

/// The running OS as the registry names it.
pub fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

fn decode_key(b64: &str) -> Result<VerifyingKey> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| Error::internal(format!("bad registry public key: {e}")))?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| Error::internal("registry public key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&arr).map_err(|e| Error::internal(format!("bad registry public key: {e}")))
}

/// Verify a detached Ed25519 signature (base64) over `bytes` with a base64 public key.
pub fn verify_signature(bytes: &[u8], signature_b64: &str, public_key_b64: &str) -> Result<()> {
    let key = decode_key(public_key_b64)?;
    let sig = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.trim())
        .map_err(|_| Error::verification("registry signature is not valid base64"))?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| Error::verification("registry signature must be 64 bytes"))?;
    key.verify(bytes, &Signature::from_bytes(&sig))
        .map_err(|_| Error::verification("sounds registry signature does not match its contents"))
}

/// Parse and verify a manifest.
pub fn parse_signed(bytes: &[u8], signature_b64: &str, public_key_b64: &str) -> Result<Manifest> {
    verify_signature(bytes, signature_b64, public_key_b64)?;
    let m: Manifest =
        serde_json::from_slice(bytes).map_err(|e| Error::verification(format!("sounds registry is malformed: {e}")))?;
    Ok(m)
}

fn cached_registry(env: &SoundsEnv) -> PathBuf {
    env.cache_dir.join("sounds").join("registry.json")
}

/// The manifest in use: the cached one from `sounds update` when it verifies and is newer
/// than the embedded one, else the embedded one.
pub fn load_manifest(env: &SoundsEnv) -> Result<Manifest> {
    let embedded = parse_signed(EMBEDDED_REGISTRY, EMBEDDED_SIGNATURE, REGISTRY_PUBLIC_KEY)
        .map_err(|e| Error::internal(format!("embedded sounds registry failed verification: {}", e.message)))?;
    let cp = cached_registry(env);
    if let (Ok(b), Ok(s)) = (std::fs::read(&cp), std::fs::read_to_string(cp.with_extension("json.sig"))) {
        if let Ok(m) = parse_signed(&b, &s, REGISTRY_PUBLIC_KEY) {
            if m.updated > embedded.updated {
                return Ok(m);
            }
        }
    }
    Ok(embedded)
}

fn human_size(n: Option<u64>) -> String {
    match n {
        None => "unknown".into(),
        Some(b) if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        Some(b) if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        Some(b) => format!("{:.0} KB", b as f64 / 1024.0),
    }
}

fn entry_summary(e: &Entry, env: &SoundsEnv) -> Value {
    let os = current_os();
    let installed = resolve::read_installed(env).iter().any(|r| r.id == e.id);
    json!({
        "id": e.id, "name": e.name, "type": e.kind, "formats": e.formats,
        "version": e.version, "license": e.license.spdx, "category": e.category,
        "tags": e.tags, "aliases": e.aliases,
        "status": if e.is_vendor_only() { "vendor".to_string() } else if e.verified_for(os) { "verified".to_string() } else { e.status.clone() },
        "available_here": !e.downloads_for(os).is_empty() || e.install_from.is_some(),
        "size_bytes": e.size_for(os), "size": human_size(e.size_for(os)),
        "installed": installed,
        "install_from": e.install_from,
    })
}

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("sounds search", Category::Plugins, "Search the sounds registry", sounds_search)
            .about("Searches the curated registry of free instruments, SoundFonts, SFZ libraries, and impulse responses by name, tag (piano, synth, drums, strings), type, or license. Shows size, license, and whether each entry is verified (has a pinned SHA-256) for this platform.")
            .positional("query")
            .param(param("query", ParamKind::Str).help("Words to match in name, id, tags, category, type, or license (empty lists everything)"))
            .param(param("type", ParamKind::List).help("Only these types: clap, vst3, sf2, sfz, ir"))
            .param(param("license", ParamKind::Str).help("Only entries whose SPDX license contains this text (e.g. CC0, GPL, MIT)"))
            .example("ferraus sounds search piano", "Find piano sounds")
            .example("ferraus sounds search reverb --type ir,clap --license CC0 --json", "CC0 reverbs and impulse responses as JSON")
            .example("ferraus sounds search strings --type sfz", "String libraries in SFZ format"),
        OpSpec::custom("sounds info", Category::Plugins, "Show a registry entry's license, size, and downloads", sounds_info)
            .about("Prints the license text and download size before anything is downloaded, plus the files that would be fetched on this platform and whether they have pinned SHA-256 hashes.")
            .positional("id")
            .param(param("id", ParamKind::Str).required().help("Registry id or name (see 'sounds search')"))
            .example("ferraus sounds info salamander-grand-piano", "License and size of the Salamander piano")
            .example("ferraus sounds info gm-basic --json", "Entry details as JSON")
            .example("ferraus sounds info surge-xt", "Version, license, and download size of the Surge XT synthesizer"),
        OpSpec::custom("sounds install", Category::Plugins, "Download, verify, and install a sound", sounds_install)
            .about("Downloads a registry entry (resuming partial downloads), verifies its SHA-256, refuses any mismatch, unpacks zip and tar archives, and places plugins in the per-user plugin folder and SoundFonts, SFZ libraries, and impulse responses in <data>/ferraus/sounds/, then rescans. The license is printed first. Downloads need --yes (or auto-download enabled in the config); --offline forbids them. Entries without a pinned hash are refused unless FERRAUS_ALLOW_UNVERIFIED=1. With --from, registers a bundle you downloaded yourself (for free-but-proprietary instruments whose vendors require their own installer); ferraus never runs installers or asks for administrator rights.")
            .positional("id")
            .param(param("id", ParamKind::Str).help("Registry id (optional with --from, where it names the entry the file belongs to)"))
            .param(param("from", ParamKind::Path).help("Install a file or folder you downloaded yourself: a .clap/.vst3 bundle, .sf2, .sfz library folder, IR folder, or a zip/tar archive of those"))
            .param(param("name", ParamKind::Str).help("Name for a --from install (default: the file name)"))
            .example("ferraus sounds install gm-basic -y", "Download and install the default General MIDI SoundFont")
            .example("ferraus sounds install vital --from ~/Downloads/VitalInstaller.zip", "Register a plugin downloaded from its vendor")
            .example("ferraus sounds install --from ~/Downloads/MyStrings.sf2 --name my-strings", "Install a SoundFont you already have under your own name"),
        OpSpec::custom("sounds list", Category::Plugins, "List installed sounds, or what a folder contains", sounds_list)
            .about("Without --path, lists installed sounds and everything in --sound-path folders. With --path, shows what that folder contains (CLAP/VST3 plugins, SF2, SFZ, impulse responses) with the category ferraus inferred for each: keys, strings, guitar, bass, drums, synth, pad, brass, winds, vocal, fx.")
            .inputs(Inputs::NONE)
            .param(param("path", ParamKind::Path).help("Folder to inspect instead of the installed sounds"))
            .param(param("category", ParamKind::Str).help("Only sounds in this category (keys, strings, guitar, bass, drums, synth, pad, brass, winds, vocal, fx)"))
            .example("ferraus sounds list", "Installed sounds and sound-path folders")
            .example("ferraus sounds list --path ~/Samples --json", "What a folder holds, with inferred categories, as JSON")
            .example("ferraus sounds list --category keys", "Only installed keyboard sounds (pianos, electric pianos, organs)"),
        OpSpec::custom("sounds remove", Category::Plugins, "Remove an installed sound", sounds_remove)
            .about("Deletes the files an install created (only inside ferraus' sounds folder and the per-user plugin folders) and updates the plugin scan cache.")
            .positional("id")
            .param(param("id", ParamKind::Str).required().help("Installed sound id (see 'sounds list')"))
            .example("ferraus sounds remove gm-basic", "Uninstall the GM SoundFont")
            .example("ferraus sounds remove dexed --json", "Uninstall a plugin and report the removed paths")
            .example("ferraus sounds remove salamander-grand-piano", "Free the disk space used by the Salamander piano"),
        OpSpec::custom("sounds update", Category::Plugins, "Fetch the latest signed sounds registry", sounds_update)
            .about("Downloads the newest registry manifest and its signature, verifies the Ed25519 signature against the key built into ferraus, and caches it when it is newer than the built-in one. Also reports installed sounds whose registry version changed.")
            .inputs(Inputs::NONE)
            .param(param("url", ParamKind::Str).help("Manifest URL (default: the project's published registry, or FERRAUS_SOUNDS_REGISTRY_URL)").advanced())
            .example("ferraus sounds update", "Refresh the registry")
            .example("ferraus sounds update --json", "Refresh and report changes as JSON")
            .example("ferraus sounds update --url https://example.com/sounds/registry.json", "Use a mirror or private registry instead of the published one"),
        OpSpec::custom("sounds alias", Category::Plugins, "Set what an instrument name resolves to", sounds_alias)
            .about("Writes [aliases] in <config>/sounds/aliases.toml so --instrument piano or --to piano picks the sound you chose. The target is an installed sound id, a registry id, or an explicit source such as plugin:\"Surge XT\" or sf2:/path/gm.sf2. With no arguments, lists the aliases.")
            .positional("assignment")
            .param(param("assignment", ParamKind::Str).repeatable().help("name=target pairs, e.g. piano=salamander-grand-piano"))
            .param(param("remove", ParamKind::List).help("Alias names to delete"))
            .example("ferraus sounds alias piano=salamander-grand-piano", "Make 'piano' mean the Salamander Grand Piano")
            .example("ferraus sounds alias strings=sfz:/libs/vsco/Strings.sfz pad='plugin:\"Surge XT\"'", "Point two names at explicit sources")
            .example("ferraus sounds alias --remove piano,pad", "Delete two aliases so the names resolve to their defaults again"),
        OpSpec::custom("sounds scan", Category::Plugins, "Index a folder of sounds", sounds_scan)
            .about("Indexes a folder once (plugins are probed in a host subprocess) and caches the result in <cache>/sounds/index/. With --watch, keeps running and re-indexes whenever files in the folder change (polling every --interval).")
            .positional("dir")
            .param(param("dir", ParamKind::Path).required().help("Folder to index"))
            .param(param("watch", ParamKind::Flag).help("Keep watching and re-index on changes until interrupted"))
            .param(param("interval", ParamKind::Time).default("2s").help("Polling interval for --watch"))
            .param(param("watch-cycles", ParamKind::Int).help("Stop --watch after this many polls (for scripts and tests)").advanced())
            .example("ferraus sounds scan ~/Samples", "Index a sample folder")
            .example("ferraus sounds scan ~/Samples --watch --interval 5s", "Re-index whenever the folder changes")
            .example("ferraus sounds scan /Volumes/Libraries/SFZ --json", "Index an external drive of SFZ libraries and report what was found"),
    ]
}

fn sounds_search(a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let m = load_manifest(&env)?;
    let q = a.str("query").unwrap_or_default();
    let types: Vec<SoundKind> = a
        .list("type")
        .iter()
        .map(|t| {
            SoundKind::parse(t).ok_or_else(|| Error::usage(format!("--type: '{t}' is not clap, vst3, sf2, sfz, or ir")))
        })
        .collect::<Result<_>>()?;
    let lic = a.str("license").map(|s| s.to_ascii_lowercase());
    let hits: Vec<Value> = m
        .search(&q)
        .into_iter()
        .filter(|e| {
            types.is_empty()
                || types.contains(&e.kind)
                || e.formats.iter().any(|f| types.iter().any(|t| t.as_str() == f))
        })
        .filter(|e| lic.as_ref().map(|l| e.license.spdx.to_ascii_lowercase().contains(l)).unwrap_or(true))
        .map(|e| entry_summary(e, &env))
        .collect();
    Ok(json!({"query": q, "count": hits.len(), "results": hits, "registry_updated": m.updated}))
}

fn find_entry<'a>(m: &'a Manifest, id: &str) -> Result<&'a Entry> {
    m.get(id).ok_or_else(|| {
        let near: Vec<&str> = m.search(id).iter().take(5).map(|e| e.id.as_str()).collect();
        Error::usage(format!(
            "no sound '{id}' in the registry{}",
            if near.is_empty() { String::new() } else { format!("; did you mean: {}", near.join(", ")) }
        ))
        .with_hint(format!("ferraus sounds search {id}"))
    })
}

fn license_block(e: &Entry) -> String {
    format!(
        "{} — license {}{}\n{}",
        e.name,
        e.license.spdx,
        if e.license.url.is_empty() { String::new() } else { format!(" ({})", e.license.url) },
        e.license.text
    )
}

fn sounds_info(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let m = load_manifest(&env)?;
    let e = find_entry(&m, &a.str("id").unwrap_or_default())?;
    ctx.progress.log(&license_block(e));
    let os = current_os();
    let mut v = entry_summary(e, &env);
    v["license"] = serde_json::to_value(&e.license)?;
    v["license_text"] = json!(license_block(e));
    v["homepage"] = json!(e.homepage);
    v["description"] = json!(e.description);
    v["notes"] = json!(e.notes);
    v["platform"] = json!(os);
    v["downloads"] = serde_json::to_value(e.downloads_for(os))?;
    v["all_downloads"] = serde_json::to_value(&e.downloads)?;
    Ok(v)
}

fn sounds_install(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let m = load_manifest(&env)?;
    let id = a.str("id");
    if let Some(from) = a.path("from") {
        ctx.check_allowed(&from)?;
        let entry = match &id {
            Some(i) => Some(find_entry(&m, i)?.clone()),
            None => None,
        };
        let rec = install::install_from(&env, &from, entry.as_ref(), a.str("name").as_deref(), ctx.globals.dry_run)?;
        return Ok(json!({"installed": rec, "source": "user", "dry_run": ctx.globals.dry_run}));
    }
    let id = id.ok_or_else(|| {
        Error::usage("name a registry id, or pass --from <file>").with_hint("ferraus sounds search piano")
    })?;
    let e = find_entry(&m, &id)?.clone();
    if e.id != id {
        ctx.note("resolved_id", e.id.clone());
    }
    ctx.progress.log(&license_block(&e));
    let opts = install::InstallOptions {
        yes: ctx.globals.yes || std::env::var("FERRAUS_AUTO_DOWNLOAD").map(|v| v == "1").unwrap_or(false),
        offline: ctx.globals.offline || std::env::var("FERRAUS_OFFLINE").map(|v| v == "1").unwrap_or(false),
        allow_unverified: std::env::var("FERRAUS_ALLOW_UNVERIFIED").map(|v| v == "1").unwrap_or(false),
        dry_run: ctx.globals.dry_run,
        os: current_os().to_string(),
        watchdog: default_watchdog(),
    };
    let progress = ctx.progress;
    let command = ctx.command.clone();
    let rec = install::install_entry(&env, &e, &opts, &mut |f| progress.progress(&command, f))?;
    if let Some(r) = &rec {
        // installed paths are reported in the result, not as outputs: a sidecar has no
        // place inside the sounds folder
        ctx.metric("installed_paths", r.paths.len());
        // `sounds install piano` also makes `piano` resolve to what was installed
        if e.aliases.iter().any(|a| a.eq_ignore_ascii_case(&id)) && !id.eq_ignore_ascii_case(&e.id) {
            let mut aliases = resolve::read_aliases(&env);
            if !aliases.contains_key(&id) {
                aliases.insert(id.clone(), e.id.clone());
                resolve::write_aliases(&env, &aliases)?;
                ctx.note("alias_set", format!("{id}={}", e.id));
            }
        }
    }
    Ok(json!({
        "id": e.id,
        "license": e.license,
        "license_text": license_block(&e),
        "installed": rec,
        "dry_run": ctx.globals.dry_run,
        "plan": if rec.is_none() { serde_json::to_value(e.downloads_for(current_os()))? } else { Value::Null },
    }))
}

fn sound_json(s: &resolve::Sound) -> Value {
    json!({
        "id": s.id, "name": s.name, "type": s.kind, "category": s.category,
        "instrument": s.instrument, "path": s.path.display().to_string(),
        "plugin_id": s.plugin_id, "source": s.source(), "origin": s.origin,
        "registry_id": s.registry_id, "tags": s.tags,
    })
}

fn sounds_list(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let cat = a.str("category").map(|c| c.to_ascii_lowercase());
    let keep = |s: &resolve::Sound| cat.as_ref().map(|c| s.category.as_deref() == Some(c.as_str())).unwrap_or(true);
    if let Some(dir) = a.path("path") {
        ctx.check_allowed(&dir)?;
        let sounds = resolve::discover_folder(&env, &dir, default_watchdog())?;
        let list: Vec<Value> = sounds.iter().filter(|s| keep(s)).map(sound_json).collect();
        let mut by_cat: BTreeMap<String, usize> = BTreeMap::new();
        for s in sounds.iter().filter(|s| keep(s)) {
            *by_cat.entry(s.category.clone().unwrap_or_else(|| "unknown".into())).or_default() += 1;
        }
        return Ok(
            json!({"path": dir.display().to_string(), "count": list.len(), "categories": by_cat, "sounds": list}),
        );
    }
    let installed = resolve::read_installed(&env);
    let sounds = resolve::discover(&env, &ctx.globals.sound_paths, default_watchdog())?;
    let list: Vec<Value> = sounds
        .iter()
        .filter(|s| s.origin != "plugin-scan" || s.instrument)
        .filter(|s| keep(s))
        .map(sound_json)
        .collect();
    Ok(json!({
        "installed": installed,
        "sounds": list,
        "count": list.len(),
        "sounds_dir": env.sounds_dir.display().to_string(),
        "aliases": resolve::read_aliases(&env),
    }))
}

fn sounds_remove(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let id = a.str("id").unwrap_or_default();
    let removed = install::remove(&env, &id, ctx.globals.dry_run, default_watchdog())?;
    Ok(json!({"id": id, "removed": removed, "dry_run": ctx.globals.dry_run}))
}

fn sounds_update(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let url = a
        .str("url")
        .or_else(|| std::env::var("FERRAUS_SOUNDS_REGISTRY_URL").ok())
        .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string());
    if ctx.globals.offline || std::env::var("FERRAUS_OFFLINE").map(|v| v == "1").unwrap_or(false) {
        return Err(Error::new(ErrorKind::Network, "offline", "cannot update the sounds registry with --offline"));
    }
    let before = load_manifest(&env)?;
    if ctx.globals.dry_run {
        return Ok(json!({"url": url, "current": before.updated, "dry_run": true}));
    }
    let body = install::http_get_small(&url)?;
    let sig = install::http_get_small(&format!("{url}.sig"))?;
    let sig = String::from_utf8_lossy(&sig).to_string();
    let m = parse_signed(&body, &sig, REGISTRY_PUBLIC_KEY)?;
    let newer = m.updated > before.updated;
    if newer {
        let cp = cached_registry(&env);
        if let Some(p) = cp.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&cp, &body)?;
        std::fs::write(cp.with_extension("json.sig"), sig.as_bytes())?;
    }
    let installed = resolve::read_installed(&env);
    let changed: Vec<Value> = installed
        .iter()
        .filter_map(|r| {
            let e = m.get(&r.id)?;
            let v = e.version.clone().unwrap_or_default();
            (v != r.version && !v.is_empty()).then(|| json!({"id": r.id, "installed": r.version, "available": v}))
        })
        .collect();
    Ok(
        json!({"url": url, "previous": before.updated, "updated": m.updated, "replaced": newer, "entries": m.entries.len(), "upgrades": changed}),
    )
}

fn sounds_alias(a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let mut aliases = resolve::read_aliases(&env);
    let mut changed = Vec::new();
    let mut warnings = Vec::new();
    let manifest = load_manifest(&env).ok();
    let installed = resolve::read_installed(&env);
    for assign in a.all("assignment") {
        for part in assign.split_whitespace().filter(|s| !s.is_empty()) {
            let (k, v) = part.split_once('=').ok_or_else(|| {
                Error::usage(format!("'{part}' must be name=target, e.g. piano=salamander-grand-piano"))
            })?;
            let (k, v) = (k.trim(), v.trim().trim_matches('\''));
            if k.is_empty() || v.is_empty() {
                return Err(Error::usage(format!("'{part}' must be name=target")));
            }
            let known = v.contains(':')
                || installed.iter().any(|r| r.id == v)
                || manifest.as_ref().map(|m| m.get(v).is_some()).unwrap_or(false);
            if !known {
                warnings.push(format!("'{v}' is not an installed sound, registry id, or explicit source yet"));
            }
            aliases.insert(k.to_string(), v.to_string());
            changed.push(json!({"name": k, "target": v}));
        }
    }
    let mut removed = Vec::new();
    for r in a.list("remove") {
        if aliases.remove(&r).is_some() {
            removed.push(r);
        }
    }
    if !changed.is_empty() || !removed.is_empty() {
        resolve::write_aliases(&env, &aliases)?;
    }
    for w in &warnings {
        _ctx.warn("alias_target_unknown", w.clone());
    }
    Ok(
        json!({"aliases": aliases, "set": changed, "removed": removed, "file": env.aliases_file().display().to_string()}),
    )
}

/// A cheap fingerprint of a folder tree (paths, sizes, mtimes).
pub fn tree_stamp(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fn rec(d: &Path, depth: usize, h: &mut std::collections::hash_map::DefaultHasher) {
        if depth > 10 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        v.sort();
        for p in v {
            p.hash(h);
            if let Ok(m) = std::fs::metadata(&p) {
                m.len().hash(h);
                if let Ok(t) = m.modified() {
                    t.hash(h);
                }
                if m.is_dir() {
                    rec(&p, depth + 1, h);
                }
            }
        }
    }
    rec(dir, 0, &mut h);
    h.finish()
}

/// Index a folder into `<cache>/sounds/index/<hash>.json`.
pub fn index_folder(env: &SoundsEnv, dir: &Path) -> Result<(PathBuf, Vec<resolve::Sound>)> {
    let sounds = resolve::discover_folder(env, dir, default_watchdog())?;
    let abs = ferraus_core::ctx::absolutize(dir);
    use sha2::Digest;
    let key = hex::encode(&sha2::Sha256::digest(abs.to_string_lossy().as_bytes())[..8]);
    let f = env.index_dir().join(format!("{key}.json"));
    std::fs::create_dir_all(env.index_dir())?;
    std::fs::write(
        &f,
        serde_json::to_vec_pretty(
            &json!({"dir": abs.display().to_string(), "stamp": tree_stamp(dir), "sounds": sounds}),
        )?,
    )?;
    Ok((f, sounds))
}

fn sounds_scan(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let env = SoundsEnv::from_env();
    let dir = a.path("dir").ok_or_else(|| Error::usage("name a folder to scan"))?;
    ctx.check_allowed(&dir)?;
    if !dir.is_dir() {
        return Err(Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("folder not found: {}", dir.display()),
        ));
    }
    let (mut index, mut sounds) = index_folder(&env, &dir)?;
    let mut reindexed = 0usize;
    if a.flag("watch") {
        let interval = std::time::Duration::from_secs_f64(a.seconds("interval")?.max(0.05));
        let cycles = if a.has("watch-cycles") { Some(a.usize("watch-cycles")?) } else { None };
        let mut stamp = tree_stamp(&dir);
        let mut n = 0usize;
        ctx.progress.log(&format!("watching {} (Ctrl-C to stop)", dir.display()));
        loop {
            if cycles.map(|c| n >= c).unwrap_or(false) || ctx.check_cancel().is_err() {
                break;
            }
            std::thread::sleep(interval);
            n += 1;
            let s = tree_stamp(&dir);
            if s != stamp {
                stamp = s;
                let r = index_folder(&env, &dir)?;
                index = r.0;
                sounds = r.1;
                reindexed += 1;
                ctx.progress.log(&format!("re-indexed {}: {} sounds", dir.display(), sounds.len()));
            }
        }
    }
    Ok(json!({
        "dir": dir.display().to_string(),
        "index": index.display().to_string(),
        "count": sounds.len(),
        "reindexed": reindexed,
        "sounds": sounds.iter().map(sound_json).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_registry_verifies_and_parses() {
        let m = parse_signed(EMBEDDED_REGISTRY, EMBEDDED_SIGNATURE, REGISTRY_PUBLIC_KEY).unwrap();
        assert!(m.entries.len() >= 12);
        for id in ["surge-xt", "gm-basic", "ir-pack-basic", "salamander-grand-piano", "vital"] {
            assert!(m.get(id).is_some(), "{id} missing");
        }
        // every verified entry really has hashes; unverified ones never claim them
        for e in &m.entries {
            match e.status.as_str() {
                "verified" => assert!(e.downloads.iter().all(|d| d.sha256.is_some()), "{}", e.id),
                "vendor" => assert!(e.install_from.is_some(), "{}", e.id),
                _ => {}
            }
            assert!(!e.license.spdx.is_empty() && !e.license.text.is_empty(), "{}", e.id);
        }
        assert_eq!(m.search("piano")[0].id, "salamander-grand-piano");
    }

    #[test]
    fn tampered_registry_is_rejected() {
        let mut bytes = EMBEDDED_REGISTRY.to_vec();
        let i = bytes.iter().position(|b| *b == b'G').unwrap();
        bytes[i] = b'H';
        let e = parse_signed(&bytes, EMBEDDED_SIGNATURE, REGISTRY_PUBLIC_KEY).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Verification);
    }
}
