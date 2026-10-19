//! The wire protocol between ferraus and a plugin host process (`ferraus-plughost` for CLAP,
//! `ferraus-plughost-vst3` for VST3). See `PROTOCOL.md` in this crate for the full spec.
//!
//! * Control: length-prefixed JSON frames (`u32` little-endian byte length, then UTF-8 JSON)
//!   on the host's stdin (requests) and stdout (replies). Exactly one reply per request, in
//!   order. The host's own stdout is redirected to stderr before any plugin code runs, so a
//!   chatty plugin can never corrupt the stream.
//! * Audio: a memory-mapped temp file created by the client, holding a 64-byte header and a
//!   ring of `slots` blocks; each slot has planar `f32` input channels followed by planar
//!   output channels of `block_size` frames.
//!
//! This module has no dependency on `ferraus-core`, so the host binaries link only this.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Bumped on any incompatible change to the messages or the shared-memory layout.
pub const PROTOCOL_VERSION: u32 = 1;
/// Largest accepted frame (64 MiB): states and preset lists are the only big payloads.
pub const MAX_FRAME: usize = 64 << 20;
/// First 8 bytes of the shared-memory file.
pub const SHM_MAGIC: &[u8; 8] = b"FRAUSHM1";
/// Size of the shared-memory header in bytes.
pub const SHM_HEADER: usize = 64;

/// Write one frame: `u32` LE length followed by the JSON bytes.
pub fn write_frame<W: Write + ?Sized, T: Serialize>(w: &mut W, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::other("frame too large"));
    }
    let mut buf = Vec::with_capacity(4 + bytes.len());
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&bytes);
    w.write_all(&buf)?;
    w.flush()
}

/// Read one frame. `Ok(None)` on a clean end of stream before a frame starts.
pub fn read_frame<R: Read + ?Sized>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut len[got..])?;
        if n == 0 {
            if got == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated frame header"));
        }
        got += n;
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::other(format!("frame of {n} bytes exceeds the limit")));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Client → host.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Handshake; the reply names the host, its version and protocol version.
    Hello,
    /// List the plugins inside a bundle without instantiating any (used by `plugin scan`).
    Probe { path: String },
    /// Load the bundle and instantiate one plugin (`plugin_id` may be omitted when the
    /// bundle holds exactly one). Replies with [`PluginInfo`].
    Load {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin_id: Option<String>,
    },
    /// Set parameters by id or display name; values in display text (`2.5kHz`, `-6dB`,
    /// `30%`), plain numbers, or `norm:0.3`. Replies with the resulting values.
    SetParams { params: Vec<ParamAssign> },
    /// Current parameter values.
    GetParams,
    /// Discover presets (CLAP preset-discovery factory / VST3 program lists).
    ListPresets,
    /// Load a preset by name or load key. Replies with the resulting parameter values.
    LoadPreset { preset: String },
    /// Save the plugin's native state. Replies with base64 bytes.
    GetState,
    /// Restore native state saved earlier.
    SetState { state_b64: String },
    /// Activate at a sample rate and maximum block size and map the shared-memory file.
    Activate { sample_rate: f64, block_size: u32, shm_path: String },
    /// Process `frames` (≤ block size) from slot `slot`; events carry sample offsets
    /// relative to the block start, sorted by time.
    Process {
        slot: u32,
        frames: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<Event>,
    },
    /// Stop processing and deactivate (the plugin stays loaded).
    Deactivate,
    /// Tear down and exit.
    Quit,
}

/// Host → client.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "reply", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // one reply per request; boxing buys nothing here
pub enum Reply {
    Hello {
        host: String,
        version: String,
        protocol: u32,
        formats: Vec<String>,
    },
    Probe {
        plugins: Vec<Descriptor>,
    },
    Loaded {
        info: PluginInfo,
    },
    Params {
        params: Vec<ParamValue>,
    },
    Presets {
        presets: Vec<PresetInfo>,
    },
    State {
        state_b64: String,
    },
    Activated {
        latency: u32,
        /// Tail in samples as the plugin reports it; `None` when it does not say,
        /// `Some(u32::MAX)` for infinite.
        #[serde(default)]
        tail: Option<u32>,
        channels_in: u32,
        channels_out: u32,
    },
    Processed {
        slot: u32,
    },
    Ok,
    Error {
        code: String,
        message: String,
    },
}

