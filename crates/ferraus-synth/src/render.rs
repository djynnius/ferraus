//! Rendering MIDI to audio: a sample-accurate event schedule shared by every engine, the
//! [`Engine`] trait the built-in synth, SoundFont, SFZ (and later plugin) engines
//! implement, and track/channel instrument assignment.

use crate::source::{builtin_program, InstrumentSource, Sf2Preset};
use ferraus_core::{AudioBuffer, Error, Result};
use ferraus_midi::MidiFile;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// A synthesis engine driven by MIDI channel messages.
pub trait Engine: Send {
    /// One channel message: `status` is the high nibble (0x8..=0xE), `channel` 0..=15.
    fn message(&mut self, status: u8, channel: u8, data1: u8, data2: u8);
    /// Render `left.len()` frames, overwriting `left` and `right`.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
}

/// Creates engines for `InstrumentSource::Plugin`; registered by the plugin host crate.
pub type PluginEngineFactory = fn(&InstrumentSource, u32) -> Result<Box<dyn Engine>>;

static PLUGIN_FACTORY: OnceLock<PluginEngineFactory> = OnceLock::new();

/// Register the engine factory for plugin instruments (called once by `ferraus-plugins`).
pub fn set_plugin_engine_factory(f: PluginEngineFactory) -> bool {
    PLUGIN_FACTORY.set(f).is_ok()
}

/// How note velocities are shaped before they reach the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VelocityCurve {
    Linear,
    /// Soft touch: quiet notes come out louder (exponent 0.6).
    Soft,
    /// Hard touch: notes need more velocity to get loud (exponent 1.6).
    Hard,
}

impl VelocityCurve {
    pub fn parse(s: &str) -> Result<VelocityCurve> {
        Ok(match s {
            "linear" => VelocityCurve::Linear,
            "soft" => VelocityCurve::Soft,
            "hard" => VelocityCurve::Hard,
            o => return Err(Error::usage(format!("--velocity-curve must be linear, soft, or hard, not '{o}'"))),
        })
    }
    pub fn apply(self, v: u8) -> u8 {
        let g = match self {
            VelocityCurve::Linear => return v,
            VelocityCurve::Soft => 0.6,
            VelocityCurve::Hard => 1.6,
        };
        (127.0 * (v as f64 / 127.0).powf(g)).round().clamp(1.0, 127.0) as u8
    }
}

/// Which part of the file an assignment covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    /// 0-based track index.
    Track(usize),
    /// 0-based channel.
    Channel(u8),
}

#[derive(Clone, Debug, Serialize)]
pub struct RenderOptions {
    pub sample_rate: u32,
    /// Seconds rendered after the MIDI file ends so releases ring.
    pub tail_s: f64,
    pub velocity_curve: VelocityCurve,
    /// Per-track or per-channel instruments (`--map`, `--swap`); the first matching
    /// track assignment wins, then the first matching channel assignment.
    pub assignments: Vec<(Selector, InstrumentSource)>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { sample_rate: 48000, tail_s: 2.0, velocity_curve: VelocityCurve::Linear, assignments: vec![] }
    }
}

/// One scheduled channel message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Scheduled {
    pub sample: u64,
    /// Ordering within a sample: 0 note-off, 1 controller/program/bend, 2 note-on.
    pub rank: u8,
    pub status: u8,
    pub channel: u8,
    pub data1: u8,
    pub data2: u8,
    /// Source track (0-based).
    pub track: usize,
}

