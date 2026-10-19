//! `config.toml`: every key optional. Values become defaults that command-line flags override.

use ferraus_core::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub output: OutputCfg,
    pub models: ModelsCfg,
    pub compute: ComputeCfg,
    pub plugins: PluginsCfg,
    pub sounds: SoundsCfg,
    pub presets: BTreeMap<String, toml::Table>,
    pub mcp: McpCfg,
    pub ffmpeg: FfmpegCfg,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OutputCfg {
    pub default_format: Option<String>,
    pub bit_depth: Option<u16>,
    pub sidecar: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ModelsCfg {
    pub auto_download: Option<bool>,
    pub max_cache_gb: Option<f64>,
    pub registry_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ComputeCfg {
    pub device: Option<String>,
    pub threads: Option<usize>,
    pub max_memory: Option<String>,
    pub deterministic: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PluginsCfg {
    pub extra_paths: Vec<PathBuf>,
    pub watchdog_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SoundsCfg {
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct McpCfg {
    pub allow: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FfmpegCfg {
    pub path: Option<PathBuf>,
}

impl Config {
    /// Load from an explicit path, `FERRAUS_CONFIG`, or the platform default. A missing
    /// default file is not an error; a missing explicit file is.
    pub fn load(explicit: Option<&Path>) -> Result<Config> {
        let path = match explicit {
            Some(p) => {
                if !p.exists() {
                    return Err(Error::input(format!(
                        "config file not found: {}",
                        p.display()
                    )));
                }
                p.to_path_buf()
            }
            None => {
                let p = ferraus_core::paths::config_file();
                if !p.exists() {
                    return Ok(Config::default());
                }
                p
            }
        };
        let text = std::fs::read_to_string(&path)?;
        toml::from_str(&text).map_err(|e| Error::usage(format!("{}: {e}", path.display())))
    }

    /// Per-effect default overrides from `[presets]`.
    pub fn preset_overrides(&self, command: &str) -> Vec<(String, String)> {
        self.presets
            .get(command)
            .map(|t| {
                t.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            match v {
                                toml::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prd_example() {
        let text = r#"
[output]
default_format = "wav"
bit_depth = 24
sidecar = true

[models]
auto_download = true
max_cache_gb = 20
registry_url = "https://github.com/djynnius/ferraus-models/releases/latest/download/registry.json"

[compute]
device = "auto"
threads = 0
max_memory = "2GB"
deterministic = false

[plugins]
extra_paths = []
watchdog_seconds = 60

[presets]
compress = { threshold = "-16dB", ratio = "3:1" }

[mcp]
allow = ["~/music"]
"#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.output.bit_depth, Some(24));
        assert_eq!(c.preset_overrides("compress").len(), 2);
        assert_eq!(c.mcp.allow.len(), 1);
    }
}
