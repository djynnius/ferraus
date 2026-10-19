//! `models ...` commands.

use crate::cache::Cache;
use crate::download::{get_bytes, sha256_file};
use crate::ensure::{
    active_registry, check_license, human_bytes, install, offline, pinned_manifest_path, sig_path,
    updated_manifest_path,
};
use crate::manifest::{embedded_public_key, parse_ref, parse_signed, Entry, Manifest, Status};
use crate::spec::ModelSpec;
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use std::collections::BTreeSet;

/// Default locations of the signed registry manifest (GitHub Releases, Hugging Face mirror).
pub const DEFAULT_REGISTRY_URLS: &[&str] = &[
    "https://github.com/djynnius/ferraus/releases/latest/download/registry.json",
    "https://huggingface.co/djynnius/ferraus-models/resolve/main/registry.json",
];

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom(
            "models list",
            Category::Models,
            "List registry models: version, task, size, license, cached, and which tiers use them",
            list,
        )
        .inputs(Inputs::NONE)
        .about("Shows every model in the active registry. Models whose license forbids commercial use, or that have no clear license, are flagged and never used by default tiers; unverified entries have no recorded checksum yet and are refused by downloads unless FERRAUS_ALLOW_UNVERIFIED=1.")
        .param(param("task", ParamKind::Str).help("Only models for this task (separate, denoise, pitch, transcribe, omr)"))
        .param(param("cached", ParamKind::Flag).help("Only models present in the local cache"))
        .example("ferraus models list --task separate", "Separation models and the tiers that use them")
        .example("ferraus models list --cached", "Only the models already downloaded to this machine")
        .example("ferraus models list --task pitch --json", "Pitch-tracker models with size, license, and cache state as JSON"),
        OpSpec::custom(
            "models pull",
            Category::Models,
            "Download models with resume and SHA-256 verification",
            pull,
        )
        .positional("name")
        .about("Downloads each named model (name or name@version) into the cache, resuming partial downloads and verifying the SHA-256 recorded in the signed registry. The size and license are printed before fetching. --all-for pulls every model a command's tiers need.")
        .param(param("name", ParamKind::Str).repeatable().help("Model name[@version]; several allowed"))
        .param(param("all-for", ParamKind::Str).help("Pull every model the tiers of this command need (e.g. separate)"))
        .param(param("tier", ParamKind::Str).help("With --all-for: only this tier"))
        .param(param("accept-license", ParamKind::Flag).help("Accept a research-only / non-commercial license for the named models"))
        .example("ferraus models pull separate/htdemucs", "Fetch the fast separation model")
        .example("ferraus models pull --all-for separate --tier fast", "Everything the fast separation tier needs")
        .example("ferraus models pull denoise/dfn3 pitch/crepe -y", "Fetch two models at once without the confirmation prompt"),
        OpSpec::custom("models rm", Category::Models, "Remove a cached model", rm)
            .positional("name")
            .param(param("name", ParamKind::Str).required().help("Model name[@version]; without a version every cached version is removed"))
            .example("ferraus models rm separate/htdemucs", "Free the disk space")
            .example("ferraus models rm separate/htdemucs@v4-onnx.2026-05-21", "Remove one cached version and keep any others")
            .example("ferraus models rm pitch/crepe --json", "Remove a model and report the bytes freed as JSON"),
        OpSpec::custom(
            "models prune",
            Category::Models,
            "Remove cached models no tier uses, stale partial downloads, and enforce the cache budget",
            prune,
        )
        .inputs(Inputs::NONE)
        .example("ferraus models prune --dry-run", "Show what would be removed")
        .example("ferraus models prune", "Delete unused models and partial downloads, then enforce the cache budget")
        .example("ferraus models prune --json", "Prune and list every removed file with its size as JSON"),
        OpSpec::custom("models verify", Category::Models, "Re-hash the model cache", verify)
            .positional("name")
            .param(param("name", ParamKind::Str).help("Only this model (default: every cached model)"))
            .example("ferraus models verify", "Check every cached file against its recorded SHA-256")
            .example("ferraus models verify separate/htdemucs", "Re-hash one model, e.g. after a disk error")
            .example("ferraus models verify --json", "Per-model verification results as JSON; exit code 9 on a mismatch"),
        OpSpec::custom(
            "models info",
            Category::Models,
            "Spec, license, source, benchmarks, and cache state of a model",
            info,
        )
        .positional("name")
        .param(param("name", ParamKind::Str).required().help("Model name[@version]"))
        .example("ferraus models info separate/htdemucs", "Everything about the fast tier model")
        .example("ferraus models info denoise/dfn3 --json", "License, source, size, and cache state of the speech denoiser as JSON")
        .example("ferraus models info omr/homr", "Check a model's license (AGPL) before using it"),
        OpSpec::custom("models path", Category::Models, "Print the cached file path of a model", path)
            .positional("name")
            .param(param("name", ParamKind::Str).required().help("Model name[@version]"))
            .example("ferraus models path transcribe/basic-pitch", "Where the ONNX file lives")
            .example("ferraus models path separate/htdemucs --json", "The cached path as JSON")
            .example("ferraus models path pitch/crepe-tiny --offline", "Print the path without ever downloading; fails if the model is not cached"),
        OpSpec::custom(
            "models registry update",
            Category::Models,
            "Refresh the signed registry manifest, or pin a snapshot",
            registry_update,
        )
        .inputs(Inputs::NONE)
        .about("Fetches the Ed25519-signed registry manifest and verifies it with the public key built into ferraus. Updates only advertise newer models: tiers keep the versions pinned by this release until you pass --model name@version or upgrade. --pin freezes a signed snapshot for reproducible deployments.")
        .param(param("pin", ParamKind::Path).help("Freeze this signed manifest (expects <file>.sig beside it)"))
        .param(param("unpin", ParamKind::Flag).help("Remove a pinned snapshot"))
        .param(param("url", ParamKind::Str).help("Fetch the manifest from this URL (default: GitHub Releases, then Hugging Face)"))
        .example("ferraus models registry update", "See whether newer models are available")
        .example("ferraus models registry update --pin registry.json", "Freeze a snapshot for a deployment")
        .example("ferraus models registry update --unpin", "Go back to following the live registry")
        .example("ferraus models registry update --url https://example.com/ferraus/registry.json", "Fetch the signed manifest from a mirror"),
    ]
}

