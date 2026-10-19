//! The public entry point other crates use: `ensure_model(ctx, name)`. It resolves the name
//! in the active registry, returns the cached file when present, and otherwise applies the
//! download policy (ADR 0004): download only with `-y` / `models.auto_download = true`
//! (`FERRAUS_AUTO_DOWNLOAD=1`), never with `--offline`; a miss is exit code 5 naming the
//! exact `ferraus models pull` command.

use crate::cache::{max_cache_bytes, Cache, LockEntry};
use crate::download::{download, DownloadRequest};
use crate::manifest::{embedded_public_key, parse_signed, Entry, Manifest, Status};
use crate::spec::ModelSpec;
use ferraus_core::{Error, ErrorKind, OpCtx, Result};
use std::path::PathBuf;

/// A model ready to load.
#[derive(Clone, Debug)]
pub struct CachedModel {
    pub path: PathBuf,
    pub spec: ModelSpec,
    pub entry: Entry,
}

/// Overrides for tests and for commands with their own policy.
#[derive(Clone, Debug, Default)]
pub struct EnsureOptions {
    /// The user passed `--accept-license` for a research-only model.
    pub accept_license: bool,
    /// Use this cache instead of the platform default.
    pub cache: Option<Cache>,
    /// Use this manifest instead of the active registry.
    pub manifest: Option<Manifest>,
    /// `Some(true)` for explicit downloads (`models pull`), `None` = the global policy.
    pub allow_download: Option<bool>,
}

fn env_is(k: &str, v: &str) -> bool {
    std::env::var(k).map(|x| x.trim() == v).unwrap_or(false)
}

/// `--offline` (or `FERRAUS_OFFLINE=1`).
pub fn offline(ctx: &OpCtx) -> bool {
    ctx.globals.offline || env_is("FERRAUS_OFFLINE", "1")
}

/// Whether an implicit download is allowed (`-y` or the config opt-in), per ADR 0004.
pub fn downloads_allowed(ctx: &OpCtx) -> bool {
    !offline(ctx) && (ctx.globals.yes || env_is("FERRAUS_AUTO_DOWNLOAD", "1"))
}

/// `FERRAUS_ALLOW_UNVERIFIED=1` permits entries without a recorded SHA-256.
pub fn allow_unverified() -> bool {
    env_is("FERRAUS_ALLOW_UNVERIFIED", "1")
}

/// The exit-code-5 error for models that are not cached.
pub fn not_cached_error(names: &[String]) -> Error {
    let list = names.join(" ");
    Error::new(
        ErrorKind::ModelNotCached,
        "model_not_cached",
        format!(
            "model{} not cached and downloads are disabled: {list}",
            if names.len() == 1 { " is" } else { "s are" }
        ),
    )
    .with_hint(format!("ferraus models pull {list}"))
}

/// Paths of the updated and pinned manifests inside a cache.
pub fn updated_manifest_path(cache: &Cache) -> PathBuf {
    cache.root.join("registry.json")
}

pub fn pinned_manifest_path(cache: &Cache) -> PathBuf {
    cache.root.join("registry.pinned.json")
}

