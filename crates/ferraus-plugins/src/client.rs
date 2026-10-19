//! The ferraus side of the host protocol: spawns one host process per plugin instance,
//! sends framed JSON requests, moves audio through a memory-mapped temp file, and enforces
//! the watchdog. A crash or timeout becomes [`ErrorKind::Plugin`] (exit code 7) naming the
//! plugin and its path; the calling process is never affected.

use crate::protocol::*;
use base64::Engine;
use ferraus_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default watchdog: a host that makes no progress for this long is stopped.
pub const DEFAULT_WATCHDOG_S: f64 = 60.0;

/// The watchdog from `FERRAUS_PLUGIN_WATCHDOG_S`, else 60 s.
pub fn default_watchdog() -> Duration {
    std::env::var("FERRAUS_PLUGIN_WATCHDOG_S")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0 && v.is_finite())
        .map(Duration::from_secs_f64)
        .unwrap_or(Duration::from_secs_f64(DEFAULT_WATCHDOG_S))
}

/// Plugin binary format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum PluginFormat {
    Clap,
    Vst3,
}

impl PluginFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginFormat::Clap => "clap",
            PluginFormat::Vst3 => "vst3",
        }
    }

    pub fn parse(s: &str) -> Option<PluginFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "clap" => Some(PluginFormat::Clap),
            "vst3" | "vst" => Some(PluginFormat::Vst3),
            _ => None,
        }
    }

    /// Format from a bundle path's extension.
    pub fn from_path(p: &Path) -> Option<PluginFormat> {
        let ext = p.extension()?.to_string_lossy().to_ascii_lowercase();
        PluginFormat::parse(&ext)
    }

    /// The host executable for this format.
    pub fn host_program(self) -> &'static str {
        match self {
            PluginFormat::Clap => "ferraus-plughost",
            PluginFormat::Vst3 => "ferraus-plughost-vst3",
        }
    }
}