fn cache() -> Cache {
    Cache::default_root()
}

fn entry_json(m: &Manifest, c: &Cache, e: &Entry) -> Value {
    let cached = if e.is_builtin() { json!("builtin") } else { json!(c.cached(e).is_some()) };
    let mut flags = Vec::new();
    if e.license.research_only() {
        flags.push("non_commercial");
    }
    if e.license.unclear() {
        flags.push("no_clear_license");
    }
    if e.status == Status::Unverified {
        flags.push("unverified");
    }
    json!({
        "name": e.name,
        "version": e.version,
        "task": e.task,
        "size_bytes": e.size_bytes,
        "size": e.size_bytes.map(human_bytes),
        "license": e.license.spdx,
        "commercial_use": e.license.commercial_use,
        "status": e.status,
        "cached": cached,
        "tiers": m.tiers_using(e),
        "flags": flags,
    })
}

fn list(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let active = active_registry(&c);
    let task = a.str("task");
    let only_cached = a.flag("cached");
    let mut rows = Vec::new();
    for e in &active.manifest.models {
        if task.as_deref().map(|t| t != e.task).unwrap_or(false) {
            continue;
        }
        if only_cached && c.cached(e).is_none() {
            continue;
        }
        rows.push(entry_json(&active.manifest, &c, e));
    }
    ctx.metric("count", rows.len());
    ctx.note("registry", active.source);
    Ok(Value::Array(rows))
}

