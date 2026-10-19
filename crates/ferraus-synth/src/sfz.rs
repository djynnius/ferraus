//! A pure-Rust SFZ sampler for the common opcodes.
//!
//! The PRD mentions sfizz through a feature-gated FFI; a native player keeps the build
//! pure Rust (no C++ toolchain, no shared library to ship), deterministic, and sandbox
//! friendly, and covers the opcodes most free libraries use:
//!
//! headers `<control> <global> <master> <group> <region>` (inheritance in that order);
//! `#define $NAME value` and `#include "file"`; `sample` (WAV 8/16/24/32-bit and float,
//! FLAC; `*sine` and `*silence` generators), `default_path`, `note_offset`, `octave_offset`,
//! `lokey`/`hikey`/`key`, `lovel`/`hivel`, `lochan`/`hichan`, `pitch_keycenter`,
//! `pitch_keytrack`, `transpose`, `tune`, `volume`, `pan`, `amp_veltrack`, `ampeg_delay`,
//! `ampeg_attack`, `ampeg_hold`, `ampeg_decay`, `ampeg_sustain`, `ampeg_release`,
//! `loop_mode` (`no_loop`, `one_shot`, `loop_continuous`, `loop_sustain`), `loop_start`,
//! `loop_end` (or the WAV `smpl` loop), `offset`, `end`, `trigger` (`attack`, `release`),
//! `seq_length`/`seq_position`, `lorand`/`hirand`, `bend_up`/`bend_down`. Unknown opcodes
//! are ignored. Velocity layers, round robins, and release samples therefore work; filters,
//! LFOs, and modulation opcodes are not implemented.

use crate::render::Engine;
use ferraus_core::{units, Error, ErrorKind, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
enum LoopMode {
    NoLoop,
    OneShot,
    Continuous,
    Sustain,
}

#[derive(Clone, Debug)]
struct Region {
    sample: usize,
    lokey: u8,
    hikey: u8,
    lovel: u8,
    hivel: u8,
    lochan: u8,
    hichan: u8,
    keycenter: f64,
    keytrack: f64,
    transpose: f64,
    tune: f64,
    volume_db: f64,
    pan: f64,
    veltrack: f64,
    delay: f64,
    attack: f64,
    hold: f64,
    decay: f64,
    sustain: f64,
    release: f64,
    loop_mode: Option<LoopMode>,
    loop_start: Option<usize>,
    loop_end: Option<usize>,
    offset: usize,
    end: Option<usize>,
    release_trigger: bool,
    seq_length: u32,
    seq_position: u32,
    lorand: f64,
    hirand: f64,
    bend_up: f64,
    bend_down: f64,
}

/// Decoded sample data (planar, 1 or 2 channels).
#[derive(Debug)]
struct Sample {
    sr: f64,
    data: Vec<Vec<f32>>,
    loop_points: Option<(usize, usize)>,
}

impl Sample {
    fn frames(&self) -> usize {
        self.data.first().map(|c| c.len()).unwrap_or(0)
    }
}

/// A parsed SFZ instrument with its samples loaded.
#[derive(Debug)]
pub struct SfzInstrument {
    regions: Vec<Region>,
    samples: Vec<Arc<Sample>>,
}

fn sfz_err(path: &Path, msg: impl std::fmt::Display) -> Error {
    Error::unsupported(format!("{}: {msg}", path.display()))
}

/// Strip comments, apply `#define`, expand `#include`.
fn preprocess(path: &Path, defines: &mut HashMap<String, String>, depth: usize) -> Result<String> {
    if depth > 8 {
        return Err(sfz_err(path, "#include nesting is too deep"));
    }
    let raw = std::fs::read(path).map_err(|e| {
        Error::new(ErrorKind::InputMissing, "sound_not_found", format!("cannot read {}: {e}", path.display()))
    })?;
    let text = String::from_utf8_lossy(&raw);
    // remove /* */ comments
    let mut s = String::with_capacity(text.len());
    let mut rest: &str = &text;
    while let Some(i) = rest.find("/*") {
        s.push_str(&rest[..i]);
        rest = match rest[i + 2..].find("*/") {
            Some(j) => &rest[i + 2 + j + 2..],
            None => "",
        };
    }
    s.push_str(rest);
    let mut out = String::new();
    for line in s.lines() {
        let line = match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        };
        let t = line.trim();
        if let Some(d) = t.strip_prefix("#define") {
            let mut it = d.split_whitespace();
            if let (Some(k), Some(v)) = (it.next(), it.next()) {
                defines.insert(k.to_string(), v.to_string());
            }
            continue;
        }
        if let Some(inc) = t.strip_prefix("#include") {
            let name = inc.trim().trim_matches('"').replace('\\', "/");
            let p = path.parent().unwrap_or(Path::new(".")).join(name);
            out.push_str(&preprocess(&p, defines, depth + 1)?);
            out.push('\n');
            continue;
        }
        let mut l = line.to_string();
        if l.contains('$') {
            let mut keys: Vec<&String> = defines.keys().collect();
            keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
            for k in keys {
                l = l.replace(k.as_str(), &defines[k]);
            }
        }
        out.push_str(&l);
        out.push('\n');
    }
    Ok(out)
}

