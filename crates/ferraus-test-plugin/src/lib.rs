//! Tiny CLAP plugins used by ferraus' plugin-host tests. One bundle exposes:
//!
//! * `ferraus.test.gain` — gain effect with a `gain` parameter in dB (display text
//!   `"-6.00 dB"`, text-to-value accepts `-6`, `-6dB`, `-6 dB`), plus two built-in presets
//!   (`Quiet` = -12 dB, `Loud` = +6 dB) published through the preset-discovery factory.
//! * `ferraus.test.delay` — delays audio by exactly [`DELAY_LATENCY`] samples and reports that
//!   latency, so a host that compensates latency gets the input back unchanged.
//! * `ferraus.test.sine` — a polyphonic sine instrument that starts each voice on the exact
//!   sample of its note-on event (CLAP note events or raw MIDI).
//! * `ferraus.test.crash` — aborts the process in `process()` when `crash_on_process` is on.
//! * `ferraus.test.hang` — sleeps forever in `process()` when `hang_on_process` is on (default).
//!
//! Every plugin implements the `state` extension (parameter values as bytes), so state
//! save/load round-trips can be tested. This crate is a test fixture, never shipped.

#![allow(clippy::missing_safety_doc)]

use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::preset_load::*;
use clap_sys::ext::state::*;
use clap_sys::ext::tail::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::factory::preset_discovery::*;
use clap_sys::host::clap_host;
use clap_sys::id::{clap_id, CLAP_INVALID_ID};
use clap_sys::plugin::*;
use clap_sys::process::*;
use clap_sys::stream::*;
use clap_sys::universal_plugin_id::clap_universal_plugin_id;
use clap_sys::version::CLAP_VERSION;
use std::ffi::{c_char, c_void, CStr};
use std::ptr::{null, null_mut};

/// Latency, in samples, reported (and applied) by `ferraus.test.delay`.
pub const DELAY_LATENCY: u32 = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Gain,
    Delay,
    Sine,
    Crash,
    Hang,
}

const KINDS: [Kind; 5] = [Kind::Gain, Kind::Delay, Kind::Sine, Kind::Crash, Kind::Hang];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Db,
    Bool,
}

struct ParamDef {
    id: clap_id,
    name: &'static str,
    min: f64,
    max: f64,
    default: f64,
    unit: Unit,
}

const GAIN_PARAMS: &[ParamDef] =
    &[ParamDef { id: 0, name: "gain", min: -60.0, max: 24.0, default: 0.0, unit: Unit::Db }];
const SINE_PARAMS: &[ParamDef] =
    &[ParamDef { id: 0, name: "level", min: -60.0, max: 0.0, default: -6.0, unit: Unit::Db }];
const CRASH_PARAMS: &[ParamDef] =
    &[ParamDef { id: 7, name: "crash_on_process", min: 0.0, max: 1.0, default: 0.0, unit: Unit::Bool }];
const HANG_PARAMS: &[ParamDef] =
    &[ParamDef { id: 9, name: "hang_on_process", min: 0.0, max: 1.0, default: 1.0, unit: Unit::Bool }];

fn params_of(k: Kind) -> &'static [ParamDef] {
    match k {
        Kind::Gain => GAIN_PARAMS,
        Kind::Delay => &[],
        Kind::Sine => SINE_PARAMS,
        Kind::Crash => CRASH_PARAMS,
        Kind::Hang => HANG_PARAMS,
    }
}

// ---------------------------------------------------------------------------------------
// descriptors

struct Ptrs<const N: usize>([*const c_char; N]);
unsafe impl<const N: usize> Sync for Ptrs<N> {}

static GAIN_FEATURES: Ptrs<3> = Ptrs([c"audio-effect".as_ptr(), c"utility".as_ptr(), null()]);
static DELAY_FEATURES: Ptrs<3> = Ptrs([c"audio-effect".as_ptr(), c"delay".as_ptr(), null()]);
static SINE_FEATURES: Ptrs<4> = Ptrs([c"instrument".as_ptr(), c"synthesizer".as_ptr(), c"stereo".as_ptr(), null()]);
static TEST_FEATURES: Ptrs<2> = Ptrs([c"audio-effect".as_ptr(), null()]);

const fn desc(id: &'static CStr, name: &'static CStr, features: *const *const c_char) -> clap_plugin_descriptor {
    clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: id.as_ptr(),
        name: name.as_ptr(),
        vendor: c"ferraus".as_ptr(),
        url: c"https://github.com/djynnius/ferraus".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"0.1.0".as_ptr(),
        description: c"ferraus test plugin (not for production use)".as_ptr(),
        features,
    }
}