/// Every model reference the tiers of `command` need (cascade pseudo-commands included).
pub fn tier_models(m: &Manifest, command: &str, tier: Option<&str>) -> Vec<String> {
    let mut out = BTreeSet::new();
    for (cmd, tiers) in &m.tiers {
        if cmd != command && !cmd.starts_with(&format!("{command}.")) {
            continue;
        }
        for (name, spec) in tiers {
            if cmd == command && tier.map(|t| t != name).unwrap_or(false) {
                continue;
            }
            for mem in spec.all_members() {
                out.insert(mem.model.clone());
            }
        }
    }
    out.into_iter().collect()
}

fn pull(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let active = active_registry(&c);
    let mut refs = a.list("name");
    if let Some(cmd) = a.str("all-for") {
        let tier = a.str("tier");
        let found = tier_models(&active.manifest, &cmd, tier.as_deref());
        if found.is_empty() {
            return Err(Error::usage(format!(
                "no registry tiers for '{cmd}'{}",
                tier.map(|t| format!(" tier '{t}'")).unwrap_or_default()
            ))
            .with_hint("ferraus models list"));
        }
        refs.extend(found);
    }
    if refs.is_empty() {
        return Err(Error::usage("name a model to pull, or pass --all-for <command>").with_hint("ferraus models list"));
    }
    let mut results = Vec::new();
    let mut seen = BTreeSet::new();
    for r in refs {
        let e = active.manifest.resolve(&r)?.clone();
        if !seen.insert(e.id()) {
            continue;
        }
        if e.is_builtin() {
            results.push(json!({"model": e.id(), "status": "builtin"}));
            continue;
        }
        check_license(ctx, &c, &e, a.flag("accept-license"))?;
        if let Some(rec) = c.cached(&e) {
            results.push(json!({"model": e.id(), "status": "cached", "path": c.root.join(&rec.path)}));
            continue;
        }
        if ctx.globals.dry_run {
            let unverified = e.sha256.is_none() || e.urls.is_empty();
            let allow = std::env::var("FERRAUS_ALLOW_UNVERIFIED").map(|v| v == "1").unwrap_or(false);
            let status = if unverified && !allow { "would_refuse_unverified" } else { "would_download" };
            results.push(
                json!({"model": e.id(), "status": status, "size_bytes": e.size_bytes, "license": e.license.label()}),
            );
            continue;
        }
        if offline(ctx) {
            return Err(Error::new(
                ErrorKind::Network,
                "offline",
                format!("--offline is set; cannot download {}", e.id()),
            ));
        }
        let rec = install(ctx, &c, &e)?;
        ctx.model_used(&e.name, &e.version, &rec.sha256);
        results.push(json!({
            "model": e.id(),
            "status": "downloaded",
            "path": c.root.join(&rec.path),
            "sha256": rec.sha256,
            "size_bytes": rec.size_bytes,
        }));
    }
    Ok(Value::Array(results))
}

fn matching_cached(c: &Cache, reference: &str, m: &Manifest) -> Vec<(String, String)> {
    let (name, version) = parse_ref(reference);
    let full = m.versions_of(name).first().map(|e| e.name.clone()).unwrap_or_else(|| name.to_string());
    c.load_lock()
        .models
        .into_iter()
        .filter(|r| r.name == full && version.map(|v| v == r.version).unwrap_or(true))
        .map(|r| (r.name, r.version))
        .collect()
}

fn rm(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let active = active_registry(&c);
    let name = a.str("name").unwrap_or_default();
    let targets = matching_cached(&c, &name, &active.manifest);
    if targets.is_empty() {
        return Err(Error::usage(format!("{name} is not in the cache")).with_hint("ferraus models list --cached"));
    }
    let mut removed = Vec::new();
    for (n, v) in targets {
        if !ctx.globals.dry_run {
            c.remove(&n, &v)?;
        }
        removed.push(format!("{n}@{v}"));
    }
    Ok(json!({"removed": removed}))
}