/// A sample-accurate event inside one block.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    NoteOn {
        time: u32,
        channel: u8,
        key: u8,
        velocity: f32,
    },
    NoteOff {
        time: u32,
        channel: u8,
        key: u8,
        velocity: f32,
    },
    /// Any 3-byte MIDI message (CC, pitch bend, aftertouch, program change...).
    Midi {
        time: u32,
        data: [u8; 3],
    },
}

impl Event {
    pub fn time(&self) -> u32 {
        match self {
            Event::NoteOn { time, .. } | Event::NoteOff { time, .. } | Event::Midi { time, .. } => *time,
        }
    }
}

/// `name=value` as given on the command line.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ParamAssign {
    /// Stable parameter id (`12`, `id:12`) or display name (`cutoff`, `Filter/Cutoff`).
    pub name: String,
    pub value: String,
}

/// What a plugin declares about itself (no instantiation needed).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Descriptor {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    /// CLAP feature strings or VST3 sub-categories, lower-cased.
    #[serde(default)]
    pub features: Vec<String>,
    /// `instrument`, `audio-effect`, `note-effect`, `analyzer`, or `other`.
    #[serde(default)]
    pub kind: String,
}

impl Descriptor {
    /// Derive [`Descriptor::kind`] from feature strings.
    pub fn classify(features: &[String]) -> String {
        let has = |f: &str| features.iter().any(|x| x.eq_ignore_ascii_case(f));
        if has("instrument") || has("synth") || has("synthesizer") || has("sampler") {
            "instrument"
        } else if has("note-effect") {
            "note-effect"
        } else if has("audio-effect") || has("fx") {
            "audio-effect"
        } else if has("analyzer") {
            "analyzer"
        } else {
            "other"
        }
        .to_string()
    }
}

/// One parameter, with its range in plain (plugin) units.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub value: f64,
    /// Display text of the default and current values, from the plugin.
    #[serde(default)]
    pub default_text: String,
    #[serde(default)]
    pub value_text: String,
    /// Unit inferred from the display text (`dB`, `Hz`, `%`, `ms`...), when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default)]
    pub stepped: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub bypass: bool,
    #[serde(default)]
    pub automatable: bool,
}

/// A parameter's value after a set/get.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ParamValue {
    pub id: u32,
    pub name: String,
    pub value: f64,
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PortInfo {
    pub name: String,
    pub channels: u32,
    pub main: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct NotePortInfo {
    pub name: String,
    /// `clap`, `midi`, `midi-mpe`, `midi2`.
    pub dialects: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PresetInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_key: Option<String>,
    /// `plugin` (built into the plugin), `file`, or `program` (VST3 program list).
    pub location_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creators: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
}

/// Everything ferraus needs after instantiation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PluginInfo {
    pub descriptor: Descriptor,
    /// `clap` or `vst3`.
    pub format: String,
    pub params: Vec<ParamInfo>,
    pub audio_inputs: Vec<PortInfo>,
    pub audio_outputs: Vec<PortInfo>,
    pub note_inputs: Vec<NotePortInfo>,
    pub note_outputs: Vec<NotePortInfo>,
    /// Channels of the main input/output port: the ones carried in shared memory.
    pub main_in_channels: u32,
    pub main_out_channels: u32,
    pub has_state: bool,
    pub has_presets: bool,
}

impl PluginInfo {
    pub fn accepts_notes(&self) -> bool {
        !self.note_inputs.is_empty()
    }
}

// ---------------------------------------------------------------------------------------
// shared memory

/// Geometry of the shared-memory audio file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShmLayout {
    pub channels_in: u32,
    pub channels_out: u32,
    pub block_size: u32,
    pub slots: u32,
}

impl ShmLayout {
    pub fn slot_bytes(&self) -> usize {
        (self.channels_in + self.channels_out) as usize * self.block_size as usize * 4
    }

    pub fn total_bytes(&self) -> usize {
        SHM_HEADER + self.slot_bytes() * self.slots as usize
    }

    /// Byte offset of input channel `ch` in `slot`.
    pub fn input_offset(&self, slot: u32, ch: u32) -> usize {
        SHM_HEADER + slot as usize * self.slot_bytes() + (ch * self.block_size) as usize * 4
    }