static DESCRIPTORS: [clap_plugin_descriptor; 5] = [
    desc(c"ferraus.test.gain", c"Ferraus Test Gain", GAIN_FEATURES.0.as_ptr()),
    desc(c"ferraus.test.delay", c"Ferraus Test Delay", DELAY_FEATURES.0.as_ptr()),
    desc(c"ferraus.test.sine", c"Ferraus Test Sine", SINE_FEATURES.0.as_ptr()),
    desc(c"ferraus.test.crash", c"Ferraus Test Crash", TEST_FEATURES.0.as_ptr()),
    desc(c"ferraus.test.hang", c"Ferraus Test Hang", TEST_FEATURES.0.as_ptr()),
];

// ---------------------------------------------------------------------------------------
// instance

struct Voice {
    key: i16,
    channel: i16,
    phase: f64,
    inc: f64,
    amp: f64,
}

struct Instance {
    clap: clap_plugin,
    kind: Kind,
    values: Vec<f64>,
    sample_rate: f64,
    voices: Vec<Voice>,
    delay: Vec<Vec<f32>>,
    delay_pos: usize,
}

unsafe fn inst<'a>(p: *const clap_plugin) -> &'a mut Instance {
    &mut *((*p).plugin_data as *mut Instance)
}

fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

impl Instance {
    fn set_param(&mut self, id: clap_id, value: f64) {
        if let Some(i) = params_of(self.kind).iter().position(|p| p.id == id) {
            let d = &params_of(self.kind)[i];
            self.values[i] = value.clamp(d.min, d.max);
        }
    }

    fn reset(&mut self) {
        self.voices.clear();
        self.delay = vec![vec![0.0; DELAY_LATENCY as usize]; 2];
        self.delay_pos = 0;
    }

    unsafe fn apply_event(&mut self, hdr: *const clap_event_header) {
        let h = &*hdr;
        if h.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return;
        }
        match h.type_ {
            CLAP_EVENT_PARAM_VALUE => {
                let e = &*(hdr as *const clap_event_param_value);
                self.set_param(e.param_id, e.value);
            }
            CLAP_EVENT_NOTE_ON => {
                let e = &*(hdr as *const clap_event_note);
                self.note_on(e.key, e.channel, e.velocity);
            }
            CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE => {
                let e = &*(hdr as *const clap_event_note);
                self.note_off(e.key, e.channel);
            }
            CLAP_EVENT_MIDI => {
                let e = &*(hdr as *const clap_event_midi);
                let status = e.data[0] & 0xF0;
                let ch = (e.data[0] & 0x0F) as i16;
                let key = e.data[1] as i16;
                let vel = e.data[2];
                if status == 0x90 && vel > 0 {
                    self.note_on(key, ch, vel as f64 / 127.0);
                } else if status == 0x80 || status == 0x90 {
                    self.note_off(key, ch);
                }
            }
            _ => {}
        }
    }

    fn note_on(&mut self, key: i16, channel: i16, velocity: f64) {
        if self.kind != Kind::Sine || self.voices.len() >= 32 {
            return;
        }
        let freq = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
        self.voices.push(Voice {
            key,
            channel,
            phase: 0.0,
            inc: 2.0 * std::f64::consts::PI * freq / self.sample_rate,
            amp: velocity,
        });
    }

    fn note_off(&mut self, key: i16, channel: i16) {
        self.voices.retain(|v| !(v.key == key && (channel < 0 || v.channel == channel)));
    }

    fn render_sine(&mut self, outs: &mut [&mut [f32]], from: usize, to: usize) {
        let level = db_to_lin(self.values[0]);
        for i in from..to {
            let mut s = 0.0;
            for v in &mut self.voices {
                v.phase += v.inc;
                if v.phase > 2.0 * std::f64::consts::PI {
                    v.phase -= 2.0 * std::f64::consts::PI;
                }
                s += v.amp * v.phase.sin();
            }
            let s = (s * level) as f32;
            for o in outs.iter_mut() {
                o[i] = s;
            }
        }
    }
}

