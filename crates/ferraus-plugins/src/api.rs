//! Public entry points for running plugins from other crates (`ferraus-synth` renders
//! `InstrumentSource::Plugin` through [`render_instrument`]; effect chains use
//! [`process_effect`]). Every call starts its own host process, so a misbehaving plugin
//! surfaces as an [`ErrorKind::Plugin`] error and never takes the caller down.

pub use crate::client::PluginFormat;
use crate::client::{default_watchdog, PluginHost};
use crate::protocol::{Event, ParamValue, PluginInfo};
use base64::Engine;
use ferraus_core::{AudioBuffer, Error, ErrorKind, OpCtx, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A specific plugin: bundle path plus the plugin id inside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginRef {
    pub format: PluginFormat,
    /// The `.clap` / `.vst3` bundle (file or directory).
    pub path: PathBuf,
    /// Plugin id inside the bundle (CLAP id or VST3 class id). `None` = the only one.
    pub plugin_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
}

impl PluginRef {
    /// A plugin given directly by path (format from the extension, CLAP by default).
    pub fn from_path(path: impl Into<PathBuf>, plugin_id: Option<&str>) -> PluginRef {
        let path = path.into();
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "plugin".into());
        PluginRef {
            format: PluginFormat::from_path(&path).unwrap_or(PluginFormat::Clap),
            path,
            plugin_id: plugin_id.map(|s| s.to_string()),
            name,
            vendor: String::new(),
            version: String::new(),
        }
    }

    /// Resolve a name, id, or path (`"Surge XT"`, `org.surge-synth-team.surge-xt`,
    /// `plugin:"Surge XT"`, `/path/Foo.clap`, `/path/Foo.clap#plugin.id`) against the scan
    /// cache, the standard plugin folders, and `sound_paths`.
    pub fn resolve(query: &str, sound_paths: &[PathBuf]) -> Result<PluginRef> {
        crate::scan::resolve_plugin(query, sound_paths, default_watchdog())
    }

    fn label(&self) -> &str {
        &self.name
    }
}

/// One MIDI event at an absolute sample frame of the render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiEvent {
    pub frame: u64,
    pub kind: MidiEventKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MidiEventKind {
    /// `velocity` in 0..=1.
    NoteOn {
        channel: u8,
        key: u8,
        velocity: f32,
    },
    NoteOff {
        channel: u8,
        key: u8,
        velocity: f32,
    },
    /// Any other 3-byte MIDI message (CC, pitch bend, program change...).
    Raw([u8; 3]),
}

impl MidiEvent {
    pub fn note_on(frame: u64, channel: u8, key: u8, velocity: f32) -> MidiEvent {
        MidiEvent { frame, kind: MidiEventKind::NoteOn { channel, key, velocity } }
    }

    pub fn note_off(frame: u64, channel: u8, key: u8) -> MidiEvent {
        MidiEvent { frame, kind: MidiEventKind::NoteOff { channel, key, velocity: 0.0 } }
    }

    fn to_wire(self) -> (u64, Event) {
        let e = match self.kind {
            MidiEventKind::NoteOn { channel, key, velocity } => Event::NoteOn { time: 0, channel, key, velocity },
            MidiEventKind::NoteOff { channel, key, velocity } => Event::NoteOff { time: 0, channel, key, velocity },
            MidiEventKind::Raw(data) => Event::Midi { time: 0, data },
        };
        (self.frame, e)
    }
}

/// How long to keep processing after the input (or the render duration) ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tail {
    /// The plugin's reported tail (capped at 30 s). Effects that report nothing or an
    /// infinite tail get 2 s; instruments that report nothing get none.
    Auto,
    Seconds(f64),
}

/// Everything that configures one plugin run.
#[derive(Clone, Debug)]
pub struct PluginOptions {
    /// `(name or id, value)`: values in display units (`2.5kHz`, `-6dB`, `30%`), plain
    /// numbers, or `norm:0.3`.
    pub params: Vec<(String, String)>,
    /// Preset name or load key (applied after `state`, before `params`).
    pub preset: Option<String>,
    /// Native state blob (applied first).
    pub state: Option<Vec<u8>>,
    pub block_size: u32,
    pub tail: Tail,
    pub watchdog: Duration,
    /// Blocks in flight in the shared-memory ring.
    pub slots: u32,
    /// Called with the fraction done after each block; returning an error aborts.
    pub progress: Option<fn(f64)>,
}

impl Default for PluginOptions {
    fn default() -> Self {
        PluginOptions {
            params: Vec::new(),
            preset: None,
            state: None,
            block_size: 512,
            tail: Tail::Auto,
            watchdog: default_watchdog(),
            slots: 4,
            progress: None,
        }
    }
}