#[derive(Debug)]
enum Token {
    Header(String),
    Opcode(String, String),
}

/// Opcodes whose value may contain spaces (runs to the next opcode or header).
fn spacey(key: &str) -> bool {
    matches!(
        key,
        "sample" | "default_path" | "label" | "region_label" | "group_label" | "master_label" | "global_label"
    )
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    for line in text.lines() {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() {
                break;
            }
            if b[i] == b'<' {
                let j = line[i..].find('>').map(|j| i + j).unwrap_or(b.len());
                out.push(Token::Header(line[i + 1..j.min(b.len())].trim().to_ascii_lowercase()));
                i = (j + 1).min(b.len());
                continue;
            }
            let Some(eq) = line[i..].find('=').map(|e| i + e) else { break };
            let key = line[i..eq].trim().to_string();
            let vstart = eq + 1;
            let vend = if spacey(&key) {
                // up to the next ` word=` or `<`
                let mut end = b.len();
                let mut k = vstart;
                while k < b.len() {
                    if b[k] == b'<' {
                        end = k;
                        break;
                    }
                    if b[k].is_ascii_whitespace() {
                        let rest = &line[k..];
                        let trimmed = rest.trim_start();
                        let word_end = trimmed.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(trimmed.len());
                        if trimmed[word_end..].starts_with('=') && word_end > 0 {
                            end = k;
                            break;
                        }
                    }
                    k += 1;
                }
                end
            } else {
                line[vstart..].find(|c: char| c.is_whitespace() || c == '<').map(|e| vstart + e).unwrap_or(b.len())
            };
            out.push(Token::Opcode(key, line[vstart..vend].trim().to_string()));
            i = vend;
        }
    }
    out
}

fn parse_key(v: &str, note_offset: i32) -> Option<u8> {
    let k = v.trim().parse::<i32>().ok().or_else(|| units::parse_note_name(v))?;
    Some((k + note_offset).clamp(0, 127) as u8)
}

fn num(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok()
}

fn default_region() -> Region {
    Region {
        sample: usize::MAX,
        lokey: 0,
        hikey: 127,
        lovel: 1,
        hivel: 127,
        lochan: 1,
        hichan: 16,
        keycenter: 60.0,
        keytrack: 100.0,
        transpose: 0.0,
        tune: 0.0,
        volume_db: 0.0,
        pan: 0.0,
        veltrack: 100.0,
        delay: 0.0,
        attack: 0.0,
        hold: 0.0,
        decay: 0.0,
        sustain: 100.0,
        release: 0.001,
        loop_mode: None,
        loop_start: None,
        loop_end: None,
        offset: 0,
        end: None,
        release_trigger: false,
        seq_length: 1,
        seq_position: 1,
        lorand: 0.0,
        hirand: 1.0,
        bend_up: 200.0,
        bend_down: -200.0,
    }
}