/// Every channel message of the file at its output sample (sorted). Note-offs land at
/// least one sample after their note-on so zero-length notes still sound.
pub fn schedule(midi: &MidiFile, sample_rate: u32) -> Vec<Scheduled> {
    let map = midi.tempo_map();
    let sr = sample_rate as f64;
    let at = |tick: u64| (map.tick_to_seconds(tick as f64) * sr).round().max(0.0) as u64;
    let mut v = Vec::new();
    for (ti, t) in midi.tracks.iter().enumerate() {
        for n in &t.notes {
            let s = at(n.tick);
            let e = at(n.end()).max(s + 1);
            v.push(Scheduled {
                sample: s,
                rank: 2,
                status: 0x9,
                channel: n.channel,
                data1: n.pitch,
                data2: n.velocity,
                track: ti,
            });
            v.push(Scheduled {
                sample: e,
                rank: 0,
                status: 0x8,
                channel: n.channel,
                data1: n.pitch,
                data2: n.off_velocity,
                track: ti,
            });
        }
        for e in &t.controllers {
            v.push(Scheduled {
                sample: at(e.tick),
                rank: 1,
                status: 0xB,
                channel: e.data.channel,
                data1: e.data.controller,
                data2: e.data.value,
                track: ti,
            });
        }
        for e in &t.programs {
            v.push(Scheduled {
                sample: at(e.tick),
                rank: 1,
                status: 0xC,
                channel: e.data.channel,
                data1: e.data.program,
                data2: 0,
                track: ti,
            });
        }
        for e in &t.pitch_bends {
            let raw = (e.data.value as i32 + 8192).clamp(0, 16383) as u16;
            v.push(Scheduled {
                sample: at(e.tick),
                rank: 1,
                status: 0xE,
                channel: e.data.channel,
                data1: (raw & 0x7F) as u8,
                data2: (raw >> 7) as u8,
                track: ti,
            });
        }
        for e in &t.channel_pressure {
            v.push(Scheduled {
                sample: at(e.tick),
                rank: 1,
                status: 0xD,
                channel: e.data.channel,
                data1: e.data.value,
                data2: 0,
                track: ti,
            });
        }
        for e in &t.poly_pressure {
            v.push(Scheduled {
                sample: at(e.tick),
                rank: 1,
                status: 0xA,
                channel: e.data.channel,
                data1: e.data.pitch,
                data2: e.data.value,
                track: ti,
            });
        }
        for e in &t.raw {
            // unpaired note-offs are harmless; unpaired note-ons would hang, so skip them
            if e.data.status == 0x8 || (e.data.status == 0x9 && e.data.data2 == 0) {
                v.push(Scheduled {
                    sample: at(e.tick),
                    rank: 0,
                    status: 0x8,
                    channel: e.data.channel,
                    data1: e.data.data1,
                    data2: 0,
                    track: ti,
                });
            }
        }
    }
    v.sort_by_key(|e| (e.sample, e.rank, e.track, e.channel, e.data1));
    v
}

/// Output length in frames: the MIDI length plus the tail.
pub fn output_frames(midi: &MidiFile, sample_rate: u32, tail_s: f64) -> usize {
    ((midi.duration_seconds() + tail_s.max(0.0)) * sample_rate as f64).round() as usize
}

/// How a source treats the file's program changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Programs {
    Follow,
    /// Force a program (and bank) on melodic channels.
    Force {
        bank: u16,
        program: u8,
    },
    /// Everything plays the drum kit (channel 10).
    Drums {
        program: u8,
    },
    /// Engine ignores programs (SFZ, plugins).
    Ignore,
}

fn program_policy(src: &InstrumentSource) -> Programs {
    match src {
        InstrumentSource::BuiltIn { name } => match builtin_program(name) {
            None => Programs::Follow,
            Some(None) => Programs::Drums { program: 0 },
            Some(Some(p)) => Programs::Force { bank: 0, program: p },
        },
        InstrumentSource::Sf2 { preset, .. } => match preset {
            None => Programs::Follow,
            Some(Sf2Preset { bank: 128, program }) => Programs::Drums { program: *program },
            Some(p) => Programs::Force { bank: p.bank, program: p.program },
        },
        _ => Programs::Ignore,
    }
}

/// A group of events rendered by one engine.
#[derive(Clone, Debug, Serialize)]
pub struct Group {
    pub source: InstrumentSource,
    /// True when the group came from an explicit `--map`/`--swap` (channel 10 follows it).
    pub explicit: bool,
    pub events: Vec<Scheduled>,
    pub notes: usize,
}