    /// Byte offset of output channel `ch` in `slot`.
    pub fn output_offset(&self, slot: u32, ch: u32) -> usize {
        SHM_HEADER + slot as usize * self.slot_bytes() + ((self.channels_in + ch) * self.block_size) as usize * 4
    }

    pub fn write_header(&self, buf: &mut [u8]) {
        buf[..8].copy_from_slice(SHM_MAGIC);
        let words = [PROTOCOL_VERSION, self.channels_in, self.channels_out, self.block_size, self.slots];
        for (i, w) in words.iter().enumerate() {
            buf[8 + 4 * i..12 + 4 * i].copy_from_slice(&w.to_le_bytes());
        }
    }

    pub fn read_header(buf: &[u8]) -> Result<ShmLayout, String> {
        if buf.len() < SHM_HEADER || &buf[..8] != SHM_MAGIC {
            return Err("shared memory file has no ferraus header".into());
        }
        let w = |i: usize| u32::from_le_bytes([buf[8 + 4 * i], buf[9 + 4 * i], buf[10 + 4 * i], buf[11 + 4 * i]]);
        if w(0) != PROTOCOL_VERSION {
            return Err(format!("shared memory protocol {} does not match host protocol {PROTOCOL_VERSION}", w(0)));
        }
        let l = ShmLayout { channels_in: w(1), channels_out: w(2), block_size: w(3), slots: w(4) };
        if l.block_size == 0 || l.slots == 0 || l.total_bytes() > buf.len() {
            return Err("shared memory header is inconsistent with the file size".into());
        }
        Ok(l)
    }
}

/// View `n` f32 values at byte offset `off` of a mapped region.
///
/// # Safety
/// `base` must point to a live mapping at least `off + 4n` bytes long, `off` must be a
/// multiple of 4 (the mapping itself is page-aligned), and no other live reference may
/// alias the same range.
pub unsafe fn f32_slice_mut<'a>(base: *mut u8, off: usize, n: usize) -> &'a mut [f32] {
    std::slice::from_raw_parts_mut(base.add(off) as *mut f32, n)
}

// ---------------------------------------------------------------------------------------
// parameter text conversion (shared by every backend)

/// Hooks into a plugin's own text conversion.
pub trait ParamText {
    fn text_to_value(&mut self, id: u32, text: &str) -> Option<f64>;
    fn value_to_text(&mut self, id: u32, value: f64) -> Option<String>;
}

fn fmt_num(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Split `"2.5kHz"` into `(2.5, "khz")`.
fn split_number(text: &str) -> Option<(f64, String)> {
    let t = text.trim();
    if !t.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-' | '+')) {
        return None;
    }
    // the longest numeric prefix (handles `1e3`, `-6`, `2.5`)
    let end = (1..=t.len()).rev().filter(|&i| t.is_char_boundary(i)).find(|&i| {
        let s = &t[..i];
        s.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')) && s.parse::<f64>().is_ok()
    })?;
    let n: f64 = t[..end].parse().ok()?;
    Some((n, t[end..].trim().to_ascii_lowercase()))
}