impl SfzInstrument {
    pub fn load(path: &Path) -> Result<SfzInstrument> {
        let mut defines = HashMap::new();
        let text = preprocess(path, &mut defines, 0)?;
        let tokens = tokenize(&text);
        let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        // opcode maps per level
        let mut control: Vec<(String, String)> = Vec::new();
        let mut global: Vec<(String, String)> = Vec::new();
        let mut master: Vec<(String, String)> = Vec::new();
        let mut group: Vec<(String, String)> = Vec::new();
        let mut regions_ops: Vec<Vec<(String, String)>> = Vec::new();
        #[derive(PartialEq)]
        enum Lvl {
            None,
            Control,
            Global,
            Master,
            Group,
            Region,
            Ignore,
        }
        let mut lvl = Lvl::None;
        let mut cur: Vec<(String, String)> = Vec::new();
        let mut defaults_for_region: Vec<(String, String)> = Vec::new();
        let mut paths_at_region: Vec<(String, i32)> = Vec::new();
        let flush = |lvl: &Lvl,
                     cur: &mut Vec<(String, String)>,
                     regions_ops: &mut Vec<Vec<(String, String)>>,
                     defaults: &[(String, String)]| {
            if *lvl == Lvl::Region {
                let mut ops = defaults.to_vec();
                ops.append(cur);
                regions_ops.push(ops);
            }
        };
        let mut default_path = String::new();
        let mut note_offset: i32;
        for t in tokens {
            match t {
                Token::Header(h) => {
                    flush(&lvl, &mut cur, &mut regions_ops, &defaults_for_region);
                    match lvl {
                        Lvl::Control => control.append(&mut cur),
                        Lvl::Global => global.append(&mut cur),
                        Lvl::Master => master.append(&mut cur),
                        Lvl::Group => group.append(&mut cur),
                        _ => cur.clear(),
                    }
                    let (mut no, mut oo) = (0i32, 0i32);
                    for (k, v) in &control {
                        match k.as_str() {
                            "default_path" => default_path = v.replace('\\', "/"),
                            "note_offset" => no = v.trim().parse().unwrap_or(0),
                            "octave_offset" => oo = v.trim().parse().unwrap_or(0),
                            _ => {}
                        }
                    }
                    note_offset = no + 12 * oo;
                    lvl = match h.as_str() {
                        "control" => {
                            control.clear();
                            Lvl::Control
                        }
                        "global" => {
                            global.clear();
                            master.clear();
                            group.clear();
                            Lvl::Global
                        }
                        "master" => {
                            master.clear();
                            group.clear();
                            Lvl::Master
                        }
                        "group" => {
                            group.clear();
                            Lvl::Group
                        }
                        "region" => Lvl::Region,
                        _ => Lvl::Ignore,
                    };
                    defaults_for_region = global.iter().chain(&master).chain(&group).cloned().collect();
                    if lvl == Lvl::Region {
                        paths_at_region.push((default_path.clone(), note_offset));
                    }
                }
                Token::Opcode(k, v) => {
                    if lvl != Lvl::Ignore {
                        cur.push((k, v));
                    }
                }
            }
        }
        flush(&lvl, &mut cur, &mut regions_ops, &defaults_for_region);
        let mut samples: Vec<Arc<Sample>> = Vec::new();
        let mut sample_index: HashMap<String, usize> = HashMap::new();
        let mut regions = Vec::new();
        for (ri, ops) in regions_ops.iter().enumerate() {
            let (dpath, noff) = paths_at_region.get(ri).cloned().unwrap_or((String::new(), 0));
            let mut r = default_region();
            let mut sample_name: Option<String> = None;
            for (k, v) in ops {
                match k.as_str() {
                    "sample" => sample_name = Some(v.clone()),
                    "lokey" => r.lokey = parse_key(v, noff).unwrap_or(r.lokey),
                    "hikey" => r.hikey = parse_key(v, noff).unwrap_or(r.hikey),
                    "key" => {
                        if let Some(x) = parse_key(v, noff) {
                            r.lokey = x;
                            r.hikey = x;
                            r.keycenter = x as f64;
                        }
                    }
                    "pitch_keycenter" => {
                        if let Some(x) = parse_key(v, noff) {
                            r.keycenter = x as f64
                        }
                    }
                    "lovel" => r.lovel = num(v).unwrap_or(1.0).clamp(0.0, 127.0) as u8,
                    "hivel" => r.hivel = num(v).unwrap_or(127.0).clamp(0.0, 127.0) as u8,
                    "lochan" => r.lochan = num(v).unwrap_or(1.0).clamp(1.0, 16.0) as u8,
                    "hichan" => r.hichan = num(v).unwrap_or(16.0).clamp(1.0, 16.0) as u8,
                    "pitch_keytrack" => r.keytrack = num(v).unwrap_or(100.0),
                    "transpose" => r.transpose = num(v).unwrap_or(0.0),
                    "tune" | "pitch" => r.tune = num(v).unwrap_or(0.0),
                    "volume" => r.volume_db = num(v).unwrap_or(0.0),
                    "pan" => r.pan = num(v).unwrap_or(0.0).clamp(-100.0, 100.0),
                    "amp_veltrack" => r.veltrack = num(v).unwrap_or(100.0),
                    "ampeg_delay" => r.delay = num(v).unwrap_or(0.0).max(0.0),
                    "ampeg_attack" => r.attack = num(v).unwrap_or(0.0).max(0.0),
                    "ampeg_hold" => r.hold = num(v).unwrap_or(0.0).max(0.0),
                    "ampeg_decay" => r.decay = num(v).unwrap_or(0.0).max(0.0),
                    "ampeg_sustain" => r.sustain = num(v).unwrap_or(100.0).clamp(0.0, 100.0),
                    "ampeg_release" => r.release = num(v).unwrap_or(0.001).max(0.0),
                    "loop_mode" | "loopmode" => {
                        r.loop_mode = Some(match v.as_str() {
                            "one_shot" => LoopMode::OneShot,
                            "loop_continuous" => LoopMode::Continuous,
                            "loop_sustain" => LoopMode::Sustain,
                            _ => LoopMode::NoLoop,
                        })
                    }
                    "loop_start" | "loopstart" => r.loop_start = num(v).map(|x| x.max(0.0) as usize),
                    "loop_end" | "loopend" => r.loop_end = num(v).map(|x| x.max(0.0) as usize),
                    "offset" => r.offset = num(v).unwrap_or(0.0).max(0.0) as usize,
                    "end" => r.end = num(v).map(|x| x.max(0.0) as usize),
                    "trigger" => r.release_trigger = v == "release",
                    "seq_length" => r.seq_length = num(v).unwrap_or(1.0).max(1.0) as u32,
                    "seq_position" => r.seq_position = num(v).unwrap_or(1.0).max(1.0) as u32,
                    "lorand" => r.lorand = num(v).unwrap_or(0.0),
                    "hirand" => r.hirand = num(v).unwrap_or(1.0),
                    "bend_up" => r.bend_up = num(v).unwrap_or(200.0),
                    "bend_down" => r.bend_down = num(v).unwrap_or(-200.0),
                    _ => {}
                }
            }
            let Some(name) = sample_name else { continue };
            let key = format!("{dpath}{name}");
            let idx = match sample_index.get(&key) {
                Some(i) => *i,
                None => {
                    let s = load_sample(&base_dir, &dpath, &name, r.keycenter)?;
                    samples.push(Arc::new(s));
                    sample_index.insert(key, samples.len() - 1);
                    samples.len() - 1
                }
            };
            r.sample = idx;
            regions.push(r);
        }
        if regions.is_empty() {
            return Err(sfz_err(path, "no playable <region> with a sample"));
        }
        Ok(SfzInstrument { regions, samples })
    }

