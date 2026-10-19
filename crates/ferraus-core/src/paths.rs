//! Per-platform config, cache, and data directories. `FERRAUS_HOME` overrides cache and
//! data; `FERRAUS_CONFIG` overrides the config file.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Directory holding `config.toml` and user presets.
pub fn config_dir() -> PathBuf {
    if let Some(c) = env_path("FERRAUS_CONFIG") {
        if let Some(p) = c.parent() {
            return p.to_path_buf();
        }
    }
    if let Some(h) = env_path("FERRAUS_HOME") {
        return h.join("config");
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/ferraus")
    } else if cfg!(windows) {
        env_path("APPDATA").unwrap_or_else(home).join("ferraus")
    } else {
        env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")).join("ferraus")
    }
}

pub fn config_file() -> PathBuf {
    env_path("FERRAUS_CONFIG").unwrap_or_else(|| config_dir().join("config.toml"))
}

/// Cache: models, execution providers, plugin scans, pipeline stage cache.
pub fn cache_dir() -> PathBuf {
    if let Some(h) = env_path("FERRAUS_HOME") {
        return h.join("cache");
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Caches/ferraus")
    } else if cfg!(windows) {
        env_path("LOCALAPPDATA").unwrap_or_else(home).join("ferraus").join("cache")
    } else {
        env_path("XDG_CACHE_HOME").unwrap_or_else(|| home().join(".cache")).join("ferraus")
    }
}

/// Data: installed sounds, job records.
pub fn data_dir() -> PathBuf {
    if let Some(h) = env_path("FERRAUS_HOME") {
        return h.join("data");
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/ferraus/data")
    } else if cfg!(windows) {
        env_path("LOCALAPPDATA").unwrap_or_else(home).join("ferraus").join("data")
    } else {
        env_path("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share")).join("ferraus")
    }
}

pub fn models_dir() -> PathBuf {
    cache_dir().join("models")
}

pub fn sounds_dir() -> PathBuf {
    data_dir().join("sounds")
}

pub fn jobs_dir() -> PathBuf {
    data_dir().join("jobs")
}

pub fn presets_dir() -> PathBuf {
    config_dir().join("presets")
}

pub fn pipeline_cache_dir() -> PathBuf {
    cache_dir().join("pipeline")
}

pub fn tools_dir() -> PathBuf {
    cache_dir().join("tools")
}