fn prune(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let active = active_registry(&c);
    let used: BTreeSet<String> = active
        .manifest
        .tiers
        .values()
        .flat_map(|t| t.values())
        .flat_map(|s| s.all_members())
        .map(|m| m.model.clone())
        .collect();
    let mut removed = Vec::new();
    for r in c.load_lock().models {
        let id = format!("{}@{}", r.name, r.version);
        if !used.contains(&id) {
            if !ctx.globals.dry_run {
                c.remove(&r.name, &r.version)?;
            }
            removed.push(id);
        }
    }
    let partials = if ctx.globals.dry_run { Vec::new() } else { c.clean_partials() };
    let lru = if ctx.globals.dry_run { Vec::new() } else { c.prune_lru(crate::cache::max_cache_bytes(), &[])? };
    Ok(json!({
        "removed": removed,
        "partial_downloads_removed": partials.len(),
        "lru_removed": lru.iter().map(|r| format!("{}@{}", r.name, r.version)).collect::<Vec<_>>(),
        "cache_bytes": c.total_bytes(),
    }))
}

fn verify(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let active = active_registry(&c);
    let filter = a.str("name");
    let mut rows = Vec::new();
    let mut bad = Vec::new();
    for r in c.load_lock().models {
        if let Some(f) = &filter {
            let (n, v) = parse_ref(f);
            let full = active.manifest.versions_of(n).first().map(|e| e.name.clone()).unwrap_or_else(|| n.to_string());
            if r.name != full || v.map(|v| v != r.version).unwrap_or(false) {
                continue;
            }
        }
        ctx.check_cancel()?;
        let path = c.root.join(&r.path);
        let expected = active
            .manifest
            .models
            .iter()
            .find(|e| e.name == r.name && e.version == r.version)
            .and_then(|e| e.sha256.clone())
            .unwrap_or_else(|| r.sha256.clone());
        let status = match sha256_file(&path) {
            Ok(h) if h.eq_ignore_ascii_case(&expected) => "ok".to_string(),
            Ok(h) => format!("mismatch (got {h})"),
            Err(e) => format!("unreadable ({})", e.message),
        };
        if status != "ok" {
            bad.push(format!("{}@{}", r.name, r.version));
        }
        rows.push(
            json!({"model": format!("{}@{}", r.name, r.version), "path": path, "sha256": expected, "status": status}),
        );
    }
    if !bad.is_empty() {
        return Err(Error::verification(format!("cached model(s) failed verification: {}", bad.join(", ")))
            .with_hint(format!("ferraus models rm {0} && ferraus models pull {0}", bad[0])));
    }
    ctx.metric("verified", rows.len());
    Ok(Value::Array(rows))
}

fn info(a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let active = active_registry(&c);
    let e = active.manifest.resolve(&a.str("name").unwrap_or_default())?.clone();
    let rec = c.cached(&e);
    let versions: Vec<String> = active.manifest.versions_of(&e.name).iter().map(|x| x.version.clone()).collect();
    Ok(json!({
        "entry": e,
        "spec": ModelSpec::from_entry(&e),
        "license_summary": e.license.label(),
        "tiers": active.manifest.tiers_using(&e),
        "versions": versions,
        "cached": rec.is_some(),
        "path": rec.map(|r| c.root.join(r.path)),
        "registry": active.source,
    }))
}

fn path(a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let active = active_registry(&c);
    let e = active.manifest.resolve(&a.str("name").unwrap_or_default())?;
    if e.is_builtin() {
        return Err(Error::usage(format!("{} is built in and has no file", e.name)));
    }
    match c.cached(e) {
        Some(r) => Ok(json!(c.root.join(r.path).display().to_string())),
        None => Err(crate::ensure::not_cached_error(&[e.id()])),
    }
}