    pub fn region_count(&self) -> usize {
        self.regions.len()
    }
}

fn load_sample(base: &Path, default_path: &str, name: &str, keycenter: f64) -> Result<Sample> {
    let n = name.trim();
    if n == "*silence" {
        return Ok(Sample { sr: 48000.0, data: vec![vec![0.0; 16]], loop_points: None });
    }
    if n == "*sine" {
        // one cycle at the key centre, looped
        let len = 2048usize;
        let f = 440.0 * 2f64.powf((keycenter - 69.0) / 12.0);
        let data: Vec<f32> =
            (0..len).map(|i| (2.0 * std::f64::consts::PI * i as f64 / len as f64).sin() as f32).collect();
        return Ok(Sample { sr: f * len as f64, data: vec![data], loop_points: Some((0, len)) });
    }
    let rel: PathBuf = PathBuf::from(format!("{default_path}{}", n.replace('\\', "/")));
    let p = if rel.is_absolute() { rel } else { base.join(rel) };
    let bytes = std::fs::read(&p).map_err(|e| {
        Error::new(
            ErrorKind::InputMissing,
            "sample_not_found",
            format!("SFZ sample {} cannot be read: {e}", p.display()),
        )
    })?;
    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if ext == "flac" || bytes.starts_with(b"fLaC") {
        let mut r = claxon::FlacReader::new(std::io::Cursor::new(&bytes))
            .map_err(|e| sfz_err(&p, format!("not a valid FLAC file: {e}")))?;
        let info = r.streaminfo();
        let ch = info.channels.max(1) as usize;
        let scale = 1.0 / (1u64 << (info.bits_per_sample.max(1) - 1)) as f32;
        let mut data = vec![Vec::new(); ch];
        for (i, s) in r.samples().enumerate() {
            let s = s.map_err(|e| sfz_err(&p, format!("FLAC decode error: {e}")))?;
            data[i % ch].push(s as f32 * scale);
        }
        return Ok(Sample { sr: info.sample_rate as f64, data, loop_points: None });
    }
    if ext == "wav" || bytes.starts_with(b"RIFF") {
        let (buf, _, _) = ferraus_core::wav::decode(&bytes).map_err(|e| sfz_err(&p, e.message))?;
        let mut data = buf.data;
        data.truncate(2);
        return Ok(Sample { sr: buf.sample_rate as f64, data, loop_points: wav_loop(&bytes) });
    }
    Err(sfz_err(&p, "only WAV and FLAC samples are supported"))
}