pub fn sig_path(p: &std::path::Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

fn read_signed(path: &std::path::Path) -> Option<Manifest> {
    let body = std::fs::read(path).ok()?;
    let sig = std::fs::read_to_string(sig_path(path)).ok()?;
    parse_signed(&embedded_public_key(), &body, &sig).ok()
}

/// Which manifest is in effect and where it came from.
pub struct Active {
    pub manifest: Manifest,
    pub source: &'static str,
}

/// The active registry: a pinned snapshot wins outright; otherwise the embedded manifest
/// plus any newer entries from a verified `models registry update`. Tier pins always come
/// from the embedded manifest (updates never change behavior silently).
pub fn active_registry(cache: &Cache) -> Active {
    if let Some(m) = read_signed(&pinned_manifest_path(cache)) {
        return Active { manifest: m, source: "pinned" };
    }
    let mut m = Manifest::embedded();
    if let Some(updated) = read_signed(&updated_manifest_path(cache)) {
        for e in updated.models {
            if !m.models.iter().any(|x| x.name == e.name && x.version == e.version) {
                m.models.push(e);
            }
        }
        return Active { manifest: m, source: "embedded+updated" };
    }
    Active { manifest: m, source: "embedded" }
}

pub fn ensure_model(ctx: &mut OpCtx, name: &str) -> Result<CachedModel> {
    ensure_model_with(ctx, name, &EnsureOptions::default())
}

pub fn ensure_model_with(ctx: &mut OpCtx, name: &str, opts: &EnsureOptions) -> Result<CachedModel> {
    let cache = opts.cache.clone().unwrap_or_else(Cache::default_root);
    let manifest = match &opts.manifest {
        Some(m) => m.clone(),
        None => active_registry(&cache).manifest,
    };
    let entry = manifest.resolve(name)?.clone();
    if entry.is_builtin() {
        return Err(Error::usage(format!("{} is built into ferraus and has no model file", entry.name)));
    }
    check_license(ctx, &cache, &entry, opts.accept_license)?;
    let rec = match cache.cached(&entry) {
        Some(rec) => {
            cache.touch(&entry.name, &entry.version);
            rec
        }
        None => {
            let allowed = match opts.allow_download {
                Some(a) => a && !offline(ctx),
                None => downloads_allowed(ctx),
            };
            if !allowed {
                return Err(not_cached_error(&[entry.id()]));
            }
            install(ctx, &cache, &entry)?
        }
    };
    ctx.model_used(&entry.name, &entry.version, &rec.sha256);
    Ok(CachedModel { path: cache.root.join(&rec.path), spec: ModelSpec::from_entry(&entry), entry })
}

/// Research-only licenses need `--accept-license` once; unclear licenses warn.
pub fn check_license(ctx: &mut OpCtx, cache: &Cache, e: &Entry, accept: bool) -> Result<()> {
    if e.license.research_only() {
        if accept {
            if !ctx.globals.dry_run {
                cache.accept_license(&e.id())?;
            }
        } else if !cache.license_accepted(&e.id()) {
            return Err(Error::new(
                ErrorKind::Usage,
                "license_not_accepted",
                format!(
                    "{} is licensed {} and is not for commercial use; read {} and accept it explicitly",
                    e.id(),
                    e.license.label(),
                    e.license.url.as_deref().unwrap_or("its model card")
                ),
            )
            .with_hint(format!("ferraus models pull {} --accept-license", e.id())));
        }
    } else if e.license.unclear() {
        ctx.warn(
            "model_license_unclear",
            format!(
                "{} has no clear license; it is excluded from default tiers. Check {} before using its output commercially",
                e.id(),
                e.source.weights.as_deref().unwrap_or("its source")
            ),
        );
    }
    Ok(())
}

/// Download, verify, and record one entry (explicit or policy-approved).
pub fn install(ctx: &mut OpCtx, cache: &Cache, e: &Entry) -> Result<LockEntry> {
    if offline(ctx) {
        return Err(Error::new(ErrorKind::Network, "offline", format!("--offline is set; cannot download {}", e.id())));
    }
    if e.is_builtin() {
        return Err(Error::usage(format!("{} is built in; nothing to download", e.name)));
    }
    if e.status != Status::Verified && !allow_unverified() {
        return Err(Error::new(
            ErrorKind::ModelDownload,
            "model_unverified",
            format!(
                "{} has no verified download (status: unverified{}); refusing to fetch an unchecked file",
                e.id(),
                if e.sha256.is_none() { ", no SHA-256 recorded" } else { "" }
            ),
        )
        .with_hint(format!("FERRAUS_ALLOW_UNVERIFIED=1 ferraus models pull {}", e.id())));
    }
    if e.status == Status::Verified && e.sha256.is_none() {
        return Err(Error::internal(format!("{} is verified but has no hash", e.id())));
    }
    if e.urls.is_empty() {
        return Err(Error::new(
            ErrorKind::ModelDownload,
            "no_download_url",
            format!("{} has no download URL yet", e.id()),
        ));
    }
    let size = e.size_bytes.map(human_bytes).unwrap_or_else(|| "unknown size".into());
    ctx.warn("model_download", format!("downloading {} ({size}, license {})", e.id(), e.license.label()));
    if ctx.globals.dry_run {
        return Err(Error::new(
            ErrorKind::ModelNotCached,
            "model_not_cached",
            format!("dry run: {} would be downloaded ({size})", e.id()),
        )
        .with_hint(format!("ferraus models pull {}", e.id())));
    }
    let dest = cache.model_path(e);
    crate::cache::ensure_inside(&cache.root, &dest)?;
    let progress = ctx.progress;
    let cancel = ctx.cancel.clone();
    let mut log = |line: &str| progress.log(line);
    let check = move || cancel.check();
    let report = download(DownloadRequest {
        urls: &e.urls,
        sha256: e.sha256.as_deref(),
        dest: &dest,
        log: &mut log,
        check_cancel: &check,
    })?;
    if e.sha256.is_none() {
        ctx.warn(
            "model_unverified",
            format!("{} was downloaded without hash verification (sha256 {})", e.id(), report.sha256),
        );
    }
    let mut spec = ModelSpec::from_entry(e);
    spec.sha256 = Some(report.sha256.clone());
    spec.save(&cache.spec_path(e))?;
    let rec = cache.record(e, &report.sha256, report.size_bytes, Some(&report.url))?;
    let budget = max_cache_bytes();
    let pruned = cache.prune_lru(budget, &[e.id()])?;
    if !pruned.is_empty() {
        let names: Vec<String> = pruned.iter().map(|p| format!("{}@{}", p.name, p.version)).collect();
        ctx.warn(
            "model_cache_pruned",
            format!(
                "model cache exceeded {} (models.max_cache_gb); removed least recently used: {}",
                human_bytes(budget),
                names.join(", ")
            ),
        );
    }
    Ok(rec)
}

pub fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.1} kB", b / 1e3)
    } else {
        format!("{b} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::test_server::{start, Behavior};
    use crate::manifest::{License, Status};
    use sha2::Digest;

    fn test_entry(urls: Vec<String>, data: &[u8]) -> Entry {
        let mut e = Manifest::embedded().resolve("separate/htdemucs").unwrap().clone();
        e.name = "separate/test-model".into();
        e.version = "1".into();
        e.urls = urls;
        e.sha256 = Some(hex::encode(sha2::Sha256::digest(data)));
        e.size_bytes = Some(data.len() as u64);
        e.status = Status::Verified;
        e.license = License { spdx: Some("MIT".into()), url: None, commercial_use: Some(true), note: None };
        e
    }

    fn manifest_with(e: Entry) -> Manifest {
        let mut m = Manifest::embedded();
        m.models.push(e);
        m
    }

    #[test]
    fn offline_miss_is_exit_5_with_pull_hint() {
        let dir = ferraus_core::testing::TempDir::new("ensure-offline");
        let mut ctx = ferraus_core::testing::ctx();
        ctx.globals.offline = true;
        ctx.globals.yes = true; // --offline wins over -y
        let e = test_entry(vec!["http://127.0.0.1:9/x".into()], b"abc");
        let opts = EnsureOptions {
            cache: Some(Cache::new(dir.0.clone())),
            manifest: Some(manifest_with(e)),
            ..Default::default()
        };
        let err = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap_err();
        assert_eq!(err.exit_code, 5);
        assert_eq!(err.code, "model_not_cached");
        assert_eq!(err.hint.as_deref(), Some("ferraus models pull separate/test-model@1"));
    }

    #[test]
    fn no_yes_means_no_download() {
        let dir = ferraus_core::testing::TempDir::new("ensure-noyes");
        let mut ctx = ferraus_core::testing::ctx();
        let e = test_entry(vec!["http://127.0.0.1:9/x".into()], b"abc");
        let opts = EnsureOptions {
            cache: Some(Cache::new(dir.0.clone())),
            manifest: Some(manifest_with(e)),
            ..Default::default()
        };
        let err = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap_err();
        assert_eq!(err.exit_code, 5);
    }

    #[test]
    fn yes_downloads_verifies_records_and_reuses() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 7) as u8).collect();
        let srv = start(vec![("/m.onnx", Behavior::Serve(data.clone()))]);
        let dir = ferraus_core::testing::TempDir::new("ensure-yes");
        let mut ctx = ferraus_core::testing::ctx();
        ctx.globals.yes = true;
        let e = test_entry(vec![format!("{}/m.onnx", srv.base)], &data);
        let opts = EnsureOptions {
            cache: Some(Cache::new(dir.0.clone())),
            manifest: Some(manifest_with(e.clone())),
            ..Default::default()
        };
        let m = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap();
        assert_eq!(std::fs::read(&m.path).unwrap(), data);
        assert!(m.path.ends_with("separate/test-model/1/model.onnx"));
        assert!(m.path.with_file_name("spec.json").exists());
        assert_eq!(ctx.models.len(), 1);
        assert!(ctx
            .warnings
            .iter()
            .any(|w| w.code == "model_download" && w.message.contains("20.0 kB") && w.message.contains("MIT")));
        let lock = Cache::new(dir.0.clone()).load_lock();
        assert_eq!(lock.models.len(), 1);
        // second call: cached, no network (server not hit again), works offline
        let hits_before = srv.hits.lock().unwrap().len();
        ctx.globals.offline = true;
        let m2 = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap();
        assert_eq!(m2.path, m.path);
        assert_eq!(srv.hits.lock().unwrap().len(), hits_before);
    }

    #[test]
    fn unverified_entries_are_refused() {
        let dir = ferraus_core::testing::TempDir::new("ensure-unverified");
        let mut ctx = ferraus_core::testing::ctx();
        ctx.globals.yes = true;
        let mut e = test_entry(vec!["http://127.0.0.1:9/x".into()], b"abc");
        e.status = Status::Unverified;
        e.sha256 = None;
        let opts = EnsureOptions {
            cache: Some(Cache::new(dir.0.clone())),
            manifest: Some(manifest_with(e)),
            ..Default::default()
        };
        let err = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap_err();
        assert_eq!(err.code, "model_unverified");
        assert_eq!(err.exit_code, 6);
    }

    #[test]
    fn research_only_needs_accept_license() {
        let dir = ferraus_core::testing::TempDir::new("ensure-license");
        let mut ctx = ferraus_core::testing::ctx();
        let mut e = test_entry(vec!["http://127.0.0.1:9/x".into()], b"abc");
        e.license.commercial_use = Some(false);
        e.license.spdx = Some("CC-BY-NC-4.0".into());
        let mut opts = EnsureOptions {
            cache: Some(Cache::new(dir.0.clone())),
            manifest: Some(manifest_with(e)),
            ..Default::default()
        };
        let err = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap_err();
        assert_eq!(err.code, "license_not_accepted");
        assert!(err.hint.unwrap().contains("--accept-license"));
        opts.accept_license = true;
        // accepted → falls through to the cache policy (not cached → 5)
        let err = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap_err();
        assert_eq!(err.exit_code, 5);
        // acceptance is remembered
        opts.accept_license = false;
        let err = ensure_model_with(&mut ctx, "separate/test-model", &opts).unwrap_err();
        assert_eq!(err.exit_code, 5);
    }
}
