//! Cache layout: `<cache>/ferraus/models/<task>/<name>/<version>/model.onnx` plus
//! `spec.json`, and `manifest.lock` recording exactly which versions are present (so
//! `--offline` runs are fully described) and when each was last used (LRU pruning).

use crate::manifest::Entry;
use ferraus_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_MAX_CACHE_GB: f64 = 20.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    pub task: String,
    pub sha256: String,
    pub size_bytes: u64,
    /// Path of the model file relative to the cache root.
    pub path: String,
    pub installed_at: u64,
    pub last_used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Lock {
    #[serde(default)]
    pub models: Vec<LockEntry>,
    /// `name@version` of research-only models whose license the user accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_licenses: Vec<String>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Cache budget from `FERRAUS_MAX_CACHE_GB` (set by the CLI from `models.max_cache_gb`).
pub fn max_cache_bytes() -> u64 {
    let gb = std::env::var("FERRAUS_MAX_CACHE_GB")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(DEFAULT_MAX_CACHE_GB);
    (gb * 1e9) as u64
}

/// A model cache rooted at a directory (normally `ferraus_core::paths::models_dir()`).
#[derive(Clone, Debug)]
pub struct Cache {
    pub root: PathBuf,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Cache {
        Cache { root: root.into() }
    }

    pub fn default_root() -> Cache {
        Cache::new(ferraus_core::paths::models_dir())
    }

    pub fn model_dir(&self, e: &Entry) -> PathBuf {
        self.root.join(sanitize(&e.task)).join(sanitize(e.short_name())).join(sanitize(&e.version))
    }

    pub fn model_path(&self, e: &Entry) -> PathBuf {
        self.model_dir(e).join(e.file_name())
    }

    pub fn spec_path(&self, e: &Entry) -> PathBuf {
        self.model_dir(e).join("spec.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join("manifest.lock")
    }

    pub fn load_lock(&self) -> Lock {
        std::fs::read(self.lock_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save_lock(&self, lock: &Lock) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let tmp = self.root.join(format!(".manifest.lock.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(lock)?)?;
        std::fs::rename(&tmp, self.lock_path())?;
        Ok(())
    }

    /// The lock record for an entry when its file is present.
    pub fn cached(&self, e: &Entry) -> Option<LockEntry> {
        let lock = self.load_lock();
        let rec = lock.models.into_iter().find(|m| m.name == e.name && m.version == e.version)?;
        let path = self.root.join(&rec.path);
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() != rec.size_bytes {
            return None;
        }
        if let Some(want) = &e.sha256 {
            if !want.eq_ignore_ascii_case(&rec.sha256) {
                return None;
            }
        }
        Some(rec)
    }

    /// Record a freshly installed model.
    pub fn record(&self, e: &Entry, sha256: &str, size: u64, url: Option<&str>) -> Result<LockEntry> {
        let mut lock = self.load_lock();
        lock.models.retain(|m| !(m.name == e.name && m.version == e.version));
        let rel = self
            .model_path(e)
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| self.model_path(e).display().to_string());
        let t = now();
        let rec = LockEntry {
            name: e.name.clone(),
            version: e.version.clone(),
            task: e.task.clone(),
            sha256: sha256.to_string(),
            size_bytes: size,
            path: rel,
            installed_at: t,
            last_used: t,
            source_url: url.map(|s| s.to_string()),
        };
        lock.models.push(rec.clone());
        lock.models.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
        self.save_lock(&lock)?;
        Ok(rec)
    }

    pub fn touch(&self, name: &str, version: &str) {
        let mut lock = self.load_lock();
        let mut changed = false;
        for m in &mut lock.models {
            if m.name == name && m.version == version {
                m.last_used = now();
                changed = true;
            }
        }
        if changed {
            let _ = self.save_lock(&lock);
        }
    }

    pub fn accept_license(&self, id: &str) -> Result<()> {
        let mut lock = self.load_lock();
        if !lock.accepted_licenses.iter().any(|a| a == id) {
            lock.accepted_licenses.push(id.to_string());
            self.save_lock(&lock)?;
        }
        Ok(())
    }

    pub fn license_accepted(&self, id: &str) -> bool {
        self.load_lock().accepted_licenses.iter().any(|a| a == id)
    }

    /// Remove one cached model (file, spec, directory) and its lock record.
    pub fn remove(&self, name: &str, version: &str) -> Result<Option<LockEntry>> {
        let mut lock = self.load_lock();
        let pos = lock.models.iter().position(|m| m.name == name && m.version == version);
        let Some(pos) = pos else { return Ok(None) };
        let rec = lock.models.remove(pos);
        let file = self.root.join(&rec.path);
        if let Some(dir) = file.parent() {
            // only ever delete inside the cache root
            if dir.starts_with(&self.root) && dir != self.root {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        self.save_lock(&lock)?;
        Ok(Some(rec))
    }

    pub fn total_bytes(&self) -> u64 {
        self.load_lock().models.iter().map(|m| m.size_bytes).sum()
    }

    /// Least-recently-used pruning down to `budget` bytes, never removing `keep`
    /// (`name@version` ids). Returns the removed records.
    pub fn prune_lru(&self, budget: u64, keep: &[String]) -> Result<Vec<LockEntry>> {
        let mut removed = Vec::new();
        loop {
            let lock = self.load_lock();
            let total: u64 = lock.models.iter().map(|m| m.size_bytes).sum();
            if total <= budget {
                break;
            }
            let victim = lock
                .models
                .iter()
                .filter(|m| !keep.contains(&format!("{}@{}", m.name, m.version)))
                .min_by_key(|m| m.last_used)
                .cloned();
            let Some(v) = victim else { break };
            self.remove(&v.name, &v.version)?;
            removed.push(v);
        }
        Ok(removed)
    }

    /// Delete stale `*.part` downloads.
    pub fn clean_partials(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(&self.root, &mut |p| {
            if p.extension().map(|e| e == "part").unwrap_or(false) && std::fs::remove_file(p).is_ok() {
                out.push(p.to_path_buf());
            }
        });
        out
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

/// Keep path components safe: no separators, no `..`.
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+') { c } else { '_' })
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        "_".into()
    } else {
        cleaned
    }
}

/// Make sure a path stays inside the cache root.
pub fn ensure_inside(root: &Path, p: &Path) -> Result<()> {
    if p.starts_with(root) {
        Ok(())
    } else {
        Err(Error::internal(format!("refusing to touch {} outside the model cache", p.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    #[test]
    fn layout_and_lru() {
        let dir = ferraus_core::testing::TempDir::new("cache-lru");
        let c = Cache::new(dir.0.clone());
        let m = Manifest::embedded();
        let mut e1 = m.resolve("separate/htdemucs").unwrap().clone();
        e1.sha256 = None;
        let p = c.model_path(&e1);
        assert!(p.ends_with(format!("separate/htdemucs/{}/model.onnx", e1.version)));
        let mut e2 = e1.clone();
        e2.version = "older".into();
        for (e, t) in [(&e1, 100u64), (&e2, 50u64)] {
            std::fs::create_dir_all(c.model_dir(e)).unwrap();
            std::fs::write(c.model_path(e), vec![0u8; 1000]).unwrap();
            c.record(e, "ab", 1000, None).unwrap();
            let mut lock = c.load_lock();
            for r in &mut lock.models {
                if r.version == e.version {
                    r.last_used = t;
                }
            }
            c.save_lock(&lock).unwrap();
        }
        assert!(c.cached(&e1).is_some());
        assert_eq!(c.total_bytes(), 2000);
        let removed = c.prune_lru(1500, &[]).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].version, "older", "least recently used goes first");
        assert!(!c.model_dir(&e2).exists());
        // keep list is respected
        let removed = c.prune_lru(0, &[e1.id()]).unwrap();
        assert!(removed.is_empty());
        assert!(c.cached(&e1).is_some());
    }

    #[test]
    fn sanitizes_components() {
        assert_eq!(sanitize("../x"), ".._x");
        assert_eq!(sanitize(".."), "_");
        assert_eq!(sanitize("v4"), "v4");
    }
}