/// First loop from a WAV `smpl` chunk (end exclusive).
fn wav_loop(b: &[u8]) -> Option<(usize, usize)> {
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let len = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().ok()?) as usize;
        if &b[pos..pos + 4] == b"smpl" {
            let body = b.get(pos + 8..pos + 8 + len)?;
            let n = u32::from_le_bytes(body.get(28..32)?.try_into().ok()?);
            if n == 0 {
                return None;
            }
            let s = u32::from_le_bytes(body.get(44..48)?.try_into().ok()?) as usize;
            let e = u32::from_le_bytes(body.get(48..52)?.try_into().ok()?) as usize;
            return if e > s { Some((s, e + 1)) } else { None };
        }
        pos += 8 + len + (len & 1);
    }
    None
}

struct SVoice {
    region: usize,
    sample: Arc<Sample>,
    ch: u8,
    key: u8,
    pos: f64,
    step: f64,
    gain_l: f64,
    gain_r: f64,
    t: u64,
    released_at: Option<(u64, f64)>,
    pedal_held: bool,
    loop_mode: LoopMode,
    loop_pts: Option<(usize, usize)>,
    end: usize,
    done: bool,
    // envelope in samples
    delay: f64,
    attack: f64,
    hold: f64,
    decay: f64,
    sustain: f64,
    release: f64,
    base_ratio: f64,
}