unsafe fn create(host: *const clap_host, kind: Kind, index: usize) -> *const clap_plugin {
    let _ = host;
    let inst = Box::new(Instance {
        clap: clap_plugin {
            desc: &DESCRIPTORS[index],
            plugin_data: null_mut(),
            init: Some(plugin_init),
            destroy: Some(plugin_destroy),
            activate: Some(plugin_activate),
            deactivate: Some(plugin_deactivate),
            start_processing: Some(plugin_start_processing),
            stop_processing: Some(plugin_stop_processing),
            reset: Some(plugin_reset),
            process: Some(plugin_process),
            get_extension: Some(plugin_get_extension),
            on_main_thread: Some(plugin_on_main_thread),
        },
        kind,
        values: params_of(kind).iter().map(|p| p.default).collect(),
        sample_rate: 48000.0,
        voices: Vec::new(),
        delay: Vec::new(),
        delay_pos: 0,
    });
    let raw = Box::into_raw(inst);
    (*raw).clap.plugin_data = raw as *mut c_void;
    &(*raw).clap
}

unsafe extern "C" fn plugin_init(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn plugin_destroy(p: *const clap_plugin) {
    let raw = (*p).plugin_data as *mut Instance;
    drop(Box::from_raw(raw));
}

unsafe extern "C" fn plugin_activate(p: *const clap_plugin, sr: f64, _min: u32, _max: u32) -> bool {
    let me = inst(p);
    me.sample_rate = sr;
    me.reset();
    true
}

unsafe extern "C" fn plugin_deactivate(_p: *const clap_plugin) {}

unsafe extern "C" fn plugin_start_processing(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn plugin_stop_processing(_p: *const clap_plugin) {}

unsafe extern "C" fn plugin_reset(p: *const clap_plugin) {
    inst(p).reset();
}

unsafe extern "C" fn plugin_on_main_thread(_p: *const clap_plugin) {}

unsafe fn port_channels<'a>(
    bufs: *const clap_sys::audio_buffer::clap_audio_buffer,
    count: u32,
    frames: usize,
) -> Vec<&'a mut [f32]> {
    let mut out = Vec::new();
    if bufs.is_null() || count == 0 {
        return out;
    }
    let b = &*bufs;
    if b.data32.is_null() {
        return out;
    }
    for c in 0..b.channel_count as usize {
        let ptr = *b.data32.add(c);
        if !ptr.is_null() {
            out.push(std::slice::from_raw_parts_mut(ptr, frames));
        }
    }
    out
}

