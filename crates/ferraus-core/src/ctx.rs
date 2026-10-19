//! The execution context handed to every op: global flags, input/output helpers, and
//! the collectors for warnings, metrics, models, and resolved parameter notes.

use crate::buffer::AudioBuffer;
use crate::error::{Error, ErrorKind, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Information about a decoded source.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SourceInfo {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    pub duration_s: f64,
    pub sample_rate: u32,
    pub channels: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tags: std::collections::BTreeMap<String, String>,
}

/// Encoding options for an output.
#[derive(Clone, Debug, Default, Serialize)]
pub struct EncodeOptions {
    /// Container/codec name (`wav`, `flac`, `mp3`, `ogg`, `opus`, `m4a`, `aiff`). Inferred
    /// from the path extension when `None`.
    pub format: Option<String>,
    pub bit_depth: Option<u16>,
    /// Write 32-bit float WAV.
    pub float: bool,
    pub bitrate: Option<u32>,
    pub vbr: bool,
    /// `triangular` (default when reducing bit depth), `shaped`, `none`.
    pub dither: Option<String>,
    pub seed: u64,
    pub tags: std::collections::BTreeMap<String, String>,
}

/// Decoding and encoding, implemented by `ferraus-codec` and injected by the CLI.
pub trait AudioIo: Send + Sync {
    fn read(&self, path: &Path) -> Result<(AudioBuffer, SourceInfo)>;
    /// Read from bytes (stdin) with a format hint.
    fn read_bytes(&self, bytes: &[u8], format_hint: Option<&str>) -> Result<(AudioBuffer, SourceInfo)>;
    fn probe(&self, path: &Path) -> Result<SourceInfo>;
    fn write(&self, path: &Path, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<()>;
    fn write_bytes(&self, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<Vec<u8>>;
    /// Extensions this build can encode.
    fn can_encode(&self, ext: &str) -> bool;
}

/// Progress sink (stderr bar, NDJSON events, or nothing).
pub trait Progress: Send + Sync {
    fn stage(&self, _stage: &str, _index: usize, _of: usize) {}
    fn progress(&self, _stage: &str, _fraction: f64) {}
    fn output(&self, _path: &str, _role: Option<&str>) {}
    fn warning(&self, _code: &str, _message: &str) {}
    fn log(&self, _message: &str) {}
}

pub struct NoProgress;
impl Progress for NoProgress {}

/// Flags accepted by every command.
#[derive(Clone, Debug, Serialize)]
pub struct Globals {
    pub output: Option<String>,
    pub outdir: Option<PathBuf>,
    pub format: Option<String>,
    pub input_format: Option<String>,
    pub overwrite: bool,
    pub in_place: bool,
    pub sidecar: bool,
    pub dry_run: bool,
    pub explain: bool,
    pub force: bool,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u16>,
    pub channels: Option<usize>,
    pub bitrate: Option<u32>,
    pub seed: u64,
    pub threads: usize,
    pub device: String,
    pub deterministic: bool,
    pub offline: bool,
    pub yes: bool,
    pub bpm: Option<f64>,
    pub sound_paths: Vec<PathBuf>,
    pub max_memory: u64,
    /// When set, every read and write must stay inside one of these directories (MCP sandbox).
    pub allow: Vec<PathBuf>,
    pub config_path: Option<PathBuf>,
}

impl Default for Globals {
    fn default() -> Self {
        Globals {
            output: None,
            outdir: None,
            format: None,
            input_format: None,
            overwrite: false,
            in_place: false,
            sidecar: true,
            dry_run: false,
            explain: false,
            force: false,
            sample_rate: None,
            bit_depth: None,
            channels: None,
            bitrate: None,
            seed: 0,
            threads: 0,
            device: "auto".into(),
            deterministic: false,
            offline: false,
            yes: false,
            bpm: None,
            sound_paths: Vec::new(),
            max_memory: 2 * 1024 * 1024 * 1024,
            allow: Vec::new(),
            config_path: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OutputRecord {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<usize>,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WarningRecord {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ModelRecord {
    pub name: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputRecord {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<usize>,
}

/// Cancellation flag shared with signal handlers and job runners.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst)
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::cancelled("cancelled"))
        } else {
            Ok(())
        }
    }
}

/// Context for one op invocation.
pub struct OpCtx<'a> {
    /// The command path being run (e.g. `separate`).
    pub command: String,
    pub inputs: Vec<PathBuf>,
    pub globals: Globals,
    pub io: &'a dyn AudioIo,
    pub progress: &'a dyn Progress,
    pub cancel: CancelToken,
    /// Registry of every op, for ops that compose others (master, enhance, hum...).
    pub registry: Option<&'a crate::registry::Registry>,
    pub outputs: Vec<OutputRecord>,
    pub input_records: Vec<InputRecord>,
    pub warnings: Vec<WarningRecord>,
    pub metrics: Map<String, Value>,
    /// Parameter values resolved at run time (detected key, chosen noise region...).
    pub notes: Map<String, Value>,
    pub models: Vec<ModelRecord>,
    pub plugins: Vec<Value>,
    /// False when a stage may be nondeterministic (plugins with internal randomness).
    pub deterministic: Option<bool>,
    /// Stdin bytes (when an input is `-`).
    pub stdin: Option<Vec<u8>>,
    /// Bytes to write to stdout when `-o -`.
    pub stdout: Option<Vec<u8>>,
    /// Index of this item in a batch (for `{index}` in output templates).
    pub index: usize,
    /// Files created by this op that must be removed if the op fails.
    pub temp_files: Vec<PathBuf>,
    /// Result value set by a Transform op (custom ops return theirs directly).
    pub result: Option<Value>,
}

impl<'a> OpCtx<'a> {
    pub fn new(
        command: &str,
        inputs: Vec<PathBuf>,
        globals: Globals,
        io: &'a dyn AudioIo,
        progress: &'a dyn Progress,
    ) -> Self {
        OpCtx {
            command: command.to_string(),
            inputs,
            globals,
            io,
            progress,
            cancel: CancelToken::default(),
            registry: None,
            outputs: Vec::new(),
            input_records: Vec::new(),
            warnings: Vec::new(),
            metrics: Map::new(),
            notes: Map::new(),
            models: Vec::new(),
            plugins: Vec::new(),
            deterministic: None,
            stdin: None,
            stdout: None,
            index: 0,
            temp_files: Vec::new(),
            result: None,
        }
    }

    /// A child context for running a nested op (shares io, progress, globals, registry).
    pub fn child(&self, command: &str, inputs: Vec<PathBuf>) -> OpCtx<'a> {
        let mut c = OpCtx::new(command, inputs, self.globals.clone(), self.io, self.progress);
        c.cancel = self.cancel.clone();
        c.registry = self.registry;
        c
    }

    /// Merge a child's records into this context.
    pub fn absorb(&mut self, child: OpCtx<'a>) {
        self.warnings.extend(child.warnings);
        for m in child.models {
            if !self.models.contains(&m) {
                self.models.push(m);
            }
        }
        self.plugins.extend(child.plugins);
        if child.deterministic == Some(false) {
            self.deterministic = Some(false);
        }
    }

    pub fn bpm(&self) -> Option<f64> {
        self.globals.bpm
    }

    pub fn seed(&self) -> u64 {
        self.globals.seed
    }

    pub fn input(&self, i: usize) -> Result<&Path> {
        self.inputs
            .get(i)
            .map(|p| p.as_path())
            .ok_or_else(|| Error::usage(format!("'{}' needs at least {} input(s)", self.command, i + 1)))
    }

    /// Check that a path is inside the sandbox (when one is set).
    pub fn check_allowed(&self, path: &Path) -> Result<()> {
        if self.globals.allow.is_empty() {
            return Ok(());
        }
        let abs = absolutize(path);
        if self.globals.allow.iter().any(|a| abs.starts_with(absolutize(a))) {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::Usage,
                "path_not_allowed",
                format!("{} is outside the allowed directories", path.display()),
            )
            .with_hint("serve with --allow <dir> to permit it"))
        }
    }

    /// Decode input `i` (or stdin for `-`), applying `--channels`/`--sample-rate` only at write time.
    pub fn read_audio(&mut self, i: usize) -> Result<AudioBuffer> {
        let path = self.input(i)?.to_path_buf();
        self.read_audio_path(&path)
    }

    pub fn read_audio_path(&mut self, path: &Path) -> Result<AudioBuffer> {
        let (buf, info) = if path.as_os_str() == "-" {
            let bytes = self.stdin.take().ok_or_else(|| Error::input("stdin was already consumed or is empty"))?;
            let r = self.io.read_bytes(&bytes, self.globals.input_format.as_deref())?;
            self.stdin = Some(bytes);
            r
        } else {
            self.check_allowed(path)?;
            if !path.exists() {
                return Err(Error::new(
                    ErrorKind::InputMissing,
                    "input_not_found",
                    format!("input not found: {}", path.display()),
                ));
            }
            self.io.read(path)?
        };
        self.input_records.push(InputRecord {
            path: info.path.clone(),
            duration_s: Some(info.duration_s),
            sample_rate: Some(info.sample_rate),
            channels: Some(info.channels),
        });
        Ok(buf)
    }

    /// Record a non-audio input (MIDI, score, profile...).
    pub fn note_input(&mut self, path: &Path) {
        self.input_records.push(InputRecord {
            path: path.display().to_string(),
            duration_s: None,
            sample_rate: None,
            channels: None,
        });
    }

    /// Resolve the output path for this op.
    ///
    /// * `role` — e.g. `vocals` for a stem (fills `{stem}`); `None` for the main output.
    /// * `ext` — default extension when the user gave no `-o`.
    ///
    /// Honours `-o` templates (`{name}`, `{stem}`, `{index}`, `{ext}`, `{command}`), `--outdir`,
    /// `--in-place`, and the default `<input>.<command>[.<role>].<ext>` beside the input.
    pub fn output_path(&self, role: Option<&str>, ext: &str) -> Result<PathBuf> {
        let input = self.inputs.first().filter(|p| p.as_os_str() != "-");
        let name = input
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "ferraus".to_string());
        let in_ext = input.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase());
        let cmd = self.command.replace(' ', "-");
        let ext = if ext.is_empty() { in_ext.clone().unwrap_or_else(|| "wav".into()) } else { ext.to_string() };

        if self.globals.in_place && role.is_none() {
            if let Some(p) = input {
                return Ok(p.clone());
            }
        }
        let p = if let Some(o) = &self.globals.output {
            if o == "-" {
                return Ok(PathBuf::from("-"));
            }
            let has_template = o.contains('{');
            let mut s = o
                .replace("{name}", &name)
                .replace("{index}", &self.index.to_string())
                .replace("{command}", &cmd)
                .replace("{ext}", &ext);
            s = s.replace("{stem}", role.unwrap_or("out"));
            let mut p = PathBuf::from(s);
            if let (Some(role), false) = (role, has_template) {
                // several outputs from one -o: insert the role before the extension
                let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let e = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| ext.clone());
                p.set_file_name(format!("{stem}.{role}.{e}"));
            }
            if let Some(dir) = &self.globals.outdir {
                if p.is_relative() {
                    p = dir.join(p);
                }
            }
            p
        } else {
            let file = match role {
                Some(r) => format!("{name}.{r}.{ext}"),
                None => format!("{name}.{cmd}.{ext}"),
            };
            match (&self.globals.outdir, input.and_then(|p| p.parent())) {
                (Some(dir), _) => dir.join(file),
                (None, Some(parent)) => parent.join(file),
                (None, None) => PathBuf::from(file),
            }
        };
        Ok(p)
    }

    /// Check overwrite policy and sandbox for an output path, creating its directory.
    pub fn prepare_output(&self, path: &Path) -> Result<()> {
        if path.as_os_str() == "-" {
            return Ok(());
        }
        self.check_allowed(path)?;
        let is_input = self.inputs.iter().any(|i| i == path);
        if path.exists() && !self.globals.overwrite && !(self.globals.in_place && is_input) {
            return Err(Error::new(ErrorKind::Usage, "output_exists", format!("{} already exists", path.display()))
                .with_hint("pass --overwrite to replace it, or choose another -o path"));
        }
        if is_input && !self.globals.in_place {
            return Err(Error::usage(format!("output {} is the same as an input; use --in-place", path.display())));
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !self.globals.dry_run {
                std::fs::create_dir_all(parent)?;
            }
        }
        Ok(())
    }

    /// Encoding options from global flags for a given output path.
    pub fn encode_options(&self, path: &Path) -> EncodeOptions {
        let format = if path.as_os_str() == "-" {
            Some(self.globals.format.clone().unwrap_or_else(|| "wav".into()))
        } else {
            self.globals.format.clone().or_else(|| path.extension().map(|e| e.to_string_lossy().to_lowercase()))
        };
        EncodeOptions {
            format,
            bit_depth: self.globals.bit_depth,
            float: false,
            bitrate: self.globals.bitrate,
            vbr: true,
            dither: None,
            seed: self.globals.seed,
            tags: Default::default(),
        }
    }

    /// Write an audio output, applying `--sample-rate` and `--channels` conversions is the
    /// caller's job (the executor does it for transforms). Records the output.
    pub fn write_audio(&mut self, path: &Path, buf: &AudioBuffer, role: Option<&str>) -> Result<()> {
        let opts = self.encode_options(path);
        self.write_audio_with(path, buf, role, &opts)
    }

    pub fn write_audio_with(
        &mut self,
        path: &Path,
        buf: &AudioBuffer,
        role: Option<&str>,
        opts: &EncodeOptions,
    ) -> Result<()> {
        self.prepare_output(path)?;
        if !self.globals.dry_run {
            if path.as_os_str() == "-" {
                let bytes = self.io.write_bytes(buf, opts)?;
                self.stdout.get_or_insert_with(Vec::new).extend(bytes);
            } else {
                if self.globals.in_place && path.exists() {
                    let bak = PathBuf::from(format!("{}.bak", path.display()));
                    std::fs::copy(path, &bak)?;
                }
                // write to a temp file then rename, so a failed encode never leaves a partial file
                let tmp = tmp_path(path);
                self.io.write(&tmp, buf, opts).inspect_err(|_| {
                    let _ = std::fs::remove_file(&tmp);
                })?;
                std::fs::rename(&tmp, path)?;
            }
        }
        self.outputs.push(OutputRecord {
            path: path.display().to_string(),
            role: role.map(|r| r.to_string()),
            duration_s: Some(buf.duration_s()),
            sample_rate: Some(buf.sample_rate),
            channels: Some(buf.channels()),
            extra: Map::new(),
        });
        self.progress.output(&path.display().to_string(), role);
        Ok(())
    }

    /// Write a non-audio file (JSON, MIDI, text...) and record it.
    pub fn write_file(&mut self, path: &Path, bytes: &[u8], role: Option<&str>) -> Result<()> {
        self.prepare_output(path)?;
        if !self.globals.dry_run {
            if path.as_os_str() == "-" {
                self.stdout.get_or_insert_with(Vec::new).extend_from_slice(bytes);
            } else {
                let tmp = tmp_path(path);
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, path)?;
            }
        }
        self.record_output(path, role);
        Ok(())
    }

    /// Record an output that was written by other means (e.g. a helper process).
    pub fn record_output(&mut self, path: &Path, role: Option<&str>) {
        self.outputs.push(OutputRecord {
            path: path.display().to_string(),
            role: role.map(|r| r.to_string()),
            duration_s: None,
            sample_rate: None,
            channels: None,
            extra: Map::new(),
        });
        self.progress.output(&path.display().to_string(), role);
    }

    /// Set the envelope `result` from a Transform op.
    pub fn set_result(&mut self, value: impl Serialize) {
        self.result = Some(serde_json::to_value(value).unwrap_or(Value::Null));
    }

    pub fn warn(&mut self, code: &str, message: impl Into<String>) {
        let message = message.into();
        self.progress.warning(code, &message);
        self.warnings.push(WarningRecord { code: code.to_string(), message });
    }

    pub fn metric(&mut self, key: &str, value: impl Serialize) {
        self.metrics.insert(key.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
    }

    /// Record a parameter resolved at run time (shown in params and the sidecar).
    pub fn note(&mut self, key: &str, value: impl Serialize) {
        self.notes.insert(key.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
    }

    pub fn model_used(&mut self, name: &str, version: &str, sha256: &str) {
        let r = ModelRecord { name: name.into(), version: version.into(), sha256: sha256.into() };
        if !self.models.contains(&r) {
            self.models.push(r);
        }
    }

    pub fn set_progress(&self, fraction: f64) {
        self.progress.progress(&self.command, fraction.clamp(0.0, 1.0));
    }

    pub fn check_cancel(&self) -> Result<()> {
        self.cancel.check()
    }

    /// Thread count to use (0 = all cores).
    pub fn threads(&self) -> usize {
        if self.globals.threads == 0 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        } else {
            self.globals.threads
        }
    }
}

pub fn tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    // keep the real extension last so encoders that infer format still work
    path.with_file_name(format!(".{name}.{}.part.{ext}", std::process::id()))
}

pub fn absolutize(p: &Path) -> PathBuf {
    let expanded = if let Ok(s) = p.strip_prefix("~") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(s)).unwrap_or_else(|| p.to_path_buf())
    } else {
        p.to_path_buf()
    };
    let abs =
        if expanded.is_absolute() { expanded } else { std::env::current_dir().unwrap_or_default().join(expanded) };
    // lexical normalization (no symlink resolution so non-existent outputs work)
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}