impl SVoice {
    fn env(&self) -> f64 {
        let t = self.t as f64;
        let held = if t < self.delay {
            0.0
        } else if t < self.delay + self.attack {
            (t - self.delay) / self.attack.max(1.0)
        } else if t < self.delay + self.attack + self.hold {
            1.0
        } else {
            let td = t - self.delay - self.attack - self.hold;
            if self.decay <= 0.0 {
                self.sustain
            } else {
                // exponential decay reaching -60 dB of the distance in `decay`
                self.sustain + (1.0 - self.sustain) * (-6.9 * td / self.decay).exp()
            }
        };
        match self.released_at {
            Some((r, lvl)) => {
                let tr = t - r as f64;
                if self.release <= 1.0 {
                    if tr >= 1.0 {
                        0.0
                    } else {
                        lvl
                    }
                } else {
                    lvl * (-6.9 * tr / self.release).exp()
                }
            }
            None => held,
        }
    }

    fn release(&mut self) {
        if self.released_at.is_none() && self.loop_mode != LoopMode::OneShot {
            let l = self.env();
            self.released_at = Some((self.t, l));
        }
    }

    fn sample_at(&self, c: usize, i: isize) -> f32 {
        let d = &self.sample.data[c.min(self.sample.data.len() - 1)];
        let i = if let (Some((ls, le)), true) = (self.loop_pts, self.looping()) {
            if i as usize >= le && le > ls {
                (ls + (i as usize - ls) % (le - ls)) as isize
            } else {
                i
            }
        } else {
            i
        };
        if i < 0 || i as usize >= d.len() {
            0.0
        } else {
            d[i as usize]
        }
    }

    fn looping(&self) -> bool {
        match self.loop_mode {
            LoopMode::Continuous => true,
            LoopMode::Sustain => self.released_at.is_none(),
            _ => false,
        }
    }

    /// Cubic (Catmull-Rom) interpolated frame.
    fn frame(&self) -> (f64, f64) {
        let i = self.pos.floor() as isize;
        let f = self.pos - i as f64;
        let ch = self.sample.data.len();
        let mut out = [0.0f64; 2];
        for (c, o) in out.iter_mut().enumerate().take(ch.max(1)) {
            let p0 = self.sample_at(c, i - 1) as f64;
            let p1 = self.sample_at(c, i) as f64;
            let p2 = self.sample_at(c, i + 1) as f64;
            let p3 = self.sample_at(c, i + 2) as f64;
            *o = p1 + 0.5 * f * (p2 - p0 + f * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + f * (3.0 * (p1 - p2) + p3 - p0)));
        }
        if ch == 1 {
            (out[0], out[0])
        } else {
            (out[0], out[1])
        }
    }
}

#[derive(Clone, Copy)]
struct SChan {
    volume: u8,
    expression: u8,
    pan: u8,
    sustain: bool,
    bend: f64,
}

/// SFZ engine: every channel plays the instrument (regions can filter by `lochan`/`hichan`).
pub struct SfzEngine {
    inst: Arc<SfzInstrument>,
    sr: f64,
    voices: Vec<SVoice>,
    ch: [SChan; 16],
    seq_counter: [u32; 128],
    last_vel: [[u8; 128]; 16],
    rng: u64,
}

impl SfzEngine {
    pub fn open(path: &Path, sample_rate: u32) -> Result<SfzEngine> {
        Ok(SfzEngine::new(Arc::new(SfzInstrument::load(path)?), sample_rate))
    }

    pub fn new(inst: Arc<SfzInstrument>, sample_rate: u32) -> SfzEngine {
        SfzEngine {
            inst,
            sr: sample_rate as f64,
            voices: Vec::new(),
            ch: [SChan { volume: 100, expression: 127, pan: 64, sustain: false, bend: 0.0 }; 16],
            seq_counter: [0; 128],
            last_vel: [[0; 128]; 16],
            rng: 0x2545_F491_4F6C_DD1D,
        }
    }