unsafe extern "C" fn plugin_process(p: *const clap_plugin, process: *const clap_process) -> clap_process_status {
    let me = inst(p);
    let pr = &*process;
    let frames = pr.frames_count as usize;

    let mut events: Vec<*const clap_event_header> = Vec::new();
    if !pr.in_events.is_null() {
        let ie = &*pr.in_events;
        if let (Some(size), Some(get)) = (ie.size, ie.get) {
            for i in 0..size(pr.in_events) {
                let e = get(pr.in_events, i);
                if !e.is_null() {
                    events.push(e);
                }
            }
        }
    }

    // Inputs are read into owned copies first so in-place buffers stay correct.
    let ins: Vec<Vec<f32>> =
        port_channels(pr.audio_inputs, pr.audio_inputs_count, frames).into_iter().map(|s| s.to_vec()).collect();
    let mut outs = port_channels(pr.audio_outputs, pr.audio_outputs_count, frames);

    match me.kind {
        Kind::Sine => {
            let mut cursor = 0usize;
            for &e in &events {
                let t = ((*e).time as usize).min(frames);
                if t > cursor {
                    me.render_sine(&mut outs, cursor, t);
                    cursor = t;
                }
                me.apply_event(e);
            }
            me.render_sine(&mut outs, cursor, frames);
        }
        kind => {
            for &e in &events {
                me.apply_event(e);
            }
            if kind == Kind::Crash && me.values[0] >= 0.5 {
                std::process::abort();
            }
            if kind == Kind::Hang && me.values[0] >= 0.5 {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            let g = if kind == Kind::Gain { db_to_lin(me.values[0]) as f32 } else { 1.0 };
            for (c, out) in outs.iter_mut().enumerate() {
                let input = ins.get(c);
                for (i, o) in out.iter_mut().enumerate() {
                    let x = input.map(|v| v[i]).unwrap_or(0.0);
                    *o = if kind == Kind::Delay {
                        let line = &mut me.delay[c.min(1)];
                        let pos = (me.delay_pos + i) % DELAY_LATENCY as usize;
                        let y = line[pos];
                        line[pos] = x;
                        y
                    } else {
                        x * g
                    };
                }
            }
            if kind == Kind::Delay {
                me.delay_pos = (me.delay_pos + frames) % DELAY_LATENCY as usize;
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

// ---------------------------------------------------------------------------------------
// extensions

unsafe extern "C" fn plugin_get_extension(p: *const clap_plugin, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return null();
    }
    let id = CStr::from_ptr(id);
    let kind = inst(p).kind;
    if id == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS && kind == Kind::Sine {
        &NOTE_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_LATENCY && kind == Kind::Delay {
        &LATENCY as *const _ as *const c_void
    } else if id == CLAP_EXT_TAIL && kind != Kind::Hang {
        &TAIL as *const _ as *const c_void
    } else if (id == CLAP_EXT_PRESET_LOAD || id == CLAP_EXT_PRESET_LOAD_COMPAT) && kind == Kind::Gain {
        &PRESET_LOAD as *const _ as *const c_void
    } else {
        null()
    }
}

static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_get_info),
    get_value: Some(params_get_value),
    value_to_text: Some(params_value_to_text),
    text_to_value: Some(params_text_to_value),
    flush: Some(params_flush),
};

unsafe extern "C" fn params_count(p: *const clap_plugin) -> u32 {
    params_of(inst(p).kind).len() as u32
}

fn copy_str(dst: &mut [c_char], s: &str) {
    let n = s.len().min(dst.len() - 1);
    for (i, b) in s.as_bytes()[..n].iter().enumerate() {
        dst[i] = *b as c_char;
    }
    dst[n] = 0;
}

unsafe extern "C" fn params_get_info(p: *const clap_plugin, index: u32, info: *mut clap_param_info) -> bool {
    let defs = params_of(inst(p).kind);
    let Some(d) = defs.get(index as usize) else {
        return false;
    };
    let info = &mut *info;
    info.id = d.id;
    info.flags = CLAP_PARAM_IS_AUTOMATABLE | if d.unit == Unit::Bool { CLAP_PARAM_IS_STEPPED } else { 0 };
    info.cookie = null_mut();
    copy_str(&mut info.name, d.name);
    copy_str(&mut info.module, "");
    info.min_value = d.min;
    info.max_value = d.max;
    info.default_value = d.default;
    true
}

unsafe extern "C" fn params_get_value(p: *const clap_plugin, id: clap_id, out: *mut f64) -> bool {
    let me = inst(p);
    match params_of(me.kind).iter().position(|d| d.id == id) {
        Some(i) => {
            *out = me.values[i];
            true
        }
        None => false,
    }
}

unsafe extern "C" fn params_value_to_text(
    p: *const clap_plugin,
    id: clap_id,
    value: f64,
    buf: *mut c_char,
    cap: u32,
) -> bool {
    let me = inst(p);
    let Some(d) = params_of(me.kind).iter().find(|d| d.id == id) else {
        return false;
    };
    if cap == 0 {
        return false;
    }
    let s = match d.unit {
        Unit::Db => format!("{value:.2} dB"),
        Unit::Bool => (if value >= 0.5 { "on" } else { "off" }).to_string(),
    };
    let dst = std::slice::from_raw_parts_mut(buf, cap as usize);
    copy_str(dst, &s);
    true
}

unsafe extern "C" fn params_text_to_value(
    p: *const clap_plugin,
    id: clap_id,
    text: *const c_char,
    out: *mut f64,
) -> bool {
    let me = inst(p);
    let Some(d) = params_of(me.kind).iter().find(|d| d.id == id) else {
        return false;
    };
    if text.is_null() {
        return false;
    }
    let t = CStr::from_ptr(text).to_string_lossy().trim().to_ascii_lowercase();
    let v = match d.unit {
        Unit::Db => {
            let num = t.strip_suffix("db").unwrap_or(&t).trim();
            match num.parse::<f64>() {
                Ok(v) => v,
                Err(_) => return false,
            }
        }
        Unit::Bool => match t.as_str() {
            "on" | "true" | "1" | "yes" => 1.0,
            "off" | "false" | "0" | "no" => 0.0,
            _ => return false,
        },
    };
    *out = v.clamp(d.min, d.max);
    true
}

unsafe extern "C" fn params_flush(
    p: *const clap_plugin,
    in_: *const clap_input_events,
    _out: *const clap_output_events,
) {
    let me = inst(p);
    if in_.is_null() {
        return;
    }
    let ie = &*in_;
    if let (Some(size), Some(get)) = (ie.size, ie.get) {
        for i in 0..size(in_) {
            let e = get(in_, i);
            if !e.is_null() {
                me.apply_event(e);
            }
        }
    }
}

static STATE: clap_plugin_state = clap_plugin_state { save: Some(state_save), load: Some(state_load) };

unsafe extern "C" fn state_save(p: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let me = inst(p);
    let mut bytes = b"FTP1".to_vec();
    bytes.push(KINDS.iter().position(|k| *k == me.kind).unwrap_or(0) as u8);
    bytes.extend_from_slice(&(me.values.len() as u32).to_le_bytes());
    for v in &me.values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let s = &*stream;
    let Some(write) = s.write else {
        return false;
    };
    let mut off = 0usize;
    while off < bytes.len() {
        let n = write(stream, bytes[off..].as_ptr() as *const c_void, (bytes.len() - off) as u64);
        if n <= 0 {
            return false;
        }
        off += n as usize;
    }
    true
}

unsafe extern "C" fn state_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
    let me = inst(p);
    let s = &*stream;
    let Some(read) = s.read else {
        return false;
    };
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let n = read(stream, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u64);
        if n < 0 {
            return false;
        }
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n as usize]);
    }
    if bytes.len() < 9 || &bytes[..4] != b"FTP1" {
        return false;
    }
    let count = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]) as usize;
    if count != me.values.len() || bytes.len() != 9 + 8 * count {
        return false;
    }
    for i in 0..count {
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[9 + 8 * i..17 + 8 * i]);
        me.values[i] = f64::from_le_bytes(b);
    }
    true
}

