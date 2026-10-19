//! The built-in preview synth: a small General MIDI-ish instrument set that needs no
//! files, so `render` always works. Band-limited (PolyBLEP) oscillators, additive and FM
//! voices, ADSR envelopes, and TPT state-variable filters; a synthesized drum kit on
//! channel 10. Fully deterministic (noise comes from per-voice seeded generators).

use crate::render::Engine;
use ferraus_midi::gm;
use std::f64::consts::PI;

const TAU: f64 = 2.0 * PI;
const MAX_VOICES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Timbre {
    Piano,
    Bell,
    Organ,
    Pluck,
    Bass,
    Strings,
    Brass,
    Lead,
    Pad,
    Drum(Drum),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drum {
    Kick,
    Snare,
    Clap,
    Rim,
    HatClosed,
    HatOpen,
    Tom(f64),
    Crash,
    Ride,
    Cowbell,
    Tambourine,
    Perc(f64),
}

/// The built-in timbre a GM program maps to.
fn timbre_for_program(p: u8) -> Timbre {
    match p {
        0..=7 => Timbre::Piano,
        8..=15 => Timbre::Bell,
        16..=23 => Timbre::Organ,
        24..=31 => Timbre::Pluck,
        32..=39 => Timbre::Bass,
        40..=44 => Timbre::Strings,
        45 | 46 => Timbre::Pluck,
        47 => Timbre::Bass,
        48..=51 => Timbre::Strings,
        52..=54 => Timbre::Pad,
        55..=63 => Timbre::Brass,
        64..=71 => Timbre::Lead,
        72..=79 => Timbre::Organ,
        80..=87 => Timbre::Lead,
        88..=103 => Timbre::Pad,
        104..=108 => Timbre::Pluck,
        109 => Timbre::Organ,
        110 => Timbre::Strings,
        111 => Timbre::Lead,
        112..=114 => Timbre::Bell,
        115..=119 => Timbre::Pluck,
        _ => Timbre::Pad,
    }
}

/// Name of the built-in timbre used for a GM program (for reports).
pub fn timbre_name_for_program(p: u8) -> &'static str {
    match timbre_for_program(p) {
        Timbre::Piano => "piano",
        Timbre::Bell => "bell",
        Timbre::Organ => "organ",
        Timbre::Pluck => "guitar",
        Timbre::Bass => "bass",
        Timbre::Strings => "strings",
        Timbre::Brass => "brass",
        Timbre::Lead => "lead",
        Timbre::Pad => "pad",
        Timbre::Drum(_) => "drums",
    }
}

fn drum_for_key(k: u8) -> Drum {
    match k {
        35 | 36 => Drum::Kick,
        38 | 40 => Drum::Snare,
        39 => Drum::Clap,
        37 => Drum::Rim,
        42 | 44 => Drum::HatClosed,
        46 => Drum::HatOpen,
        41 => Drum::Tom(80.0),
        43 => Drum::Tom(95.0),
        45 => Drum::Tom(110.0),
        47 => Drum::Tom(130.0),
        48 => Drum::Tom(150.0),
        50 => Drum::Tom(175.0),
        49 | 52 | 55 | 57 => Drum::Crash,
        51 | 53 | 59 => Drum::Ride,
        56 => Drum::Cowbell,
        54 => Drum::Tambourine,
        k => Drum::Perc(400.0 + (k as f64 - 60.0).abs() * 20.0),
    }
}

fn drum_len(d: Drum) -> f64 {
    match d {
        Drum::Kick => 1.8,
        Drum::Snare => 0.9,
        Drum::Clap => 0.6,
        Drum::Rim => 0.12,
        Drum::HatClosed => 0.3,
        Drum::HatOpen => 1.6,
        Drum::Tom(_) => 1.8,
        Drum::Crash => 6.0,
        Drum::Ride => 5.0,
        Drum::Cowbell => 0.9,
        Drum::Tambourine => 0.9,
        Drum::Perc(_) => 0.4,
    }
}