    fn rand(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    fn start(&mut self, ch: u8, key: u8, vel: u8, release: bool) {
        let counter = self.seq_counter[key as usize];
        let r = self.rand();
        let inst = self.inst.clone();
        for (ri, reg) in inst.regions.iter().enumerate() {
            if reg.release_trigger != release
                || key < reg.lokey
                || key > reg.hikey
                || vel < reg.lovel
                || vel > reg.hivel
                || ch + 1 < reg.lochan
                || ch + 1 > reg.hichan
                || (counter % reg.seq_length) + 1 != reg.seq_position
                || r < reg.lorand
                || r >= reg.hirand
            {
                continue;
            }
            let sample = inst.samples[reg.sample].clone();
            let frames = sample.frames();
            let end = reg.end.map(|e| (e + 1).min(frames)).unwrap_or(frames);
            let loop_pts = match (reg.loop_start, reg.loop_end) {
                (Some(s), Some(e)) if e > s => Some((s, (e + 1).min(frames))),
                _ => sample.loop_points,
            };
            let loop_mode = reg.loop_mode.unwrap_or(if loop_pts.is_some() && sample.loop_points.is_some() {
                LoopMode::Continuous
            } else {
                LoopMode::NoLoop
            });
            let semis = (key as f64 - reg.keycenter) * reg.keytrack / 100.0 + reg.transpose + reg.tune / 100.0;
            let base_ratio = 2f64.powf(semis / 12.0) * sample.sr / self.sr;
            let v = vel as f64 / 127.0;
            let velgain = 1.0 - reg.veltrack / 100.0 * (1.0 - v * v);
            let g = 10f64.powf(reg.volume_db / 20.0) * velgain.max(0.0);
            let theta = (reg.pan + 100.0) / 200.0 * std::f64::consts::FRAC_PI_2;
            let sr = self.sr;
            self.voices.push(SVoice {
                region: ri,
                sample,
                ch,
                key,
                pos: reg.offset as f64,
                step: base_ratio,
                gain_l: g * theta.cos() * std::f64::consts::SQRT_2,
                gain_r: g * theta.sin() * std::f64::consts::SQRT_2,
                t: 0,
                released_at: None,
                pedal_held: false,
                loop_mode,
                loop_pts,
                end,
                done: false,
                delay: reg.delay * sr,
                attack: reg.attack * sr,
                hold: reg.hold * sr,
                decay: reg.decay * sr,
                sustain: reg.sustain / 100.0,
                release: reg.release * sr,
                base_ratio,
            });
            if release {
                // release samples play through without waiting for a note-off
                let l = self.voices.len() - 1;
                self.voices[l].loop_mode = LoopMode::OneShot;
            }
        }
    }
}

impl Engine for SfzEngine {
    fn message(&mut self, status: u8, channel: u8, data1: u8, data2: u8) {
        let ch = channel & 0x0F;
        match status {
            0x9 if data2 > 0 => {
                self.last_vel[ch as usize][data1 as usize] = data2;
                self.start(ch, data1, data2, false);
                self.seq_counter[data1 as usize] = self.seq_counter[data1 as usize].wrapping_add(1);
            }
            0x8 | 0x9 => {
                let sustain = self.ch[ch as usize].sustain;
                for v in self.voices.iter_mut().filter(|v| v.ch == ch && v.key == data1 && v.released_at.is_none()) {
                    if sustain {
                        v.pedal_held = true;
                    } else {
                        v.release();
                    }
                }
                let vel = self.last_vel[ch as usize][data1 as usize];
                if vel > 0 && self.inst.regions.iter().any(|r| r.release_trigger) {
                    self.start(ch, data1, vel, true);
                }
            }
            0xB => {
                let c = &mut self.ch[ch as usize];
                match data1 {
                    7 => c.volume = data2,
                    10 => c.pan = data2,
                    11 => c.expression = data2,
                    64 => {
                        c.sustain = data2 >= 64;
                        if !c.sustain {
                            for v in self.voices.iter_mut().filter(|v| v.ch == ch && v.pedal_held) {
                                v.pedal_held = false;
                                v.release();
                            }
                        }
                    }
                    120 => self.voices.retain(|v| v.ch != ch),
                    123 => {
                        for v in self.voices.iter_mut().filter(|v| v.ch == ch) {
                            v.release();
                        }
                    }
                    _ => {}
                }
            }
            0xE => {
                let raw = ((data2 as i32) << 7 | data1 as i32) - 8192;
                self.ch[ch as usize].bend = raw as f64 / 8192.0;
            }
            _ => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        let inst = self.inst.clone();
        for v in self.voices.iter_mut() {
            let c = self.ch[v.ch as usize];
            let reg = &inst.regions[v.region];
            let cents = if c.bend >= 0.0 { c.bend * reg.bend_up } else { -c.bend * reg.bend_down };
            v.step = v.base_ratio * 2f64.powf(cents / 1200.0);
            let cg = (c.volume as f64 / 127.0).powi(2) * (c.expression as f64 / 127.0).powi(2);
            let theta = c.pan as f64 / 127.0 * std::f64::consts::FRAC_PI_2;
            let (pl, pr) = (theta.cos() * std::f64::consts::SQRT_2, theta.sin() * std::f64::consts::SQRT_2);
            for i in 0..left.len() {
                if v.done {
                    break;
                }
                let e = v.env();
                if v.released_at.is_some() && e < 1e-5 {
                    v.done = true;
                    break;
                }
                let (a, b) = v.frame();
                left[i] += (a * e * v.gain_l * cg * pl * 0.5) as f32;
                right[i] += (b * e * v.gain_r * cg * pr * 0.5) as f32;
                v.pos += v.step;
                v.t += 1;
                if v.looping() {
                    if let Some((ls, le)) = v.loop_pts {
                        if le > ls {
                            while v.pos >= le as f64 {
                                v.pos -= (le - ls) as f64;
                            }
                        }
                    }
                } else if v.pos >= v.end as f64 {
                    v.done = true;
                }
            }
        }
        self.voices.retain(|v| !v.done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_handles_spaces_and_headers() {
        let t = tokenize("<region> sample=My Piano C4.wav lokey=c4 hikey=60 <region>key=62 sample=x.wav");
        let ops: Vec<String> = t
            .iter()
            .map(|t| match t {
                Token::Header(h) => format!("<{h}>"),
                Token::Opcode(k, v) => format!("{k}={v}"),
            })
            .collect();
        assert_eq!(
            ops,
            vec!["<region>", "sample=My Piano C4.wav", "lokey=c4", "hikey=60", "<region>", "key=62", "sample=x.wav"]
        );
    }

    #[test]
    fn keys_and_note_names() {
        assert_eq!(parse_key("c4", 0), Some(60));
        assert_eq!(parse_key("60", 0), Some(60));
        assert_eq!(parse_key("a#3", 12), Some(70));
    }

    #[test]
    fn sine_generator_region_plays_at_pitch() {
        let dir = ferraus_core::testing::TempDir::new("sfz-sine");
        let p = dir.path("sine.sfz");
        std::fs::write(
            &p,
            "// test\n#define $CENTER 69\n<group> ampeg_release=0.05\n<region> sample=*sine pitch_keycenter=$CENTER\n",
        )
        .unwrap();
        let mut e = SfzEngine::open(&p, 48000).unwrap();
        e.message(0x9, 0, 69, 127);
        let mut l = vec![0.0; 24000];
        let mut r = vec![0.0; 24000];
        e.render(&mut l, &mut r);
        let lv440 = ferraus_core::testing::tone_level(&l[2400..], 48000, 440.0);
        let lv466 = ferraus_core::testing::tone_level(&l[2400..], 48000, 466.0);
        assert!(lv440 > 0.2 && lv440 > 10.0 * lv466, "{lv440} {lv466}");
    }
}