/// The result of a run: audio plus everything needed to record and reproduce it.
#[derive(Clone, Debug)]
pub struct PluginRun {
    pub audio: AudioBuffer,
    pub info: PluginInfo,
    /// Values of the parameters that were set explicitly (after conversion).
    pub params: Vec<ParamValue>,
    /// Native state captured after state/preset/params were applied, before processing.
    pub state: Option<Vec<u8>>,
    pub latency: u32,
    pub tail_frames: usize,
    pub preset: Option<String>,
}

impl PluginRun {
    /// The sidecar record for this plugin stage.
    pub fn record(&self, plugin: &PluginRef) -> Value {
        let params: serde_json::Map<String, Value> = self
            .params
            .iter()
            .map(|p| (p.name.clone(), json!({"id": p.id, "value": p.value, "text": p.text})))
            .collect();
        json!({
            "name": if self.info.descriptor.name.is_empty() { plugin.name.clone() } else { self.info.descriptor.name.clone() },
            "id": self.info.descriptor.id,
            "path": plugin.path.display().to_string(),
            "format": plugin.format.as_str(),
            "version": self.info.descriptor.version,
            "vendor": self.info.descriptor.vendor,
            "params": params,
            "preset": self.preset,
            "state_b64": self.state.as_ref().map(|s| base64::engine::general_purpose::STANDARD.encode(s)),
            "latency_samples": self.latency,
            "tail_samples": self.tail_frames,
        })
    }

    /// Record this stage in an op context: adds the sidecar entry and marks the output's
    /// determinism as unknown.
    pub fn record_in(&self, plugin: &PluginRef, ctx: &mut OpCtx) {
        ctx.plugins.push(self.record(plugin));
        ctx.deterministic = Some(false);
    }
}

/// A loaded plugin in its own host process, for inspection (`plugin info`, presets,
/// state) or for driving runs by hand.
pub struct PluginSession {
    pub host: PluginHost,
    pub info: PluginInfo,
    pub plugin: PluginRef,
}

impl PluginSession {
    pub fn open(plugin: &PluginRef, watchdog: Duration) -> Result<PluginSession> {
        let mut host = PluginHost::spawn(plugin.format, plugin.label(), &plugin.path, watchdog)?;
        let info = host.load(plugin.plugin_id.as_deref())?;
        if !info.descriptor.name.is_empty() {
            host.set_label(&info.descriptor.name);
        }
        Ok(PluginSession { host, info, plugin: plugin.clone() })
    }

    /// Apply state, then preset, then params; returns the explicit params' values.
    pub fn configure(&mut self, opts: &PluginOptions) -> Result<Vec<ParamValue>> {
        if let Some(s) = &opts.state {
            self.host.set_state(s)?;
        }
        if let Some(p) = &opts.preset {
            self.host.load_preset(p)?;
        }
        self.host.set_params(&opts.params)
    }

    fn capture_state(&mut self) -> Result<Option<Vec<u8>>> {
        if self.info.has_state {
            Ok(Some(self.host.get_state()?))
        } else {
            Ok(None)
        }
    }
}

fn tail_frames(tail: Tail, reported: Option<u32>, sr: u32, instrument: bool) -> usize {
    let secs = match tail {
        Tail::Seconds(s) => s.max(0.0),
        Tail::Auto => match reported {
            Some(t) if t != u32::MAX => (t as f64 / sr as f64).min(30.0),
            Some(_) => 2.0,
            None if instrument => 0.0,
            None => 2.0,
        },
    };
    (secs * sr as f64).round() as usize
}

fn check_sr(sr: u32) -> Result<()> {
    if !(8000..=768_000).contains(&sr) {
        return Err(Error::usage(format!("sample rate {sr} Hz is out of range")));
    }
    Ok(())
}

fn validate_block(opts: &PluginOptions) -> Result<()> {
    if !(16..=16384).contains(&opts.block_size) {
        return Err(Error::usage(format!("block size {} is out of range (16..16384)", opts.block_size)));
    }
    Ok(())
}

/// Drive an instrument plugin with MIDI and return `duration` seconds of audio (plus the
/// tail, see [`Tail`]), latency-compensated so events sound at their frame.
pub fn render_instrument_with(
    plugin: &PluginRef,
    midi_events: &[MidiEvent],
    sample_rate: u32,
    duration: f64,
    opts: &PluginOptions,
) -> Result<PluginRun> {
    check_sr(sample_rate)?;
    validate_block(opts)?;
    if !(duration.is_finite() && duration >= 0.0) {
        return Err(Error::usage("render duration must be a positive number of seconds"));
    }
    let mut s = PluginSession::open(plugin, opts.watchdog)?;
    let params = s.configure(opts)?;
    let state = s.capture_state()?;
    let (ci, co) = (s.info.main_in_channels, s.info.main_out_channels);
    if co == 0 {
        return Err(Error::new(
            ErrorKind::Plugin,
            "plugin_no_output",
            format!("plugin '{}' has no audio output", s.plugin.name),
        ));
    }
    let act = s.host.activate(sample_rate, opts.block_size, ci, co, opts.slots)?;
    let latency = act.latency as usize;
    let dur = (duration * sample_rate as f64).round() as usize;
    let tail = tail_frames(opts.tail, act.tail, sample_rate, true);
    let total = dur + tail + latency;
    let events: Vec<(u64, Event)> = midi_events.iter().map(|e| e.to_wire()).collect();
    let progress = opts.progress;
    let out = s.host.run(&[], total, &events, &mut |f| {
        if let Some(p) = progress {
            p(f)
        }
        Ok(())
    })?;
    let data: Vec<Vec<f32>> = out.into_iter().map(|c| c[latency..].to_vec()).collect();
    let info = s.info.clone();
    s.host.shutdown();
    Ok(PluginRun {
        audio: AudioBuffer::from_planar(sample_rate, data),
        info,
        params,
        state,
        latency: act.latency,
        tail_frames: tail,
        preset: opts.preset.clone(),
    })
}