/// (attack s, decay time constant s, sustain level, release time constant s)
fn adsr_of(t: Timbre, f0: f64) -> (f64, f64, f64, f64) {
    match t {
        Timbre::Piano => (0.002, 1e9, 1.0, 0.08),
        Timbre::Bell => (0.001, 1e9, 1.0, 0.4),
        Timbre::Organ => (0.006, 1e9, 1.0, 0.03),
        Timbre::Pluck => (0.001, 0.9 * (261.6 / f0).powf(0.4), 0.0, 0.05),
        Timbre::Bass => (0.003, 0.5, 0.65, 0.04),
        Timbre::Strings => (0.09, 0.3, 0.9, 0.15),
        Timbre::Brass => (0.035, 0.25, 0.8, 0.08),
        Timbre::Lead => (0.006, 0.2, 0.85, 0.06),
        Timbre::Pad => (0.35, 0.5, 0.9, 0.4),
        Timbre::Drum(_) => (0.0005, 1e9, 1.0, 0.05),
    }
}

/// Convex attack (fast initial rise) then exponential decay to sustain.
fn adsr_level(t: f64, a: f64, d: f64, s: f64) -> f64 {
    if t < a {
        let x = t / a;
        1.0 - (1.0 - x).powi(3)
    } else {
        s + (1.0 - s) * (-(t - a) / d).exp()
    }
}

fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

fn saw(ph: f64, dt: f64) -> f64 {
    2.0 * ph - 1.0 - poly_blep(ph, dt)
}

fn square(ph: f64, dt: f64) -> f64 {
    let v = if ph < 0.5 { 1.0 } else { -1.0 };
    v + poly_blep(ph, dt) - poly_blep((ph + 0.5) % 1.0, dt)
}

fn wrap(p: f64) -> f64 {
    p - p.floor()
}

/// Topology-preserving-transform state-variable filter.
#[derive(Clone, Copy, Default)]
struct Svf {
    ic1: f64,
    ic2: f64,
}

impl Svf {
    /// Returns (low, band, high).
    fn run(&mut self, x: f64, fc: f64, q: f64, sr: f64) -> (f64, f64, f64) {
        let g = (PI * fc.clamp(10.0, 0.45 * sr) / sr).tan();
        let k = 1.0 / q.max(0.1);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1, x - k * v1 - v2)
    }
}

struct Voice {
    timbre: Timbre,
    ch: u8,
    key: u8,
    f0: f64,
    vel: f64,
    t: u64,
    release_at: Option<u64>,
    rel_level: f64,
    pedal_held: bool,
    phase: [f64; 10],
    amp: [f64; 10],
    decay: [f64; 10],
    freq_mul: [f64; 10],
    partials: usize,
    svf: Svf,
    svf2: Svf,
    rng: u64,
    done: bool,
    level: f64,
}

impl Voice {
    fn new(timbre: Timbre, ch: u8, key: u8, vel: u8, sr: f64, seed: u64) -> Voice {
        let f0 = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
        let v = vel as f64 / 127.0;
        let mut vc = Voice {
            timbre,
            ch,
            key,
            f0,
            vel: v,
            t: 0,
            release_at: None,
            rel_level: 0.0,
            pedal_held: false,
            phase: [0.0; 10],
            amp: [0.0; 10],
            decay: [1.0; 10],
            freq_mul: [0.0; 10],
            partials: 0,
            svf: Svf::default(),
            svf2: Svf::default(),
            rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            done: false,
            level: 0.0,
        };
        match timbre {
            Timbre::Piano => {
                let b = 0.0003;
                let tau0 = (4.0 * (261.6 / f0).powf(0.6)).clamp(0.6, 10.0);
                let bright = 0.55 - 0.4 * v;
                let mut sum = 0.0;
                for n in 1..=10usize {
                    let nf = n as f64;
                    let m = nf * (1.0 + b * nf * nf).sqrt();
                    if m * f0 > 0.45 * sr {
                        break;
                    }
                    vc.freq_mul[n - 1] = m;
                    let a = nf.powf(-1.4) * (-(nf - 1.0) * bright).exp();
                    vc.amp[n - 1] = a;
                    sum += a;
                    let tau = tau0 / (1.0 + 0.6 * (nf - 1.0));
                    vc.decay[n - 1] = (-1.0 / (tau * sr)).exp();
                    vc.partials = n;
                }
                for a in vc.amp.iter_mut() {
                    *a /= sum.max(1e-9);
                }
            }
            Timbre::Organ => {
                let bars = [(0.5, 0.5), (1.0, 1.0), (2.0, 0.8), (3.0, 0.45), (4.0, 0.35), (6.0, 0.2), (8.0, 0.15)];
                let mut sum = 0.0;
                for (m, a) in bars {
                    if m * f0 > 0.45 * sr {
                        continue;
                    }
                    vc.freq_mul[vc.partials] = m;
                    vc.amp[vc.partials] = a;
                    sum += a;
                    vc.partials += 1;
                }
                for a in vc.amp.iter_mut() {
                    *a /= sum.max(1e-9);
                }
            }
            Timbre::Strings => {
                vc.phase[0] = 0.5;
                vc.phase[1] = 0.25;
                vc.phase[2] = 0.6;
            }
            Timbre::Pad | Timbre::Brass | Timbre::Lead | Timbre::Pluck | Timbre::Bass => {
                vc.phase[0] = 0.5;
                vc.phase[1] = 0.5;
            }
            _ => {}
        }
        vc
    }