fn registry_update(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let c = cache();
    let key = embedded_public_key();
    if a.flag("unpin") {
        let p = pinned_manifest_path(&c);
        if !ctx.globals.dry_run {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(sig_path(&p));
        }
        return Ok(json!({"unpinned": true}));
    }
    if let Some(pin) = a.path("pin") {
        ctx.check_allowed(&pin)?;
        let body = std::fs::read(&pin)?;
        let sig = std::fs::read_to_string(sig_path(&pin)).map_err(|_| {
            Error::verification(format!("{} has no signature file {}", pin.display(), sig_path(&pin).display()))
        })?;
        let m = parse_signed(&key, &body, &sig)?;
        let dest = pinned_manifest_path(&c);
        if !ctx.globals.dry_run {
            std::fs::create_dir_all(&c.root)?;
            std::fs::write(&dest, &body)?;
            std::fs::write(sig_path(&dest), sig.trim())?;
        }
        return Ok(json!({"pinned": dest, "models": m.models.len()}));
    }
    if offline(ctx) {
        return Err(Error::new(ErrorKind::Network, "offline", "--offline is set; cannot refresh the registry"));
    }
    let urls: Vec<String> = match a.str("url").or_else(|| std::env::var("FERRAUS_REGISTRY_URL").ok()) {
        Some(u) => vec![u],
        None => DEFAULT_REGISTRY_URLS.iter().map(|s| s.to_string()).collect(),
    };
    let mut last_err = None;
    for url in &urls {
        let fetched = get_bytes(url, 16 << 20).and_then(|body| {
            let sig = get_bytes(&format!("{url}.sig"), 4096)?;
            let sig = String::from_utf8_lossy(&sig).to_string();
            let m = parse_signed(&key, &body, &sig)?;
            Ok((body, sig, m))
        });
        match fetched {
            Ok((body, sig, m)) => {
                let embedded = Manifest::embedded();
                let newer: Vec<String> = m
                    .models
                    .iter()
                    .filter(|e| {
                        let known = embedded.versions_of(&e.name);
                        !known.is_empty() && known.iter().all(|k| k.version < e.version)
                    })
                    .map(|e| e.id())
                    .collect();
                let added: Vec<String> =
                    m.models.iter().filter(|e| embedded.versions_of(&e.name).is_empty()).map(|e| e.id()).collect();
                if !ctx.globals.dry_run {
                    let dest = updated_manifest_path(&c);
                    std::fs::create_dir_all(&c.root)?;
                    std::fs::write(&dest, &body)?;
                    std::fs::write(sig_path(&dest), sig.trim())?;
                }
                if !newer.is_empty() {
                    ctx.warn(
                        "newer_models_available",
                        format!(
                            "newer versions are available ({}); tiers keep their pinned versions — use --model name@version to switch",
                            newer.join(", ")
                        ),
                    );
                }
                return Ok(json!({"url": url, "models": m.models.len(), "newer_versions": newer, "new_models": added}));
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| Error::new(ErrorKind::Network, "network_error", "no registry URL")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_are_well_formed() {
        let ops = ops();
        let names: Vec<&str> = ops.iter().map(|o| o.name).collect();
        for n in [
            "models list",
            "models pull",
            "models rm",
            "models prune",
            "models verify",
            "models info",
            "models path",
            "models registry update",
        ] {
            assert!(names.contains(&n), "{n}");
        }
        for o in &ops {
            assert_eq!(o.category, Category::Models);
            if let Some(p) = o.positional {
                assert!(o.find_param(p).is_some(), "{} positional {p}", o.name);
            }
        }
    }

    #[test]
    fn tier_models_for_separate() {
        let m = Manifest::embedded();
        let all = tier_models(&m, "separate", None);
        assert!(!all.is_empty());
        let fast = tier_models(&m, "separate", Some("fast"));
        assert!(fast.iter().any(|r| r.starts_with("separate/htdemucs@")));
        assert!(fast.len() <= all.len());
    }
}