/// Split the schedule into engine groups by assignment, applying each source's program
/// policy and the velocity curve.
pub fn plan(midi: &MidiFile, default: &InstrumentSource, opts: &RenderOptions) -> Vec<Group> {
    let events = schedule(midi, opts.sample_rate);
    let mut groups: Vec<Group> = Vec::new();
    let pick = |track: usize, ch: u8| -> (InstrumentSource, bool) {
        for (sel, src) in &opts.assignments {
            if *sel == Selector::Track(track) {
                return (src.clone(), true);
            }
        }
        for (sel, src) in &opts.assignments {
            if *sel == Selector::Channel(ch) {
                return (src.clone(), true);
            }
        }
        (default.clone(), false)
    };
    for e in events {
        let (src, explicit) = pick(e.track, e.channel);
        let gi = match groups.iter().position(|g| g.source == src && g.explicit == explicit) {
            Some(i) => i,
            None => {
                groups.push(Group { source: src, explicit, events: vec![], notes: 0 });
                groups.len() - 1
            }
        };
        groups[gi].events.push(e);
    }
    for g in &mut groups {
        let policy = program_policy(&g.source);
        let used: BTreeSet<u8> = g.events.iter().map(|e| e.channel).collect();
        let mut out: Vec<Scheduled> = Vec::with_capacity(g.events.len() + 8);
        match policy {
            Programs::Follow | Programs::Ignore => out.extend(g.events.iter().copied()),
            Programs::Force { bank, program } => {
                // explicit assignments make channel 10 melodic too: move it to a free channel
                let free = (0u8..16).find(|c| *c != 9 && !used.contains(c)).unwrap_or(15);
                let remap = |c: u8| if g.explicit && c == 9 { free } else { c };
                let affected: BTreeSet<u8> = used.iter().map(|c| remap(*c)).filter(|c| *c != 9).collect();
                for &c in &affected {
                    out.push(Scheduled {
                        sample: 0,
                        rank: 1,
                        status: 0xB,
                        channel: c,
                        data1: 0,
                        data2: (bank & 0x7F) as u8,
                        track: 0,
                    });
                    out.push(Scheduled {
                        sample: 0,
                        rank: 1,
                        status: 0xC,
                        channel: c,
                        data1: program,
                        data2: 0,
                        track: 0,
                    });
                }
                for e in &g.events {
                    let mut e = *e;
                    e.channel = remap(e.channel);
                    let is_prog = e.status == 0xC || (e.status == 0xB && (e.data1 == 0 || e.data1 == 32));
                    if is_prog && affected.contains(&e.channel) {
                        continue;
                    }
                    out.push(e);
                }
            }
            Programs::Drums { program } => {
                out.push(Scheduled { sample: 0, rank: 1, status: 0xB, channel: 9, data1: 0, data2: 0, track: 0 });
                out.push(Scheduled { sample: 0, rank: 1, status: 0xC, channel: 9, data1: program, data2: 0, track: 0 });
                for e in &g.events {
                    let mut e = *e;
                    if e.status == 0xC || (e.status == 0xB && (e.data1 == 0 || e.data1 == 32)) {
                        continue;
                    }
                    e.channel = 9;
                    out.push(e);
                }
            }
        }
        for e in &mut out {
            if e.status == 0x9 {
                e.data2 = opts.velocity_curve.apply(e.data2);
            }
        }
        out.sort_by_key(|e| (e.sample, e.rank));
        g.notes = out.iter().filter(|e| e.status == 0x9).count();
        g.events = out;
    }
    groups
}

/// Build the engine for a source.
pub fn make_engine(src: &InstrumentSource, sample_rate: u32) -> Result<Box<dyn Engine>> {
    match src {
        InstrumentSource::BuiltIn { .. } => Ok(Box::new(crate::builtin::BuiltinSynth::new(sample_rate))),
        InstrumentSource::Sf2 { path, .. } => Ok(Box::new(crate::sf2::Sf2Engine::open(path, sample_rate)?)),
        InstrumentSource::Sfz { path } => Ok(Box::new(crate::sfz::SfzEngine::open(path, sample_rate)?)),
        InstrumentSource::Plugin { name, .. } => match PLUGIN_FACTORY.get() {
            Some(f) => f(src, sample_rate),
            None => Err(Error::unsupported(format!(
                "plugin instruments are not available in this build yet (plugin:\"{name}\"); use a SoundFont, SFZ, or built-in sound"
            ))
            .with_hint("ferraus plugin scan")),
        },
    }
}