static AUDIO_PORTS: clap_plugin_audio_ports =
    clap_plugin_audio_ports { count: Some(audio_ports_count), get: Some(audio_ports_get) };

unsafe extern "C" fn audio_ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    if is_input && inst(p).kind == Kind::Sine {
        0
    } else {
        1
    }
}

unsafe extern "C" fn audio_ports_get(
    p: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if index >= audio_ports_count(p, is_input) {
        return false;
    }
    let info = &mut *info;
    info.id = 0;
    copy_str(&mut info.name, if is_input { "in" } else { "out" });
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = CLAP_INVALID_ID;
    true
}

static NOTE_PORTS: clap_plugin_note_ports =
    clap_plugin_note_ports { count: Some(note_ports_count), get: Some(note_ports_get) };

unsafe extern "C" fn note_ports_count(_p: *const clap_plugin, is_input: bool) -> u32 {
    if is_input {
        1
    } else {
        0
    }
}

unsafe extern "C" fn note_ports_get(
    _p: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if !is_input || index != 0 {
        return false;
    }
    let info = &mut *info;
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    copy_str(&mut info.name, "notes");
    true
}

static LATENCY: clap_plugin_latency = clap_plugin_latency { get: Some(latency_get) };

unsafe extern "C" fn latency_get(_p: *const clap_plugin) -> u32 {
    DELAY_LATENCY
}

static TAIL: clap_plugin_tail = clap_plugin_tail { get: Some(tail_get) };

unsafe extern "C" fn tail_get(_p: *const clap_plugin) -> u32 {
    0
}

static PRESET_LOAD: clap_plugin_preset_load = clap_plugin_preset_load { from_location: Some(preset_from_location) };

const PRESETS: &[(&CStr, &CStr, f64)] = &[(c"Quiet", c"quiet", -12.0), (c"Loud", c"loud", 6.0)];

unsafe extern "C" fn preset_from_location(
    p: *const clap_plugin,
    kind: u32,
    _location: *const c_char,
    load_key: *const c_char,
) -> bool {
    if kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN || load_key.is_null() {
        return false;
    }
    let key = CStr::from_ptr(load_key);
    match PRESETS.iter().find(|(_, k, _)| *k == key) {
        Some((_, _, gain)) => {
            inst(p).values[0] = *gain;
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------------------
// factories and entry

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_descriptor),
    create_plugin: Some(factory_create),
};

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    DESCRIPTORS.len() as u32
}

unsafe extern "C" fn factory_descriptor(_f: *const clap_plugin_factory, index: u32) -> *const clap_plugin_descriptor {
    DESCRIPTORS.get(index as usize).map(|d| d as *const _).unwrap_or(null())
}