/// Text variants to try against a plugin's `text_to_value` for a ferraus-style value.
/// `"2.5kHz"` yields `["2.5kHz", "2500 Hz", "2500Hz", "2500", "2.5 kHz", "2.5"]`.
pub fn text_candidates(text: &str) -> Vec<String> {
    let mut out = vec![text.trim().to_string()];
    if let Some((n, unit)) = split_number(text) {
        let mut push = |s: String| {
            if !out.contains(&s) {
                out.push(s)
            }
        };
        match unit.as_str() {
            "khz" | "k" => {
                let hz = n * 1000.0;
                push(format!("{} Hz", fmt_num(hz)));
                push(format!("{}Hz", fmt_num(hz)));
                push(fmt_num(hz));
                push(format!("{} kHz", fmt_num(n)));
                push(fmt_num(n));
            }
            "hz" => {
                push(format!("{} Hz", fmt_num(n)));
                push(format!("{}Hz", fmt_num(n)));
                push(fmt_num(n));
                push(format!("{} kHz", fmt_num(n / 1000.0)));
            }
            "db" => {
                push(format!("{} dB", fmt_num(n)));
                push(format!("{}dB", fmt_num(n)));
                push(fmt_num(n));
            }
            "%" => {
                push(format!("{} %", fmt_num(n)));
                push(format!("{}%", fmt_num(n)));
                push(fmt_num(n));
                push(fmt_num(n / 100.0));
            }
            "ms" => {
                push(format!("{} ms", fmt_num(n)));
                push(format!("{}ms", fmt_num(n)));
                push(fmt_num(n));
                push(format!("{} s", fmt_num(n / 1000.0)));
            }
            "s" | "sec" => {
                push(format!("{} s", fmt_num(n)));
                push(format!("{}s", fmt_num(n)));
                push(format!("{} ms", fmt_num(n * 1000.0)));
                push(fmt_num(n));
            }
            "st" | "semitones" | "semi" => {
                push(format!("{} st", fmt_num(n)));
                push(format!("{} semitones", fmt_num(n)));
                push(fmt_num(n));
            }
            "ct" | "cents" | "cent" => {
                push(format!("{} ct", fmt_num(n)));
                push(format!("{} cents", fmt_num(n)));
                push(fmt_num(n));
            }
            "" => push(fmt_num(n)),
            other => {
                push(format!("{} {other}", fmt_num(n)));
                push(fmt_num(n));
            }
        }
    }
    out
}

/// The unit suffix of a display string: `"-6.00 dB"` → `dB`.
pub fn infer_unit(display: &str) -> Option<String> {
    let (_, unit) = split_number(display)?;
    if unit.is_empty() {
        return None;
    }
    let orig = display.trim();
    let u = orig[orig.len() - unit.len()..].trim().to_string();
    if u.chars().any(|c| c.is_ascii_digit()) || u.len() > 12 {
        None
    } else {
        Some(u)
    }
}

/// Find a parameter by stable id (`12`, `id:12`) or display name (exact, case-insensitive,
/// punctuation-insensitive, or `module/name`).
pub fn find_param<'a>(params: &'a [ParamInfo], key: &str) -> Result<&'a ParamInfo, String> {
    let key = key.trim();
    let idkey = key.strip_prefix("id:").unwrap_or(key);
    if let Ok(id) = idkey.parse::<u32>() {
        if let Some(p) = params.iter().find(|p| p.id == id) {
            if key.starts_with("id:") || !params.iter().any(|p| p.name == key) {
                return Ok(p);
            }
        }
    }
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect::<String>();
    let full = |p: &ParamInfo| {
        if p.module.is_empty() {
            p.name.clone()
        } else {
            format!("{}/{}", p.module.trim_end_matches('/'), p.name)
        }
    };
    let stages: [&dyn Fn(&ParamInfo) -> bool; 4] = [
        &|p| p.name == key || full(p) == key,
        &|p| p.name.eq_ignore_ascii_case(key) || full(p).eq_ignore_ascii_case(key),
        &|p| norm(&p.name) == norm(key) || norm(&full(p)) == norm(key),
        &|p| norm(&p.name).starts_with(&norm(key)) && !norm(key).is_empty(),
    ];
    for test in stages {
        let hits: Vec<&ParamInfo> = params.iter().filter(|p| test(p)).collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(hits[0]),
            _ => {
                let names: Vec<String> = hits.iter().take(8).map(|p| format!("{} (id {})", full(p), p.id)).collect();
                return Err(format!(
                    "parameter '{key}' is ambiguous: {}; use the id (e.g. id:{})",
                    names.join(", "),
                    hits[0].id
                ));
            }
        }
    }
    let mut known: Vec<String> = params.iter().filter(|p| !p.hidden).take(12).map(|p| p.name.clone()).collect();
    if params.len() > 12 {
        known.push("...".into());
    }
    Err(format!("unknown parameter '{key}'; parameters include: {}", known.join(", ")))
}