/// Render events through an engine into `frames` stereo frames, sample-accurately.
pub fn render_events(
    engine: &mut dyn Engine,
    events: &[Scheduled],
    frames: usize,
    progress: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<(Vec<f32>, Vec<f32>)> {
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    let mut pos = 0usize;
    let mut i = 0usize;
    const CHUNK: usize = 8192;
    while pos < frames {
        // apply every event due at `pos`
        while i < events.len() && events[i].sample as usize <= pos {
            let e = events[i];
            engine.message(e.status, e.channel, e.data1, e.data2);
            i += 1;
        }
        let next = events.get(i).map(|e| e.sample as usize).unwrap_or(usize::MAX);
        let end = next.min(frames).min(pos + CHUNK).max(pos + 1);
        engine.render(&mut l[pos..end], &mut r[pos..end]);
        if end / CHUNK != pos / CHUNK {
            progress(end as f64 / frames as f64)?;
        }
        pos = end;
    }
    Ok((l, r))
}

/// What a render produced.
#[derive(Clone, Debug, Serialize)]
pub struct RenderReport {
    pub groups: usize,
    pub notes: usize,
    pub duration_s: f64,
    pub peak: f32,
    /// Gain applied to avoid clipping (1.0 = none).
    pub safety_gain: f32,
}

/// Render a MIDI file through `instrument` (and per-track/channel assignments).
pub fn render(midi: &MidiFile, instrument: &InstrumentSource, opts: &RenderOptions) -> Result<AudioBuffer> {
    render_with_report(midi, instrument, opts, &mut |_| Ok(())).map(|(b, _)| b)
}

/// [`render`] with a progress/cancel callback (fraction 0..1) and a report.
pub fn render_with_report(
    midi: &MidiFile,
    instrument: &InstrumentSource,
    opts: &RenderOptions,
    progress: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<(AudioBuffer, RenderReport)> {
    if !(8000..=384_000).contains(&opts.sample_rate) {
        return Err(Error::usage(format!("sample rate {} is out of range 8000..384000", opts.sample_rate)));
    }
    let frames = output_frames(midi, opts.sample_rate, opts.tail_s);
    let groups = plan(midi, instrument, opts);
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    let ng = groups.len().max(1) as f64;
    let mut notes = 0;
    for (gi, g) in groups.iter().enumerate() {
        let mut engine = make_engine(&g.source, opts.sample_rate)?;
        let (gl, gr) = render_events(engine.as_mut(), &g.events, frames, &mut |f| progress((gi as f64 + f) / ng))?;
        for (a, b) in l.iter_mut().zip(&gl) {
            *a += *b;
        }
        for (a, b) in r.iter_mut().zip(&gr) {
            *a += *b;
        }
        notes += g.notes;
    }
    let peak = l.iter().chain(r.iter()).fold(0.0f32, |m, x| m.max(x.abs()));
    let safety = if peak > 0.98 { 0.98 / peak } else { 1.0 };
    if safety < 1.0 {
        for x in l.iter_mut().chain(r.iter_mut()) {
            *x *= safety;
        }
    }
    let buf = AudioBuffer::from_planar(opts.sample_rate, vec![l, r]);
    Ok((
        buf,
        RenderReport {
            groups: groups.len(),
            notes,
            duration_s: frames as f64 / opts.sample_rate as f64,
            peak,
            safety_gain: safety,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocity_curves() {
        assert_eq!(VelocityCurve::Linear.apply(64), 64);
        assert!(VelocityCurve::Soft.apply(64) > 64);
        assert!(VelocityCurve::Hard.apply(64) < 64);
        assert_eq!(VelocityCurve::Hard.apply(127), 127);
    }

    #[test]
    fn schedule_and_plan() {
        let f = MidiFile::from_beat_notes(&[(60, 0.0, 0.0, 100, 1), (38, 1.0, 0.5, 100, 10)], 120.0, 480);
        let s = schedule(&f, 48000);
        assert_eq!(s[0].status, 0x9);
        assert_eq!(s[1].sample, 1, "zero-length note-off one sample later");
        // forcing strings keeps drums on channel 10
        let g = plan(&f, &InstrumentSource::builtin("strings"), &RenderOptions::default());
        assert_eq!(g.len(), 1);
        assert!(g[0].events.iter().any(|e| e.status == 0xC && e.data1 == 48 && e.channel == 0));
        assert!(g[0].events.iter().any(|e| e.status == 0x9 && e.channel == 9));
        // an explicit channel-10 assignment moves the drums to a melodic channel
        let opts = RenderOptions {
            assignments: vec![(Selector::Channel(9), InstrumentSource::builtin("strings"))],
            ..Default::default()
        };
        let g = plan(&f, &InstrumentSource::builtin("piano"), &opts);
        assert_eq!(g.len(), 2);
        let strings = g.iter().find(|g| g.explicit).unwrap();
        assert!(strings.events.iter().all(|e| e.channel != 9));
        // drums: everything on channel 10
        let g = plan(&f, &InstrumentSource::builtin("drums"), &RenderOptions::default());
        assert!(g[0].events.iter().filter(|e| e.status == 0x9).all(|e| e.channel == 9));
    }

    #[test]
    fn plugin_is_unsupported_with_hint() {
        let e = make_engine(&InstrumentSource::Plugin { name: "Surge XT".into(), preset: None, state: None }, 48000)
            .err()
            .unwrap();
        assert_eq!(e.kind, ferraus_core::ErrorKind::Unsupported);
        assert_eq!(e.hint.as_deref(), Some("ferraus plugin scan"));
    }
}