impl std::fmt::Display for PluginFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Locate the host executable: `FERRAUS_PLUGHOST` / `FERRAUS_PLUGHOST_VST3`, then next to
/// the running executable (and its parent, which covers `target/<profile>/deps` test
/// binaries), then `PATH`.
pub fn host_binary(format: PluginFormat) -> Result<PathBuf> {
    let var = match format {
        PluginFormat::Clap => "FERRAUS_PLUGHOST",
        PluginFormat::Vst3 => "FERRAUS_PLUGHOST_VST3",
    };
    if let Some(p) = std::env::var_os(var).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let file = format!("{}{}", format.host_program(), std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..2 {
            if let Some(d) = &dir {
                let c = d.join(&file);
                if c.is_file() {
                    return Ok(c);
                }
                dir = d.parent().map(|p| p.to_path_buf());
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let c = d.join(&file);
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    Err(Error::new(
        ErrorKind::Plugin,
        "plugin_host_missing",
        format!(
            "the {} plugin host '{}' was not found next to ferraus or on PATH",
            format,
            format.host_program()
        ),
    )
    .with_hint(format!(
        "reinstall ferraus, or set {var}=/path/to/{}",
        format.host_program()
    )))
}

static SHM_COUNTER: AtomicU64 = AtomicU64::new(0);

struct ShmFile {
    path: PathBuf,
    map: memmap2::MmapMut,
    layout: ShmLayout,
}

impl ShmFile {
    fn create(layout: ShmLayout) -> Result<ShmFile> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "ferraus-shm-{}-{}-{nanos}.bin",
            std::process::id(),
            SHM_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| Error::processing(format!("cannot create {}: {e}", path.display())))?;
        file.set_len(layout.total_bytes() as u64)?;
        // SAFETY: the file was just created by us and is only shared with our own host.
        let mut map = unsafe { memmap2::MmapMut::map_mut(&file) }
            .map_err(|e| Error::processing(format!("cannot map shared memory: {e}")))?;
        layout.write_header(&mut map);
        map.flush()?;
        Ok(ShmFile { path, map, layout })
    }
}

impl Drop for ShmFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What activation reported.
#[derive(Clone, Copy, Debug)]
pub struct Activated {
    pub latency: u32,
    pub tail: Option<u32>,
    pub channels_in: u32,
    pub channels_out: u32,
}

/// One running host process with one plugin.
pub struct PluginHost {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<std::io::Result<Vec<u8>>>,
    stderr_tail: Arc<Mutex<String>>,
    label: String,
    path: PathBuf,
    watchdog: Duration,
    shm: Option<ShmFile>,
    failed: Option<Error>,
}

impl PluginHost {
    /// Start a host for `format`; `label` and `path` name the plugin in errors.
    pub fn spawn(format: PluginFormat, label: &str, path: &Path, watchdog: Duration) -> Result<PluginHost> {
        let bin = host_binary(format)?;
        let mut child = Command::new(&bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                Error::new(
                    ErrorKind::Plugin,
                    "plugin_host_failed",
                    format!("cannot start {}: {e}", bin.display()),
                )
            })?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                match read_frame(&mut r) {
                    Ok(Some(f)) => {
                        if tx.send(Ok(f)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        let tail = Arc::new(Mutex::new(String::new()));
        let tail2 = tail.clone();
        let echo = std::env::var_os("FERRAUS_PLUGIN_LOG").is_some();
        std::thread::spawn(move || {
            let r = BufReader::new(stderr);
            for line in r.lines() {
                let Ok(line) = line else { break };
                if echo {
                    eprintln!("[plughost] {line}");
                }
                if let Ok(mut t) = tail2.lock() {
                    t.push_str(&line);
                    t.push('\n');
                    if t.len() > 4000 {
                        let cut = t.len() - 3000;
                        let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                        t.drain(..cut);
                    }
                }
            }
        });
        let stdin = child.stdin.take();
        let mut h = PluginHost {
            child,
            stdin,
            rx,
            stderr_tail: tail,
            label: label.to_string(),
            path: path.to_path_buf(),
            watchdog,
            shm: None,
            failed: None,
        };
        match h.request(&Request::Hello)? {
            Reply::Hello { protocol, .. } if protocol == PROTOCOL_VERSION => Ok(h),
            Reply::Hello { protocol, host, .. } => Err(Error::new(
                ErrorKind::Plugin,
                "plugin_host_mismatch",
                format!(
                    "{host} speaks protocol {protocol}, ferraus expects {PROTOCOL_VERSION}"
                ),
            )
            .with_hint("reinstall ferraus so the plugin host matches")),
            other => Err(h.unexpected(other)),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
    }

    fn who(&self) -> String {
        format!("plugin '{}' ({})", self.label, self.path.display())
    }

    fn stderr_excerpt(&self) -> String {
        let t = self.stderr_tail.lock().map(|t| t.clone()).unwrap_or_default();
        let lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            String::new()
        } else {
            let start = lines.len().saturating_sub(5);
            format!("; host said: {}", lines[start..].join(" | "))
        }
    }

    fn unexpected(&self, r: Reply) -> Error {
        Error::new(
            ErrorKind::Plugin,
            "plugin_protocol_error",
            format!("{}: unexpected reply from the host: {r:?}", self.who()),
        )
    }

    /// The host died: describe how.
    fn crashed(&mut self) -> Error {
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = self.child.kill();
                    break self.child.wait().ok();
                }
            }
        };
        let how = match status {
            Some(s) => describe_exit(s),
            None => "the host process vanished".into(),
        };
        let e = Error::new(
            ErrorKind::Plugin,
            "plugin_crashed",
            format!("{} crashed: {how}{}", self.who(), self.stderr_excerpt()),
        )
        .with_hint("the plugin failed inside its own process; ferraus is unaffected — try other parameters or report it to the plugin vendor");
        self.failed = Some(e.clone());
        e
    }

    fn timed_out(&mut self) -> Error {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let e = Error::new(
            ErrorKind::Plugin,
            "plugin_timeout",
            format!(
                "{} made no progress for {:.0} s and was stopped by the watchdog{}",
                self.who(),
                self.watchdog.as_secs_f64(),
                self.stderr_excerpt()
            ),
        )
        .with_hint("raise the limit with --watchdog 300s or FERRAUS_PLUGIN_WATCHDOG_S=300");
        self.failed = Some(e.clone());
        e
    }

    fn send(&mut self, req: &Request) -> Result<()> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.crashed());
        };
        if write_frame(stdin, req).is_err() {
            return Err(self.crashed());
        }
        Ok(())
    }

    fn recv(&mut self) -> Result<Reply> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        match self.rx.recv_timeout(self.watchdog) {
            Ok(Ok(bytes)) => {
                let r: Reply = serde_json::from_slice(&bytes).map_err(|e| {
                    Error::new(
                        ErrorKind::Plugin,
                        "plugin_protocol_error",
                        format!("{}: malformed reply: {e}", self.who()),
                    )
                })?;
                if let Reply::Error { code, message } = r {
                    return Err(self.reply_error(&code, &message));
                }
                Ok(r)
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => Err(self.crashed()),
            Err(RecvTimeoutError::Timeout) => Err(self.timed_out()),
        }
    }

    fn reply_error(&self, code: &str, message: &str) -> Error {
        let (kind, hint) = match code {
            "unknown_param" | "bad_value" => (
                ErrorKind::Usage,
                Some(format!("ferraus plugin info \"{}\"", self.label)),
            ),
            "unknown_preset" => (
                ErrorKind::Usage,
                Some(format!("ferraus plugin presets list \"{}\"", self.label)),
            ),
            "bad_state" | "bad_request" => (ErrorKind::Usage, None),
            _ => (ErrorKind::Plugin, None),
        };
        let e = Error::new(kind, code, format!("{}: {message}", self.who()));
        match hint {
            Some(h) => e.with_hint(h),
            None => e,
        }
    }

    /// Send one request and wait (under the watchdog) for its reply.
    pub fn request(&mut self, req: &Request) -> Result<Reply> {
        self.send(req)?;
        self.recv()
    }

    pub fn probe(&mut self, path: &Path) -> Result<Vec<Descriptor>> {
        match self.request(&Request::Probe {
            path: path.to_string_lossy().into_owned(),
        })? {
            Reply::Probe { plugins } => Ok(plugins),
            other => Err(self.unexpected(other)),
        }
    }

    pub fn load(&mut self, plugin_id: Option<&str>) -> Result<PluginInfo> {
        match self.request(&Request::Load {
            path: self.path.to_string_lossy().into_owned(),
            plugin_id: plugin_id.map(|s| s.to_string()),
        })? {
            Reply::Loaded { info } => Ok(info),
            other => Err(self.unexpected(other)),
        }
    }

    fn params_reply(&mut self, req: Request) -> Result<Vec<ParamValue>> {
        match self.request(&req)? {
            Reply::Params { params } => Ok(params),
            other => Err(self.unexpected(other)),
        }
    }

    pub fn set_params(&mut self, params: &[(String, String)]) -> Result<Vec<ParamValue>> {
        if params.is_empty() {
            return Ok(Vec::new());
        }
        let params = params
            .iter()
            .map(|(n, v)| ParamAssign {
                name: n.clone(),
                value: v.clone(),
            })
            .collect();
        self.params_reply(Request::SetParams { params })
    }

    pub fn get_params(&mut self) -> Result<Vec<ParamValue>> {
        self.params_reply(Request::GetParams)
    }

    pub fn load_preset(&mut self, preset: &str) -> Result<Vec<ParamValue>> {
        self.params_reply(Request::LoadPreset {
            preset: preset.to_string(),
        })
    }

    pub fn list_presets(&mut self) -> Result<Vec<PresetInfo>> {
        match self.request(&Request::ListPresets)? {
            Reply::Presets { presets } => Ok(presets),
            other => Err(self.unexpected(other)),
        }
    }

    pub fn get_state(&mut self) -> Result<Vec<u8>> {
        match self.request(&Request::GetState)? {
            Reply::State { state_b64 } => base64::engine::general_purpose::STANDARD
                .decode(state_b64)
                .map_err(|e| Error::plugin(format!("{}: state is not base64: {e}", self.who()))),
            other => Err(self.unexpected(other)),
        }
    }

    pub fn set_state(&mut self, state: &[u8]) -> Result<()> {
        let state_b64 = base64::engine::general_purpose::STANDARD.encode(state);
        match self.request(&Request::SetState { state_b64 })? {
            Reply::Ok => Ok(()),
            other => Err(self.unexpected(other)),
        }
    }

    /// Create the shared-memory ring and activate the plugin.
    pub fn activate(
        &mut self,
        sample_rate: u32,
        block_size: u32,
        channels_in: u32,
        channels_out: u32,
        slots: u32,
    ) -> Result<Activated> {
        let layout = ShmLayout {
            channels_in,
            channels_out,
            block_size,
            slots: slots.max(1),
        };
        let shm = ShmFile::create(layout)?;
        let req = Request::Activate {
            sample_rate: sample_rate as f64,
            block_size,
            shm_path: shm.path.to_string_lossy().into_owned(),
        };
        self.shm = Some(shm);
        match self.request(&req)? {
            Reply::Activated {
                latency,
                tail,
                channels_in,
                channels_out,
            } => Ok(Activated {
                latency,
                tail,
                channels_in,
                channels_out,
            }),
            other => Err(self.unexpected(other)),
        }
    }

    /// Stream `total` frames through the plugin. `input` (planar, may be shorter than
    /// `total` — the rest is silence) must match the shared-memory input channel count.
    /// `events` are `(absolute frame, event)` pairs; their `time` fields are rewritten.
    /// `progress` is called after each block with the fraction done and may abort.
    pub fn run(
        &mut self,
        input: &[Vec<f32>],
        total: usize,
        events: &[(u64, Event)],
        progress: &mut dyn FnMut(f64) -> Result<()>,
    ) -> Result<Vec<Vec<f32>>> {
        let layout = self
            .shm
            .as_ref()
            .map(|s| s.layout)
            .ok_or_else(|| Error::internal("run before activate"))?;
        let bs = layout.block_size as usize;
        let blocks = total.div_ceil(bs);
        let mut out = vec![vec![0.0f32; total]; layout.channels_out as usize];
        let mut events: Vec<(u64, Event)> = events.to_vec();
        events.sort_by_key(|e| e.0);
        let mut ev_i = 0usize;
        let (mut sent, mut done) = (0usize, 0usize);
        while done < blocks {
            while sent < blocks && sent - done < layout.slots as usize {
                let start = sent * bs;
                let n = bs.min(total - start);
                let slot = (sent % layout.slots as usize) as u32;
                {
                    let shm = self.shm.as_mut().unwrap();
                    let base = shm.map.as_mut_ptr();
                    for c in 0..layout.channels_in {
                        // SAFETY: offsets from our own layout; the host is not using this
                        // slot (at most `slots` blocks are in flight).
                        let dst = unsafe { f32_slice_mut(base, layout.input_offset(slot, c), n) };
                        dst.fill(0.0);
                        if let Some(src) = input.get(c as usize) {
                            if start < src.len() {
                                let m = n.min(src.len() - start);
                                dst[..m].copy_from_slice(&src[start..start + m]);
                            }
                        }
                    }
                }
                let mut evs = Vec::new();
                while ev_i < events.len() && (events[ev_i].0 as usize) < start + n {
                    let (frame, e) = events[ev_i];
                    let t = (frame as usize).saturating_sub(start) as u32;
                    evs.push(match e {
                        Event::NoteOn {
                            channel,
                            key,
                            velocity,
                            ..
                        } => Event::NoteOn {
                            time: t,
                            channel,
                            key,
                            velocity,
                        },
                        Event::NoteOff {
                            channel,
                            key,
                            velocity,
                            ..
                        } => Event::NoteOff {
                            time: t,
                            channel,
                            key,
                            velocity,
                        },
                        Event::Midi { data, .. } => Event::Midi { time: t, data },
                    });
                    ev_i += 1;
                }
                std::sync::atomic::fence(Ordering::SeqCst);
                self.send(&Request::Process {
                    slot,
                    frames: n as u32,
                    events: evs,
                })?;
                sent += 1;
            }
            match self.recv()? {
                Reply::Processed { slot } => {
                    let expect = (done % layout.slots as usize) as u32;
                    if slot != expect {
                        return Err(Error::plugin(format!(
                            "{}: host answered slot {slot}, expected {expect}",
                            self.who()
                        )));
                    }
                    std::sync::atomic::fence(Ordering::SeqCst);
                    let start = done * bs;
                    let n = bs.min(total - start);
                    let shm = self.shm.as_mut().unwrap();
                    let base = shm.map.as_mut_ptr();
                    for (c, ch) in out.iter_mut().enumerate() {
                        let src = unsafe { f32_slice_mut(base, layout.output_offset(slot, c as u32), n) };
                        ch[start..start + n].copy_from_slice(src);
                    }
                    done += 1;
                    progress(done as f64 / blocks as f64)?;
                }
                other => return Err(self.unexpected(other)),
            }
        }
        Ok(out)
    }

    /// Ask the host to exit and reap it.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if self.failed.is_none() {
            if let Some(mut s) = self.stdin.take() {
                let _ = write_frame(&mut s, &Request::Quit);
                let _ = s.flush();
            }
        }
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        self.shm = None;
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn describe_exit(s: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            let name = match sig {
                4 => " (SIGILL)",
                6 => " (SIGABRT)",
                7 | 10 => " (SIGBUS)",
                8 => " (SIGFPE)",
                9 => " (SIGKILL)",
                11 => " (SIGSEGV)",
                _ => "",
            };
            return format!("the host process was killed by signal {sig}{name}");
        }
    }
    match s.code() {
        Some(c) => format!("the host process exited with code {c}"),
        None => "the host process ended abnormally".into(),
    }
}