unsafe extern "C" fn factory_create(
    _f: *const clap_plugin_factory,
    host: *const clap_host,
    plugin_id: *const c_char,
) -> *const clap_plugin {
    if plugin_id.is_null() {
        return null();
    }
    let want = CStr::from_ptr(plugin_id);
    for (i, d) in DESCRIPTORS.iter().enumerate() {
        if CStr::from_ptr(d.id) == want {
            return create(host, KINDS[i], i);
        }
    }
    null()
}

static PROVIDER_DESC: clap_preset_discovery_provider_descriptor = clap_preset_discovery_provider_descriptor {
    clap_version: CLAP_VERSION,
    id: c"ferraus.test.presets".as_ptr(),
    name: c"Ferraus Test Presets".as_ptr(),
    vendor: c"ferraus".as_ptr(),
};

static PRESET_FACTORY: clap_preset_discovery_factory = clap_preset_discovery_factory {
    count: Some(pf_count),
    get_descriptor: Some(pf_descriptor),
    create: Some(pf_create),
};

unsafe extern "C" fn pf_count(_f: *const clap_preset_discovery_factory) -> u32 {
    1
}

unsafe extern "C" fn pf_descriptor(
    _f: *const clap_preset_discovery_factory,
    index: u32,
) -> *const clap_preset_discovery_provider_descriptor {
    if index == 0 {
        &PROVIDER_DESC
    } else {
        null()
    }
}

struct Provider {
    provider: clap_preset_discovery_provider,
    indexer: *const clap_preset_discovery_indexer,
}

unsafe extern "C" fn pf_create(
    _f: *const clap_preset_discovery_factory,
    indexer: *const clap_preset_discovery_indexer,
    provider_id: *const c_char,
) -> *const clap_preset_discovery_provider {
    if provider_id.is_null() || CStr::from_ptr(provider_id) != c"ferraus.test.presets" {
        return null();
    }
    let b = Box::new(Provider {
        provider: clap_preset_discovery_provider {
            desc: &PROVIDER_DESC,
            provider_data: null_mut(),
            init: Some(provider_init),
            destroy: Some(provider_destroy),
            get_metadata: Some(provider_get_metadata),
            get_extension: Some(provider_get_extension),
        },
        indexer,
    });
    let raw = Box::into_raw(b);
    (*raw).provider.provider_data = raw as *mut c_void;
    &(*raw).provider
}

unsafe extern "C" fn provider_init(p: *const clap_preset_discovery_provider) -> bool {
    let me = &*((*p).provider_data as *const Provider);
    if me.indexer.is_null() {
        return false;
    }
    let ix = &*me.indexer;
    if let Some(declare) = ix.declare_location {
        let loc = clap_preset_discovery_location {
            flags: CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT,
            name: c"Built-in".as_ptr(),
            kind: CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
            location: null(),
        };
        declare(me.indexer, &loc);
    }
    true
}

unsafe extern "C" fn provider_destroy(p: *const clap_preset_discovery_provider) {
    drop(Box::from_raw((*p).provider_data as *mut Provider));
}

unsafe extern "C" fn provider_get_metadata(
    _p: *const clap_preset_discovery_provider,
    kind: u32,
    _location: *const c_char,
    receiver: *const clap_preset_discovery_metadata_receiver,
) -> bool {
    if kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN || receiver.is_null() {
        return false;
    }
    let r = &*receiver;
    let id = clap_universal_plugin_id { abi: c"clap".as_ptr(), id: c"ferraus.test.gain".as_ptr() };
    for (name, key, _) in PRESETS {
        if let Some(begin) = r.begin_preset {
            if !begin(receiver, name.as_ptr(), key.as_ptr()) {
                break;
            }
        }
        if let Some(add) = r.add_plugin_id {
            add(receiver, &id);
        }
        if let Some(flags) = r.set_flags {
            flags(receiver, CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT);
        }
    }
    true
}

unsafe extern "C" fn provider_get_extension(
    _p: *const clap_preset_discovery_provider,
    _id: *const c_char,
) -> *const c_void {
    null()
}

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if id.is_null() {
        return null();
    }
    let id = CStr::from_ptr(id);
    if id == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const _ as *const c_void
    } else if id == CLAP_PRESET_DISCOVERY_FACTORY_ID || id == CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT {
        &PRESET_FACTORY as *const _ as *const c_void
    } else {
        null()
    }
}

/// The CLAP entry point every host looks up.
#[allow(non_upper_case_globals)]
#[no_mangle]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};