/// Convert a ferraus value string to the plugin's plain value: `norm:x`, the plugin's own
/// text parser (with unit-spelling variants), a plain in-range number, or a stepped label.
pub fn resolve_value(p: &ParamInfo, text: &str, conv: &mut dyn ParamText) -> Result<f64, String> {
    let t = text.trim();
    let (lo, hi) = (p.min.min(p.max), p.min.max(p.max));
    let in_range = |v: f64| v >= lo - 1e-9 * (1.0 + lo.abs()) && v <= hi + 1e-9 * (1.0 + hi.abs());
    if let Some(n) = t.strip_prefix("norm:") {
        let x: f64 = n.trim().parse().map_err(|_| format!("'{t}' is not norm:<0..1>"))?;
        if !(0.0..=1.0).contains(&x) {
            return Err(format!("{t} is outside norm:0..1"));
        }
        let mut v = p.min + x * (p.max - p.min);
        if p.stepped {
            v = v.round();
        }
        return Ok(v);
    }
    for c in text_candidates(t) {
        if let Some(v) = conv.text_to_value(p.id, &c) {
            if v.is_finite() {
                return Ok(v.clamp(lo, hi));
            }
        }
    }
    if let Ok(v) = t.parse::<f64>() {
        if in_range(v) {
            return Ok(v.clamp(lo, hi));
        }
    }
    if p.stepped && hi - lo <= 512.0 {
        let mut v = lo.ceil();
        while v <= hi {
            if let Some(label) = conv.value_to_text(p.id, v) {
                if label.trim().eq_ignore_ascii_case(t) {
                    return Ok(v);
                }
            }
            v += 1.0;
        }
    }
    let example = if p.default_text.is_empty() { fmt_num(p.default) } else { p.default_text.clone() };
    Err(format!(
        "cannot convert '{t}' for parameter '{}' (range {}..{}, e.g. '{example}' or norm:0.5)",
        p.name,
        fmt_num(p.min),
        fmt_num(p.max)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Request::Hello).unwrap();
        write_frame(
            &mut buf,
            &Request::Process {
                slot: 2,
                frames: 64,
                events: vec![Event::NoteOn { time: 3, channel: 0, key: 60, velocity: 0.8 }],
            },
        )
        .unwrap();
        let mut r = &buf[..];
        let a: Request = serde_json::from_slice(&read_frame(&mut r).unwrap().unwrap()).unwrap();
        let b: Request = serde_json::from_slice(&read_frame(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(a, Request::Hello);
        assert!(matches!(b, Request::Process { slot: 2, frames: 64, .. }));
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn candidates_cover_units() {
        let c = text_candidates("2.5kHz");
        assert!(c.contains(&"2500 Hz".to_string()) && c.contains(&"2500".to_string()));
        assert!(text_candidates("30%").contains(&"0.3".to_string()));
        assert!(text_candidates("-6dB").contains(&"-6".to_string()));
        assert_eq!(infer_unit("-6.00 dB").as_deref(), Some("dB"));
        assert_eq!(infer_unit("440 Hz").as_deref(), Some("Hz"));
        assert_eq!(infer_unit("0.500"), None);
    }

    #[test]
    fn shm_layout_offsets() {
        let l = ShmLayout { channels_in: 2, channels_out: 2, block_size: 512, slots: 4 };
        let mut b = vec![0u8; l.total_bytes()];
        l.write_header(&mut b);
        assert_eq!(ShmLayout::read_header(&b).unwrap(), l);
        assert_eq!(l.input_offset(0, 0), 64);
        assert_eq!(l.output_offset(0, 0), 64 + 2 * 512 * 4);
        assert_eq!(l.input_offset(1, 0), 64 + 4 * 512 * 4);
    }

    struct Db;
    impl ParamText for Db {
        fn text_to_value(&mut self, _id: u32, t: &str) -> Option<f64> {
            t.trim().parse().ok()
        }
        fn value_to_text(&mut self, _id: u32, v: f64) -> Option<String> {
            Some(format!("{v:.2} dB"))
        }
    }

    #[test]
    fn resolves_values() {
        let p = ParamInfo { id: 3, name: "gain".into(), min: -60.0, max: 24.0, ..Default::default() };
        assert_eq!(resolve_value(&p, "-6dB", &mut Db).unwrap(), -6.0);
        assert_eq!(resolve_value(&p, "norm:0.5", &mut Db).unwrap(), -18.0);
        assert!(resolve_value(&p, "loud", &mut Db).is_err());
        let ps = vec![p.clone()];
        assert_eq!(find_param(&ps, "Gain").unwrap().id, 3);
        assert_eq!(find_param(&ps, "id:3").unwrap().id, 3);
        assert!(find_param(&ps, "cutoff").is_err());
    }
}