/// Render an instrument plugin. `params` are `(name or id, value)` pairs; `state` is a
/// native state blob (e.g. from `plugin state save`). See [`render_instrument_with`].
pub fn render_instrument(
    plugin: &PluginRef,
    midi_events: &[MidiEvent],
    sample_rate: u32,
    duration: f64,
    params: &[(String, String)],
    state: Option<&[u8]>,
) -> Result<AudioBuffer> {
    let opts = PluginOptions { params: params.to_vec(), state: state.map(|s| s.to_vec()), ..Default::default() };
    Ok(render_instrument_with(plugin, midi_events, sample_rate, duration, &opts)?.audio)
}

/// Run audio through an effect plugin. The output is `input` length plus the tail, with
/// the plugin's latency removed so every sample stays at its original position.
pub fn process_effect_with(plugin: &PluginRef, input: &AudioBuffer, opts: &PluginOptions) -> Result<PluginRun> {
    process_effect_inner(plugin, input, opts, &mut |_| Ok(()))
}

pub(crate) fn process_effect_inner(
    plugin: &PluginRef,
    input: &AudioBuffer,
    opts: &PluginOptions,
    progress: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<PluginRun> {
    check_sr(input.sample_rate)?;
    validate_block(opts)?;
    let mut s = PluginSession::open(plugin, opts.watchdog)?;
    let params = s.configure(opts)?;
    let state = s.capture_state()?;
    let (ci, co) = (s.info.main_in_channels, s.info.main_out_channels);
    if ci == 0 || co == 0 {
        return Err(Error::new(
            ErrorKind::Usage,
            "plugin_not_effect",
            format!(
                "plugin '{}' has no audio {} (is it an instrument?)",
                s.plugin.name,
                if ci == 0 { "input" } else { "output" }
            ),
        )
        .with_hint("render instruments with: ferraus render <file.mid> --instrument plugin:<name>"));
    }
    let act = s.host.activate(input.sample_rate, opts.block_size, ci, co, opts.slots)?;
    let latency = act.latency as usize;
    let tail = tail_frames(opts.tail, act.tail, input.sample_rate, false);
    let adapted = input.with_channels(ci as usize);
    let total = input.frames() + tail + latency;
    let user_progress = opts.progress;
    let out = s.host.run(&adapted.data, total, &[], &mut |f| {
        if let Some(p) = user_progress {
            p(f)
        }
        progress(f)
    })?;
    let data: Vec<Vec<f32>> = out.into_iter().map(|c| c[latency..].to_vec()).collect();
    let info = s.info.clone();
    s.host.shutdown();
    Ok(PluginRun {
        audio: AudioBuffer::from_planar(input.sample_rate, data),
        info,
        params,
        state,
        latency: act.latency,
        tail_frames: tail,
        preset: opts.preset.clone(),
    })
}

/// Run audio through an effect plugin with `tail_s` seconds of tail. See
/// [`process_effect_with`].
pub fn process_effect(
    plugin: &PluginRef,
    input: &AudioBuffer,
    params: &[(String, String)],
    state: Option<&[u8]>,
    tail_s: f64,
) -> Result<AudioBuffer> {
    let opts = PluginOptions {
        params: params.to_vec(),
        state: state.map(|s| s.to_vec()),
        tail: Tail::Seconds(tail_s),
        ..Default::default()
    };
    Ok(process_effect_with(plugin, input, &opts)?.audio)
}

/// Read a state file written by `plugin state save` (JSON with `state_b64`), or a raw
/// native blob.
pub fn read_state_file(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("cannot read state file {}: {e}", path.display()),
        )
    })?;
    if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
        let b64 = v
            .get("state_b64")
            .or_else(|| v.pointer("/plugins/0/state_b64"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| Error::usage(format!("{} is JSON but has no state_b64 field", path.display())))?;
        return base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| Error::usage(format!("{}: state_b64 is not base64: {e}", path.display())));
    }
    Ok(bytes)
}