    fn noise(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }

    fn release(&mut self, sr: f64) {
        if self.release_at.is_none() {
            self.rel_level = self.env_level(self.t as f64 / sr);
            self.release_at = Some(self.t);
        }
    }

    fn env_level(&self, ts: f64) -> f64 {
        let (a, d, s, _) = adsr_of(self.timbre, self.f0);
        adsr_level(ts, a, d, s)
    }

    /// One output sample (mono, before channel gain and pan).
    fn tick(&mut self, sr: f64, bend: f64, modwheel: f64) -> f64 {
        let ts = self.t as f64 / sr;
        let (_, _, sustain, rtau) = adsr_of(self.timbre, self.f0);
        let env = match self.release_at {
            Some(r) => self.rel_level * (-((self.t - r) as f64 / sr) / rtau).exp(),
            None => self.env_level(ts),
        };
        self.level = env;
        let f = self.f0 * bend;
        let out = match self.timbre {
            Timbre::Piano | Timbre::Organ => {
                let mut s = 0.0;
                for i in 0..self.partials {
                    s += self.amp[i] * (TAU * self.phase[i]).sin();
                    self.phase[i] = wrap(self.phase[i] + f * self.freq_mul[i] / sr);
                    self.amp[i] *= self.decay[i];
                }
                s
            }
            Timbre::Bell => {
                let idx = 3.0 * (-ts / 0.25).exp() + 0.3;
                let m = (TAU * self.phase[1]).sin();
                let s = (TAU * self.phase[0] + idx * m).sin() * (-ts / (1.8 * (261.6 / self.f0).powf(0.3))).exp();
                self.phase[0] = wrap(self.phase[0] + f / sr);
                self.phase[1] = wrap(self.phase[1] + 3.5 * f / sr);
                s
            }
            Timbre::Pluck => {
                let dt = f / sr;
                let x = 0.7 * saw(self.phase[0], dt) + 0.3 * square(self.phase[0], dt);
                self.phase[0] = wrap(self.phase[0] + dt);
                let fc = self.f0 * 1.5 + (self.f0 * 14.0).min(9000.0) * (-ts / 0.12).exp() * (0.4 + 0.6 * self.vel);
                self.svf.run(x, fc, 0.9, sr).0
            }
            Timbre::Bass => {
                let dt = f / sr;
                let x = 0.6 * saw(self.phase[0], dt) + 0.6 * (TAU * self.phase[1]).sin();
                self.phase[0] = wrap(self.phase[0] + dt);
                self.phase[1] = wrap(self.phase[1] + dt);
                let fc = 200.0 + self.f0 * 3.0 + 1800.0 * (-ts / 0.07).exp() * self.vel;
                self.svf.run(x, fc, 1.0, sr).0
            }
            Timbre::Strings => {
                let depth = (0.12 + 0.3 * modwheel) * (ts / 0.4).min(1.0);
                let vib = 2f64.powf(depth * (TAU * 5.2 * ts).sin() / 12.0);
                let mut x = 0.0;
                for (i, cents) in [-8.0f64, 0.0, 7.0].iter().enumerate() {
                    let dt = f * vib * 2f64.powf(cents / 1200.0) / sr;
                    x += saw(self.phase[i], dt);
                    self.phase[i] = wrap(self.phase[i] + dt);
                }
                let fc = (self.f0 * 5.0 + 2200.0).min(9000.0);
                self.svf.run(x / 3.0, fc, 0.6, sr).0
            }
            Timbre::Brass => {
                let mut x = 0.0;
                for (i, cents) in [-5.0f64, 5.0].iter().enumerate() {
                    let dt = f * 2f64.powf(cents / 1200.0) / sr;
                    x += saw(self.phase[i], dt);
                    self.phase[i] = wrap(self.phase[i] + dt);
                }
                let fc =
                    (self.f0 * 1.5 + self.f0 * 7.0 * (1.0 - (-ts / 0.06).exp()) * (0.5 + 0.5 * self.vel)).min(10000.0);
                self.svf.run(x / 2.0, fc, 1.2, sr).0
            }
            Timbre::Lead => {
                let depth = (0.18 + 0.3 * modwheel) * ((ts - 0.25) / 0.3).clamp(0.0, 1.0);
                let vib = 2f64.powf(depth * (TAU * 5.5 * ts).sin() / 12.0);
                let dt = f * vib / sr;
                let x = 0.6 * square(self.phase[0], dt) + 0.4 * saw(self.phase[1], dt);
                self.phase[0] = wrap(self.phase[0] + dt);
                self.phase[1] = wrap(self.phase[1] + dt);
                self.svf.run(x, 3200.0 + self.f0 * 2.0, 1.4, sr).0
            }
            Timbre::Pad => {
                let mut x = 0.0;
                for (i, cents) in [-11.0f64, 11.0].iter().enumerate() {
                    let dt = f * 2f64.powf(cents / 1200.0) / sr;
                    x += saw(self.phase[i], dt);
                    self.phase[i] = wrap(self.phase[i] + dt);
                }
                x += 0.4 * (TAU * self.phase[2]).sin();
                self.phase[2] = wrap(self.phase[2] + 0.5 * f / sr);
                self.svf.run(x / 2.4, 900.0 + self.f0 * 2.5, 0.7, sr).0
            }
            Timbre::Drum(d) => {
                let s = self.drum(d, ts, sr);
                if ts > drum_len(d) {
                    self.done = true;
                }
                s
            }
        };
        self.t += 1;
        if let Some(r) = self.release_at {
            if env < 1e-4 && self.t > r + 8 {
                self.done = true;
            }
        } else if sustain == 0.0 && ts > 0.05 && env < 1e-4 {
            self.done = true;
        }
        if let Timbre::Piano | Timbre::Bell = self.timbre {
            if ts > 0.05 && self.partials > 0 && self.amp[0] < 1e-5 {
                self.done = true;
            }
            if self.timbre == Timbre::Bell && ts > 12.0 {
                self.done = true;
            }
        }
        out * env
    }

    fn drum(&mut self, d: Drum, t: f64, sr: f64) -> f64 {
        let n = self.noise();
        match d {
            Drum::Kick => {
                let f = 48.0 + 120.0 * (-t / 0.025).exp();
                let s = (TAU * self.phase[0]).sin() * (-t / 0.32).exp();
                self.phase[0] = wrap(self.phase[0] + f / sr);
                s + 0.3 * n * (-t / 0.002).exp()
            }
            Drum::Snare => {
                let tone = 0.6 * (TAU * self.phase[0]).sin() * (-t / 0.06).exp()
                    + 0.3 * (TAU * self.phase[1]).sin() * (-t / 0.04).exp();
                self.phase[0] = wrap(self.phase[0] + 185.0 / sr);
                self.phase[1] = wrap(self.phase[1] + 330.0 / sr);
                tone + 0.9 * self.svf.run(n, 1800.0, 0.7, sr).2 * (-t / 0.11).exp()
            }
            Drum::Clap => {
                let mut e = 0.0;
                for k in 0..3 {
                    let dt = t - k as f64 * 0.011;
                    if dt >= 0.0 {
                        e += (-dt / 0.004).exp();
                    }
                }
                if t >= 0.03 {
                    e += 0.8 * (-(t - 0.03) / 0.09).exp();
                }
                self.svf.run(n, 1200.0, 2.0, sr).1 * e * 1.5
            }
            Drum::Rim => {
                let s = (TAU * self.phase[0]).sin() * (-t / 0.008).exp();
                self.phase[0] = wrap(self.phase[0] + 1700.0 / sr);
                s + 0.5 * self.svf.run(n, 3000.0, 0.7, sr).2 * (-t / 0.004).exp()
            }
            Drum::HatClosed => 0.7 * self.svf.run(n, 7000.0, 0.7, sr).2 * (-t / 0.03).exp(),
            Drum::HatOpen => 0.6 * self.svf.run(n, 7000.0, 0.7, sr).2 * (-t / 0.25).exp(),
            Drum::Tom(ft) => {
                let f = ft * (1.0 + 0.5 * (-t / 0.04).exp());
                let s = (TAU * self.phase[0]).sin() * (-t / 0.28).exp();
                self.phase[0] = wrap(self.phase[0] + f / sr);
                s
            }
            Drum::Crash | Drum::Ride => {
                let mut metal = 0.0;
                for (i, f) in [540.0, 800.0, 1070.0, 1430.0].iter().enumerate() {
                    metal += square(self.phase[i], f / sr);
                    self.phase[i] = wrap(self.phase[i] + f / sr);
                }
                let (hp, tau, mt, g) =
                    if d == Drum::Crash { (4500.0, 1.1, 0.6, 0.6) } else { (6000.0, 0.9, 0.6, 0.35) };
                let noise = self.svf.run(n, hp, 0.7, sr).2;
                let m = self.svf2.run(metal * 0.25, 3000.0, 0.7, sr).2;
                g * noise * (-t / tau).exp() + 0.15 * m * (-t / mt).exp()
            }
            Drum::Cowbell => {
                let x = square(self.phase[0], 545.0 / sr) + square(self.phase[1], 815.0 / sr);
                self.phase[0] = wrap(self.phase[0] + 545.0 / sr);
                self.phase[1] = wrap(self.phase[1] + 815.0 / sr);
                0.6 * self.svf.run(x, 800.0, 2.0, sr).1 * (-t / 0.12).exp()
            }
            Drum::Tambourine => {
                let jingle = 0.5 + 0.5 * (TAU * 22.0 * t).sin().abs();
                0.5 * self.svf.run(n, 8000.0, 0.7, sr).2 * (-t / 0.15).exp() * jingle
            }
            Drum::Perc(f) => {
                let s = (TAU * self.phase[0]).sin() * (-t / 0.05).exp();
                self.phase[0] = wrap(self.phase[0] + f / sr);
                s
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Chan {
    program: u8,
    volume: u8,
    expression: u8,
    pan: u8,
    sustain: bool,
    bend: f64,
    bend_range: f64,
    rpn: (u8, u8),
    modwheel: u8,
}

impl Default for Chan {
    fn default() -> Self {
        Chan {
            program: 0,
            volume: 100,
            expression: 127,
            pan: 64,
            sustain: false,
            bend: 0.0,
            bend_range: 2.0,
            rpn: (127, 127),
            modwheel: 0,
        }
    }
}

/// The built-in synth engine (follows General MIDI program changes; channel 10 = drums).
pub struct BuiltinSynth {
    sr: f64,
    clock: u64,
    voices: Vec<Voice>,
    ch: [Chan; 16],
}

impl BuiltinSynth {
    pub fn new(sample_rate: u32) -> BuiltinSynth {
        BuiltinSynth { sr: sample_rate as f64, clock: 0, voices: Vec::new(), ch: [Chan::default(); 16] }
    }

    fn note_on(&mut self, ch: u8, key: u8, vel: u8) {
        let timbre =
            if ch == 9 { Timbre::Drum(drum_for_key(key)) } else { timbre_for_program(self.ch[ch as usize].program) };
        let sr = self.sr;
        if ch == 9 {
            if matches!(timbre, Timbre::Drum(Drum::HatClosed)) {
                for v in self.voices.iter_mut().filter(|v| v.ch == 9 && v.timbre == Timbre::Drum(Drum::HatOpen)) {
                    v.done = true;
                }
            }
        } else {
            for v in self.voices.iter_mut().filter(|v| v.ch == ch && v.key == key) {
                v.release(sr);
            }
        }
        if self.voices.len() >= MAX_VOICES {
            let idx = self
                .voices
                .iter()
                .enumerate()
                .max_by_key(|(_, v)| (v.release_at.is_some(), v.t))
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.voices.remove(idx);
        }
        let seed = self.clock ^ ((key as u64) << 40) ^ ((ch as u64) << 48);
        self.voices.push(Voice::new(timbre, ch, key, vel, sr, seed));
    }

    fn note_off(&mut self, ch: u8, key: u8) {
        if ch == 9 {
            return; // drums are one-shots
        }
        let sustain = self.ch[ch as usize].sustain;
        let sr = self.sr;
        for v in self.voices.iter_mut().filter(|v| v.ch == ch && v.key == key && v.release_at.is_none()) {
            if sustain {
                v.pedal_held = true;
            } else {
                v.release(sr);
            }
        }
    }

    fn controller(&mut self, ch: u8, cc: u8, val: u8) {
        let sr = self.sr;
        let c = &mut self.ch[ch as usize];
        match cc {
            1 => c.modwheel = val,
            7 => c.volume = val,
            10 => c.pan = val,
            11 => c.expression = val,
            64 => {
                c.sustain = val >= 64;
                if !c.sustain {
                    for v in self.voices.iter_mut().filter(|v| v.ch == ch && v.pedal_held) {
                        v.pedal_held = false;
                        v.release(sr);
                    }
                }
            }
            100 => c.rpn.1 = val,
            101 => c.rpn.0 = val,
            6 if c.rpn == (0, 0) => c.bend_range = val as f64,
            120 => self.voices.retain(|v| v.ch != ch),
            121 => {
                let p = c.program;
                *c = Chan { program: p, ..Chan::default() };
            }
            123 => {
                for v in self.voices.iter_mut().filter(|v| v.ch == ch) {
                    v.release(sr);
                }
            }
            _ => {}
        }
    }
}

impl Engine for BuiltinSynth {
    fn message(&mut self, status: u8, channel: u8, data1: u8, data2: u8) {
        let ch = channel & 0x0F;
        match status {
            0x9 if data2 > 0 => self.note_on(ch, data1, data2),
            0x8 | 0x9 => self.note_off(ch, data1),
            0xB => self.controller(ch, data1, data2),
            0xC => self.ch[ch as usize].program = data1 & 0x7F,
            0xE => {
                let raw = ((data2 as i32) << 7 | data1 as i32) - 8192;
                let c = &mut self.ch[ch as usize];
                c.bend = raw as f64 / 8192.0 * c.bend_range;
            }
            _ => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        let n = left.len();
        let sr = self.sr;
        for v in self.voices.iter_mut() {
            let c = self.ch[v.ch as usize];
            let gain = (c.volume as f64 / 127.0).powi(2) * (c.expression as f64 / 127.0).powi(2);
            let theta = c.pan as f64 / 127.0 * PI / 2.0;
            let (gl, gr) = (theta.cos() * std::f64::consts::SQRT_2, theta.sin() * std::f64::consts::SQRT_2);
            let vamp = if v.ch == 9 { 0.5 } else { 0.22 } * v.vel.powf(1.6) * gain;
            let bend = 2f64.powf(c.bend / 12.0);
            let modw = c.modwheel as f64 / 127.0;
            for i in 0..n {
                if v.done {
                    break;
                }
                let s = v.tick(sr, bend, modw) * vamp;
                left[i] += (s * gl) as f32;
                right[i] += (s * gr) as f32;
            }
        }
        self.voices.retain(|v| !v.done);
        self.clock += n as u64;
    }
}

/// Built-in timbre names with the GM program each uses (for docs and `describe`).
pub fn timbres() -> Vec<(&'static str, &'static str)> {
    crate::source::BUILTIN_TIMBRES.iter().map(|(n, p)| (*n, gm::program_name(*p))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_note(program: u8, ch: u8, key: u8, secs: f64) -> Vec<f32> {
        let mut s = BuiltinSynth::new(48000);
        s.message(0xC, ch, program, 0);
        s.message(0x9, ch, key, 100);
        let n = (48000.0 * secs) as usize;
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        s.render(&mut l[..n / 2], &mut r[..n / 2]);
        s.message(0x8, ch, key, 0);
        s.render(&mut l[n / 2..], &mut r[n / 2..]);
        l
    }

    #[test]
    fn every_family_sounds_and_ends() {
        for p in (0..128).step_by(8) {
            let x = render_note(p, 0, 60, 3.0);
            let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(peak > 0.01 && peak < 1.0, "program {p}: peak {peak}");
            assert!(x.iter().all(|v| v.is_finite()));
        }
        for key in [36u8, 38, 42, 46, 49, 51, 56, 39, 45, 70] {
            let x = render_note(0, 9, key, 2.0);
            let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(peak > 0.01, "drum {key}");
        }
    }

    #[test]
    fn pitch_is_right() {
        // organ at A4: strongest bin at 440 Hz
        let x = render_note(16, 0, 69, 1.0);
        let seg = &x[4800..28800];
        let l440 = ferraus_core::testing::tone_level(seg, 48000, 440.0);
        let l415 = ferraus_core::testing::tone_level(seg, 48000, 415.0);
        assert!(l440 > 5.0 * l415);
    }

    #[test]
    fn deterministic() {
        assert_eq!(render_note(0, 9, 38, 0.5), render_note(0, 9, 38, 0.5));
    }
}
