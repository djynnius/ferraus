//! The in-memory MIDI model and its JSON form (the agent-facing representation).
//!
//! The model keeps every event of the file: notes are paired note-on/note-off events,
//! the tempo map, time and key signatures are lifted out of their tracks, and everything
//! else (controllers, programs, bends, pressure, sysex, other meta events, and unpaired
//! note messages) stays per track. Each event remembers its original position (`seq`) so
//! writing an unedited model reproduces the file's events exactly, in order.
//!
//! JSON positions: every event has `tick`, `beat` (quarter notes), and `time_s`. On input,
//! `beat` wins over `tick` when both are present; `time_s` is used only when neither is
//! given. Notes have `duration_beats`, `duration_ticks`, and `duration_s` with the same
//! rule. Channels are 1..=16. `pitch` wins over `name`. `seq` is optional: events without
//! it are ordered canonically within a tick (note-offs, meta, controllers, note-ons).

use crate::gm;
use crate::smf::{data_len, Event, EventKind, RawTrack, Smf, Timing};
use crate::tempo::{bpm_to_us, us_to_bpm, TempoMap};
use ferraus_core::{units, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::Path;

/// Version of the JSON model.
pub const MODEL_VERSION: u32 = 1;

/// An event at a tick with its original order in the track (if known).
#[derive(Clone, Debug, PartialEq)]
pub struct Timed<T> {
    pub tick: u64,
    pub seq: Option<u32>,
    pub data: T,
}

impl<T> Timed<T> {
    pub fn new(tick: u64, data: T) -> Self {
        Timed {
            tick,
            seq: None,
            data,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tempo {
    pub us_per_quarter: u32,
    /// Track the event lives in (0 for format 0/1 files).
    pub track: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimeSig {
    pub numerator: u8,
    /// Actual denominator (4 for x/4).
    pub denominator: u32,
    pub clocks_per_click: u8,
    pub notated_32nds: u8,
    pub track: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeySig {
    pub sharps: i8,
    pub minor: bool,
    pub track: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Controller {
    pub channel: u8,
    pub controller: u8,
    pub value: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub channel: u8,
    pub program: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PitchBend {
    pub channel: u8,
    /// -8192..=8191
    pub value: i16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelPressure {
    pub channel: u8,
    pub value: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PolyPressure {
    pub channel: u8,
    pub pitch: u8,
    pub value: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub kind: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sysex {
    pub escape: bool,
    pub data: Vec<u8>,
}

/// A channel message that is not part of a paired note (an unmatched note-on/off).
#[derive(Clone, Debug, PartialEq)]
pub struct RawChannel {
    pub status: u8,
    pub channel: u8,
    pub data1: u8,
    pub data2: u8,
}

/// A note: a paired note-on and note-off.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub tick: u64,
    pub duration: u64,
    pub pitch: u8,
    pub velocity: u8,
    /// 0..=15
    pub channel: u8,
    pub off_velocity: u8,
    /// The note-off was written as a note-on with velocity 0.
    pub off_as_note_on: bool,
    pub seq: Option<u32>,
    pub off_seq: Option<u32>,
}

impl Note {
    pub fn new(tick: u64, duration: u64, pitch: u8, velocity: u8, channel: u8) -> Note {
        Note {
            tick,
            duration,
            pitch,
            velocity,
            channel,
            off_velocity: 64,
            off_as_note_on: false,
            seq: None,
            off_seq: None,
        }
    }
    pub fn end(&self) -> u64 {
        self.tick + self.duration
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Track {
    pub notes: Vec<Note>,
    pub controllers: Vec<Timed<Controller>>,
    pub programs: Vec<Timed<Program>>,
    pub pitch_bends: Vec<Timed<PitchBend>>,
    pub channel_pressure: Vec<Timed<ChannelPressure>>,
    pub poly_pressure: Vec<Timed<PolyPressure>>,
    pub meta: Vec<Timed<Meta>>,
    pub sysex: Vec<Timed<Sysex>>,
    pub raw: Vec<Timed<RawChannel>>,
    /// Tick of the End-of-Track event.
    pub end_tick: u64,
    pub has_end: bool,
}

impl Track {
    pub fn new() -> Track {
        Track {
            has_end: true,
            ..Default::default()
        }
    }

    /// Track name from the first track-name meta event.
    pub fn name(&self) -> Option<String> {
        self.meta
            .iter()
            .find(|m| m.data.kind == 0x03)
            .map(|m| String::from_utf8_lossy(&m.data.data).to_string())
    }

    pub fn set_name(&mut self, name: &str) {
        if let Some(m) = self.meta.iter_mut().find(|m| m.data.kind == 0x03) {
            if m.data.data != name.as_bytes() {
                m.data.data = name.as_bytes().to_vec();
            }
        } else {
            self.meta.push(Timed::new(
                0,
                Meta {
                    kind: 0x03,
                    data: name.as_bytes().to_vec(),
                },
            ));
        }
    }

    /// Channels (0-based) used by notes and channel events.
    pub fn channels(&self) -> Vec<u8> {
        let mut c: Vec<u8> = self
            .notes
            .iter()
            .map(|n| n.channel)
            .chain(self.controllers.iter().map(|e| e.data.channel))
            .chain(self.programs.iter().map(|e| e.data.channel))
            .chain(self.pitch_bends.iter().map(|e| e.data.channel))
            .collect();
        c.sort();
        c.dedup();
        c
    }

    /// The last tick any event (or note end) reaches.
    pub fn last_event_tick(&self) -> u64 {
        let mut m = 0;
        for n in &self.notes {
            m = m.max(n.end());
        }
        macro_rules! scan {
            ($v:expr) => {
                for e in &$v {
                    m = m.max(e.tick);
                }
            };
        }
        scan!(self.controllers);
        scan!(self.programs);
        scan!(self.pitch_bends);
        scan!(self.channel_pressure);
        scan!(self.poly_pressure);
        scan!(self.meta);
        scan!(self.sysex);
        scan!(self.raw);
        m
    }

    /// Clear every `seq` (after retiming, so events are ordered canonically).
    pub fn clear_seqs(&mut self) {
        for n in &mut self.notes {
            n.seq = None;
            n.off_seq = None;
        }
        macro_rules! clr {
            ($v:expr) => {
                for e in &mut $v {
                    e.seq = None;
                }
            };
        }
        clr!(self.controllers);
        clr!(self.programs);
        clr!(self.pitch_bends);
        clr!(self.channel_pressure);
        clr!(self.poly_pressure);
        clr!(self.meta);
        clr!(self.sysex);
        clr!(self.raw);
    }

    /// Apply `f` to the tick of every event (notes: start and end mapped separately).
    pub fn map_ticks(&mut self, f: &dyn Fn(u64) -> u64) {
        for n in &mut self.notes {
            let s = f(n.tick);
            let e = f(n.end()).max(s);
            n.tick = s;
            n.duration = e - s;
        }
        macro_rules! m {
            ($v:expr) => {
                for e in &mut $v {
                    e.tick = f(e.tick);
                }
            };
        }
        m!(self.controllers);
        m!(self.programs);
        m!(self.pitch_bends);
        m!(self.channel_pressure);
        m!(self.poly_pressure);
        m!(self.meta);
        m!(self.sysex);
        m!(self.raw);
        self.end_tick = f(self.end_tick);
    }

    pub fn is_empty_of_notes(&self) -> bool {
        self.notes.is_empty()
    }
}

/// A complete MIDI file.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiFile {
    pub format: u16,
    pub timing: Timing,
    pub tempos: Vec<Timed<Tempo>>,
    pub time_signatures: Vec<Timed<TimeSig>>,
    pub key_signatures: Vec<Timed<KeySig>>,
    pub tracks: Vec<Track>,
    pub extra_chunks: Vec<([u8; 4], Vec<u8>)>,
}

/// A note given in seconds, for building files and reading notes back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecondsNote {
    pub pitch: u8,
    pub start_s: f64,
    pub duration_s: f64,
    pub velocity: u8,
    /// 1..=16
    #[serde(default = "one")]
    pub channel: u8,
    /// 0-based track index (when reading).
    #[serde(default)]
    pub track: usize,
}

fn one() -> u8 {
    1
}

impl MidiFile {
    /// An empty format-1 file with one empty track.
    pub fn new(ppq: u16, bpm: f64) -> MidiFile {
        MidiFile {
            format: 1,
            timing: Timing::Ppq(ppq),
            tempos: vec![Timed::new(
                0,
                Tempo {
                    us_per_quarter: bpm_to_us(bpm),
                    track: 0,
                },
            )],
            time_signatures: vec![],
            key_signatures: vec![],
            tracks: vec![Track::new()],
            extra_chunks: vec![],
        }
    }

    /// Build a file from notes in seconds at a constant tempo. Notes go into one track per
    /// distinct `track` value (format 1). `program` is written for every channel used.
    pub fn from_notes(notes: &[SecondsNote], bpm: f64, ppq: u16, program: Option<u8>) -> MidiFile {
        let mut f = MidiFile::new(ppq, bpm);
        let map = f.tempo_map();
        let ntracks = notes.iter().map(|n| n.track + 1).max().unwrap_or(1);
        f.tracks = (0..ntracks).map(|_| Track::new()).collect();
        for n in notes {
            let s = map.seconds_to_tick(n.start_s.max(0.0)).round() as u64;
            let e = map
                .seconds_to_tick((n.start_s + n.duration_s).max(0.0))
                .round() as u64;
            let ch = n.channel.clamp(1, 16) - 1;
            f.tracks[n.track].notes.push(Note::new(
                s,
                e.saturating_sub(s).max(1),
                n.pitch.min(127),
                n.velocity.clamp(1, 127),
                ch,
            ));
        }
        for t in &mut f.tracks {
            t.notes.sort_by_key(|n| (n.tick, n.pitch));
            if let Some(p) = program {
                for ch in t.channels() {
                    t.programs.push(Timed::new(
                        0,
                        Program {
                            channel: ch,
                            program: p.min(127),
                        },
                    ));
                }
            }
            t.end_tick = t.last_event_tick();
        }
        f
    }

    /// Build a file from notes positioned in beats: `(pitch, start_beats, duration_beats, velocity, channel 1..=16)`.
    pub fn from_beat_notes(notes: &[(u8, f64, f64, u8, u8)], bpm: f64, ppq: u16) -> MidiFile {
        let mut f = MidiFile::new(ppq, bpm);
        let p = ppq as f64;
        for &(pitch, s, d, v, ch) in notes {
            let st = (s * p).round().max(0.0) as u64;
            let en = ((s + d) * p).round().max(0.0) as u64;
            f.tracks[0].notes.push(Note::new(
                st,
                en.saturating_sub(st),
                pitch.min(127),
                v.clamp(1, 127),
                ch.clamp(1, 16) - 1,
            ));
        }
        f.tracks[0].notes.sort_by_key(|n| (n.tick, n.pitch));
        f.tracks[0].end_tick = f.tracks[0].last_event_tick();
        f
    }

    pub fn ppq(&self) -> Option<u16> {
        match self.timing {
            Timing::Ppq(p) => Some(p),
            _ => None,
        }
    }

    /// Ticks per quarter note (SMPTE files: treated as 120 BPM ticks).
    pub fn ticks_per_beat(&self) -> f64 {
        match self.timing {
            Timing::Ppq(p) => p as f64,
            t => t.smpte_ticks_per_second().unwrap_or(1000.0) / 2.0,
        }
    }

    pub fn tempo_map(&self) -> TempoMap {
        let ch: Vec<(u64, u32)> = self
            .tempos
            .iter()
            .map(|t| (t.tick, t.data.us_per_quarter))
            .collect();
        TempoMap::new(self.timing, &ch)
    }

    /// Length in ticks: the latest End-of-Track or event.
    pub fn duration_ticks(&self) -> u64 {
        let mut m = 0;
        for t in &self.tracks {
            m = m.max(t.end_tick).max(t.last_event_tick());
        }
        for e in &self.tempos {
            m = m.max(e.tick);
        }
        m
    }

    pub fn duration_seconds(&self) -> f64 {
        self.tempo_map().tick_to_seconds(self.duration_ticks() as f64)
    }

    /// Every note in seconds, sorted by start time.
    pub fn notes_seconds(&self) -> Vec<SecondsNote> {
        let map = self.tempo_map();
        let mut v: Vec<SecondsNote> = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(ti, t)| {
                let map = &map;
                t.notes.iter().map(move |n| {
                    let s = map.tick_to_seconds(n.tick as f64);
                    let e = map.tick_to_seconds(n.end() as f64);
                    SecondsNote {
                        pitch: n.pitch,
                        start_s: s,
                        duration_s: e - s,
                        velocity: n.velocity,
                        channel: n.channel + 1,
                        track: ti,
                    }
                })
            })
            .collect();
        v.sort_by(|a, b| {
            a.start_s
                .partial_cmp(&b.start_s)
                .unwrap()
                .then(a.pitch.cmp(&b.pitch))
        });
        v
    }

    pub fn note_count(&self) -> usize {
        self.tracks.iter().map(|t| t.notes.len()).sum()
    }

    /// Clear every `seq` in the file.
    pub fn clear_seqs(&mut self) {
        for t in &mut self.tracks {
            t.clear_seqs();
        }
        for e in &mut self.tempos {
            e.seq = None;
        }
        for e in &mut self.time_signatures {
            e.seq = None;
        }
        for e in &mut self.key_signatures {
            e.seq = None;
        }
    }

    /// Set every track's End-of-Track to its last event (or keep it if later).
    pub fn fit_ends(&mut self) {
        for t in &mut self.tracks {
            t.end_tick = t.end_tick.max(t.last_event_tick());
        }
    }

    /// Change the PPQ, rescaling every tick.
    pub fn set_ppq(&mut self, ppq: u16) {
        let old = match self.timing {
            Timing::Ppq(p) => p,
            _ => {
                self.timing = Timing::Ppq(ppq);
                return;
            }
        };
        if old == ppq || ppq == 0 {
            return;
        }
        let f = move |t: u64| ((t as u128 * ppq as u128 + old as u128 / 2) / old as u128) as u64;
        for t in &mut self.tracks {
            t.map_ticks(&f);
        }
        for e in &mut self.tempos {
            e.tick = f(e.tick);
        }
        for e in &mut self.time_signatures {
            e.tick = f(e.tick);
        }
        for e in &mut self.key_signatures {
            e.tick = f(e.tick);
        }
        self.timing = Timing::Ppq(ppq);
    }

    /// First time signature (default 4/4).
    pub fn time_signature_at(&self, tick: u64) -> (u8, u32) {
        let mut ts = (4u8, 4u32);
        let mut best: Option<u64> = None;
        for e in &self.time_signatures {
            if e.tick <= tick && best.map(|b| e.tick >= b).unwrap_or(true) {
                best = Some(e.tick);
                ts = (e.data.numerator, e.data.denominator);
            }
        }
        ts
    }

    // -----------------------------------------------------------------------------
    // SMF conversion
    // -----------------------------------------------------------------------------

    pub fn from_smf(smf: &Smf) -> MidiFile {
        let mut f = MidiFile {
            format: smf.format,
            timing: smf.timing,
            tempos: vec![],
            time_signatures: vec![],
            key_signatures: vec![],
            tracks: vec![],
            extra_chunks: smf.extra_chunks.clone(),
        };
        for (ti, rt) in smf.tracks.iter().enumerate() {
            let mut t = Track::default();
            // (channel, pitch) -> queue of note indices awaiting a note-off
            let mut pending: HashMap<(u8, u8), VecDeque<usize>> = HashMap::new();
            let mut open: Vec<bool> = Vec::new();
            for (i, e) in rt.events.iter().enumerate() {
                let seq = Some(i as u32);
                let tick = e.tick;
                match &e.kind {
                    EventKind::Meta { kind: 0x2F, .. } => {
                        t.end_tick = tick;
                        t.has_end = true;
                    }
                    EventKind::Meta { kind: 0x51, data } if data.len() == 3 => {
                        let us = u32::from_be_bytes([0, data[0], data[1], data[2]]);
                        if us == 0 {
                            t.meta.push(Timed {
                                tick,
                                seq,
                                data: Meta {
                                    kind: 0x51,
                                    data: data.clone(),
                                },
                            });
                        } else {
                            f.tempos.push(Timed {
                                tick,
                                seq,
                                data: Tempo {
                                    us_per_quarter: us,
                                    track: ti,
                                },
                            });
                        }
                    }
                    EventKind::Meta { kind: 0x58, data } if data.len() == 4 && data[1] < 32 => {
                        f.time_signatures.push(Timed {
                            tick,
                            seq,
                            data: TimeSig {
                                numerator: data[0],
                                denominator: 1u32 << data[1],
                                clocks_per_click: data[2],
                                notated_32nds: data[3],
                                track: ti,
                            },
                        });
                    }
                    EventKind::Meta { kind: 0x59, data }
                        if data.len() == 2 && (data[1] == 0 || data[1] == 1) =>
                    {
                        f.key_signatures.push(Timed {
                            tick,
                            seq,
                            data: KeySig {
                                sharps: data[0] as i8,
                                minor: data[1] == 1,
                                track: ti,
                            },
                        });
                    }
                    EventKind::Meta { kind, data } => t.meta.push(Timed {
                        tick,
                        seq,
                        data: Meta {
                            kind: *kind,
                            data: data.clone(),
                        },
                    }),
                    EventKind::Sysex { escape, data } => t.sysex.push(Timed {
                        tick,
                        seq,
                        data: Sysex {
                            escape: *escape,
                            data: data.clone(),
                        },
                    }),
                    EventKind::Channel {
                        status,
                        channel,
                        data1,
                        data2,
                    } => {
                        let (ch, d1, d2) = (*channel, *data1, *data2);
                        match status {
                            0x9 if d2 > 0 => {
                                pending.entry((ch, d1)).or_default().push_back(t.notes.len());
                                open.push(true);
                                t.notes.push(Note {
                                    tick,
                                    duration: 0,
                                    pitch: d1,
                                    velocity: d2,
                                    channel: ch,
                                    off_velocity: 0,
                                    off_as_note_on: false,
                                    seq,
                                    off_seq: None,
                                });
                            }
                            0x8 | 0x9 => {
                                let q = pending.get_mut(&(ch, d1)).and_then(|q| q.pop_front());
                                match q {
                                    Some(ni) => {
                                        let n = &mut t.notes[ni];
                                        n.duration = tick - n.tick;
                                        n.off_velocity = if *status == 0x9 { 0 } else { d2 };
                                        n.off_as_note_on = *status == 0x9;
                                        n.off_seq = seq;
                                        open[ni] = false;
                                    }
                                    None => t.raw.push(Timed {
                                        tick,
                                        seq,
                                        data: RawChannel {
                                            status: *status,
                                            channel: ch,
                                            data1: d1,
                                            data2: d2,
                                        },
                                    }),
                                }
                            }
                            0xA => t.poly_pressure.push(Timed {
                                tick,
                                seq,
                                data: PolyPressure {
                                    channel: ch,
                                    pitch: d1,
                                    value: d2,
                                },
                            }),
                            0xB => t.controllers.push(Timed {
                                tick,
                                seq,
                                data: Controller {
                                    channel: ch,
                                    controller: d1,
                                    value: d2,
                                },
                            }),
                            0xC => t.programs.push(Timed {
                                tick,
                                seq,
                                data: Program {
                                    channel: ch,
                                    program: d1,
                                },
                            }),
                            0xD => t.channel_pressure.push(Timed {
                                tick,
                                seq,
                                data: ChannelPressure {
                                    channel: ch,
                                    value: d1,
                                },
                            }),
                            _ => t.pitch_bends.push(Timed {
                                tick,
                                seq,
                                data: PitchBend {
                                    channel: ch,
                                    value: ((d2 as i16) << 7 | d1 as i16) - 8192,
                                },
                            }),
                        }
                    }
                }
            }
            // unmatched note-ons stay as raw messages
            let mut keep = Vec::with_capacity(t.notes.len());
            for (i, n) in t.notes.drain(..).enumerate() {
                if open[i] {
                    t.raw.push(Timed {
                        tick: n.tick,
                        seq: n.seq,
                        data: RawChannel {
                            status: 0x9,
                            channel: n.channel,
                            data1: n.pitch,
                            data2: n.velocity,
                        },
                    });
                } else {
                    keep.push(n);
                }
            }
            t.notes = keep;
            if !t.has_end {
                t.end_tick = rt.events.last().map(|e| e.tick).unwrap_or(0);
            }
            f.tracks.push(t);
        }
        f
    }

    pub fn to_smf(&self) -> Smf {
        let ntracks = self.tracks.len().max(1);
        // (tick, seq, rank, insertion, kind)
        let mut per: Vec<Vec<(u64, Option<u32>, u8, usize, EventKind)>> =
            (0..ntracks).map(|_| Vec::new()).collect();
        let tr = |t: usize| t.min(ntracks - 1);
        for e in &self.tempos {
            let us = e.data.us_per_quarter.min(0xFF_FFFF);
            per[tr(e.data.track)].push((
                e.tick,
                e.seq,
                1,
                0,
                EventKind::Meta {
                    kind: 0x51,
                    data: us.to_be_bytes()[1..].to_vec(),
                },
            ));
        }
        for e in &self.time_signatures {
            let d = e.data.denominator.max(1).trailing_zeros() as u8;
            per[tr(e.data.track)].push((
                e.tick,
                e.seq,
                1,
                0,
                EventKind::Meta {
                    kind: 0x58,
                    data: vec![
                        e.data.numerator,
                        d,
                        e.data.clocks_per_click,
                        e.data.notated_32nds,
                    ],
                },
            ));
        }
        for e in &self.key_signatures {
            per[tr(e.data.track)].push((
                e.tick,
                e.seq,
                1,
                0,
                EventKind::Meta {
                    kind: 0x59,
                    data: vec![e.data.sharps as u8, e.data.minor as u8],
                },
            ));
        }
        for (ti, t) in self.tracks.iter().enumerate() {
            let v = &mut per[ti];
            let ch = |status: u8, channel: u8, data1: u8, data2: u8| EventKind::Channel {
                status,
                channel: channel & 0x0F,
                data1: data1 & 0x7F,
                data2: data2 & 0x7F,
            };
            for n in &t.notes {
                v.push((
                    n.tick,
                    n.seq,
                    3,
                    0,
                    ch(0x9, n.channel, n.pitch, n.velocity.max(1)),
                ));
                let off = if n.off_as_note_on {
                    ch(0x9, n.channel, n.pitch, 0)
                } else {
                    ch(0x8, n.channel, n.pitch, n.off_velocity)
                };
                v.push((n.end(), n.off_seq, if n.duration == 0 { 4 } else { 0 }, 0, off));
            }
            for e in &t.controllers {
                v.push((
                    e.tick,
                    e.seq,
                    2,
                    0,
                    ch(0xB, e.data.channel, e.data.controller, e.data.value),
                ));
            }
            for e in &t.programs {
                v.push((e.tick, e.seq, 2, 0, ch(0xC, e.data.channel, e.data.program, 0)));
            }
            for e in &t.pitch_bends {
                let raw = (e.data.value.clamp(-8192, 8191) + 8192) as u16;
                v.push((
                    e.tick,
                    e.seq,
                    2,
                    0,
                    ch(0xE, e.data.channel, (raw & 0x7F) as u8, (raw >> 7) as u8),
                ));
            }
            for e in &t.channel_pressure {
                v.push((e.tick, e.seq, 2, 0, ch(0xD, e.data.channel, e.data.value, 0)));
            }
            for e in &t.poly_pressure {
                v.push((
                    e.tick,
                    e.seq,
                    2,
                    0,
                    ch(0xA, e.data.channel, e.data.pitch, e.data.value),
                ));
            }
            for e in &t.meta {
                v.push((
                    e.tick,
                    e.seq,
                    1,
                    0,
                    EventKind::Meta {
                        kind: e.data.kind,
                        data: e.data.data.clone(),
                    },
                ));
            }
            for e in &t.sysex {
                v.push((
                    e.tick,
                    e.seq,
                    1,
                    0,
                    EventKind::Sysex {
                        escape: e.data.escape,
                        data: e.data.data.clone(),
                    },
                ));
            }
            for e in &t.raw {
                let d = &e.data;
                let rank = if d.status == 0x8 || (d.status == 0x9 && d.data2 == 0) {
                    0
                } else if d.status == 0x9 {
                    3
                } else {
                    2
                };
                v.push((
                    e.tick,
                    e.seq,
                    rank,
                    0,
                    ch(d.status, d.channel, d.data1, if data_len(d.status) == 2 { d.data2 } else { 0 }),
                ));
            }
        }
        let mut tracks = Vec::with_capacity(ntracks);
        for (ti, mut v) in per.into_iter().enumerate() {
            for (i, e) in v.iter_mut().enumerate() {
                e.3 = i;
            }
            v.sort_by_key(|e| e.0);
            let mut out: Vec<Event> = Vec::with_capacity(v.len() + 1);
            let mut i = 0;
            while i < v.len() {
                let mut j = i;
                while j < v.len() && v[j].0 == v[i].0 {
                    j += 1;
                }
                let group = &mut v[i..j];
                if group.iter().all(|e| e.1.is_some()) {
                    group.sort_by_key(|e| (e.1, e.3));
                } else {
                    group.sort_by_key(|e| (e.2, e.1.unwrap_or(u32::MAX), e.3));
                }
                for e in group.iter() {
                    out.push(Event {
                        tick: e.0,
                        kind: e.4.clone(),
                    });
                }
                i = j;
            }
            let (end_tick, has_end) = match self.tracks.get(ti) {
                Some(t) => (t.end_tick, t.has_end),
                None => (0, true),
            };
            if has_end {
                let last = out.last().map(|e| e.tick).unwrap_or(0);
                out.push(Event {
                    tick: end_tick.max(last),
                    kind: EventKind::Meta {
                        kind: 0x2F,
                        data: vec![],
                    },
                });
            }
            tracks.push(RawTrack { events: out });
        }
        let format = if tracks.len() > 1 && self.format == 0 {
            1
        } else {
            self.format
        };
        Smf {
            format,
            timing: self.timing,
            tracks,
            extra_chunks: self.extra_chunks.clone(),
        }
    }

    /// Parse SMF bytes.
    pub fn from_smf_bytes(bytes: &[u8]) -> Result<MidiFile> {
        Ok(MidiFile::from_smf(&Smf::parse(bytes)?))
    }

    pub fn to_smf_bytes(&self) -> Vec<u8> {
        self.to_smf().to_bytes()
    }

    /// Parse either SMF bytes or the JSON model (detected from the content).
    pub fn from_bytes(bytes: &[u8]) -> Result<MidiFile> {
        let first = bytes.iter().find(|b| !b.is_ascii_whitespace()).copied();
        if first == Some(b'{') {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| Error::input("MIDI JSON is not valid UTF-8"))?;
            MidiFile::from_json_str(text)
        } else {
            MidiFile::from_smf_bytes(bytes)
        }
    }

    /// Read a `.mid` or `.json` file.
    pub fn read(path: &Path) -> Result<MidiFile> {
        if !path.exists() {
            return Err(Error::new(
                ferraus_core::ErrorKind::InputMissing,
                "input_not_found",
                format!("input not found: {}", path.display()),
            ));
        }
        let bytes = std::fs::read(path)?;
        MidiFile::from_bytes(&bytes).map_err(|mut e| {
            e.message = format!("{}: {}", path.display(), e.message);
            e
        })
    }

    // -----------------------------------------------------------------------------
    // JSON conversion
    // -----------------------------------------------------------------------------

    pub fn to_json_model(&self) -> MidiJson {
        let map = self.tempo_map();
        let at = |tick: u64, seq: Option<u32>| AtJson {
            tick: Some(tick),
            beat: map.tick_to_beats(tick as f64),
            time_s: Some(map.tick_to_seconds(tick as f64)),
            seq,
        };
        let dur = self.duration_ticks();
        let mut tracks = Vec::new();
        for (ti, t) in self.tracks.iter().enumerate() {
            let mut notes: Vec<NoteJson> = t
                .notes
                .iter()
                .map(|n| {
                    let s = map.tick_to_seconds(n.tick as f64);
                    let e = map.tick_to_seconds(n.end() as f64);
                    NoteJson {
                        at: at(n.tick, n.seq),
                        duration_beats: map.tick_to_beats(n.duration as f64),
                        duration_ticks: Some(n.duration),
                        duration_s: Some(e - s),
                        pitch: Some(n.pitch),
                        name: Some(units::midi_to_name(n.pitch as i32)),
                        velocity: n.velocity,
                        channel: n.channel + 1,
                        off_velocity: if n.off_as_note_on { None } else { Some(n.off_velocity) },
                        off_note_on: n.off_as_note_on,
                        off_seq: n.off_seq,
                    }
                })
                .collect();
            notes.sort_by_key(|n| (n.at.tick, n.at.seq, n.pitch));
            tracks.push(TrackJson {
                index: Some(ti + 1),
                name: t.name(),
                channels: t.channels().iter().map(|c| c + 1).collect(),
                end_tick: Some(t.end_tick),
                missing_end: !t.has_end,
                notes,
                controllers: t
                    .controllers
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: ControllerJson {
                            channel: e.data.channel + 1,
                            controller: e.data.controller,
                            value: e.data.value,
                            name: gm::controller_name(e.data.controller).map(String::from),
                        },
                    })
                    .collect(),
                program_changes: t
                    .programs
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: ProgramJson {
                            channel: e.data.channel + 1,
                            program: e.data.program,
                            name: Some(if e.data.channel == 9 {
                                "Drum kit".to_string()
                            } else {
                                gm::program_name(e.data.program).to_string()
                            }),
                        },
                    })
                    .collect(),
                pitch_bends: t
                    .pitch_bends
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: PitchBendJson {
                            channel: e.data.channel + 1,
                            value: e.data.value,
                        },
                    })
                    .collect(),
                channel_pressure: t
                    .channel_pressure
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: ChannelPressureJson {
                            channel: e.data.channel + 1,
                            value: e.data.value,
                        },
                    })
                    .collect(),
                poly_pressure: t
                    .poly_pressure
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: PolyPressureJson {
                            channel: e.data.channel + 1,
                            pitch: e.data.pitch,
                            value: e.data.value,
                        },
                    })
                    .collect(),
                meta: t
                    .meta
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: MetaJson::from_meta(&e.data),
                    })
                    .collect(),
                sysex: t
                    .sysex
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: SysexJson {
                            escape: e.data.escape,
                            data: to_hex(&e.data.data),
                        },
                    })
                    .collect(),
                raw: t
                    .raw
                    .iter()
                    .map(|e| TimedJson {
                        at: at(e.tick, e.seq),
                        data: RawJson {
                            status: format!("{:X}", e.data.status),
                            channel: e.data.channel + 1,
                            data1: e.data.data1,
                            data2: e.data.data2,
                        },
                    })
                    .collect(),
            });
        }
        MidiJson {
            ferraus_midi: MODEL_VERSION,
            format: self.format,
            ppq: self.ppq(),
            smpte: match self.timing {
                Timing::Smpte {
                    fps,
                    ticks_per_frame,
                } => Some(SmpteJson {
                    fps,
                    ticks_per_frame,
                }),
                _ => None,
            },
            duration: Some(DurationJson {
                ticks: dur,
                beats: map.tick_to_beats(dur as f64),
                seconds: map.tick_to_seconds(dur as f64),
            }),
            tempo_map: self
                .tempos
                .iter()
                .map(|e| TimedJson {
                    at: at(e.tick, e.seq),
                    data: TempoJson {
                        bpm: us_to_bpm(e.data.us_per_quarter),
                        track: e.data.track,
                    },
                })
                .collect(),
            time_signatures: self
                .time_signatures
                .iter()
                .map(|e| TimedJson {
                    at: at(e.tick, e.seq),
                    data: TimeSigJson {
                        numerator: e.data.numerator,
                        denominator: e.data.denominator,
                        clocks_per_click: Some(e.data.clocks_per_click),
                        notated_32nds: Some(e.data.notated_32nds),
                        track: e.data.track,
                    },
                })
                .collect(),
            key_signatures: self
                .key_signatures
                .iter()
                .map(|e| TimedJson {
                    at: at(e.tick, e.seq),
                    data: KeySigJson {
                        sharps: Some(e.data.sharps),
                        minor: e.data.minor,
                        key: Some(gm::key_name(e.data.sharps, e.data.minor)),
                        track: e.data.track,
                    },
                })
                .collect(),
            tracks,
            extra_chunks: self
                .extra_chunks
                .iter()
                .map(|(id, d)| ChunkJson {
                    id: String::from_utf8_lossy(id).to_string(),
                    data: to_hex(d),
                })
                .collect(),
        }
    }

    pub fn to_json_value(&self) -> Value {
        serde_json::to_value(self.to_json_model()).unwrap_or(Value::Null)
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(&self.to_json_model()).unwrap_or_default() + "\n"
    }

    pub fn from_json_str(s: &str) -> Result<MidiFile> {
        let m: MidiJson = serde_json::from_str(s)
            .map_err(|e| Error::input(format!("MIDI JSON does not match the model: {e}")))?;
        MidiFile::from_json_model(&m)
    }

    pub fn from_json_value(v: &Value) -> Result<MidiFile> {
        let m: MidiJson = serde_json::from_value(v.clone())
            .map_err(|e| Error::input(format!("MIDI JSON does not match the model: {e}")))?;
        MidiFile::from_json_model(&m)
    }

    pub fn from_json_model(m: &MidiJson) -> Result<MidiFile> {
        let timing = match (&m.smpte, m.ppq) {
            (Some(s), _) => Timing::Smpte {
                fps: s.fps,
                ticks_per_frame: s.ticks_per_frame,
            },
            (None, Some(p)) if p > 0 && p < 0x8000 => Timing::Ppq(p),
            (None, Some(p)) => return Err(Error::input(format!("ppq {p} is out of range 1..32767"))),
            (None, None) => Timing::Ppq(960),
        };
        // tempo map first, resolved in order so seconds positions can be used
        let mut tempos: Vec<Timed<Tempo>> = Vec::new();
        for (i, e) in m.tempo_map.iter().enumerate() {
            let partial = TempoMap::new(
                timing,
                &tempos
                    .iter()
                    .map(|t| (t.tick, t.data.us_per_quarter))
                    .collect::<Vec<_>>(),
            );
            let tick = resolve_at(&e.at, &partial).map_err(|m| ctxerr(&format!("tempo_map[{i}]"), m))?;
            if !(e.data.bpm.is_finite() && e.data.bpm > 0.0) {
                return Err(Error::input(format!("tempo_map[{i}]: bpm must be positive")));
            }
            tempos.push(Timed {
                tick,
                seq: e.at.seq,
                data: Tempo {
                    us_per_quarter: bpm_to_us(e.data.bpm),
                    track: e.data.track,
                },
            });
        }
        let mut f = MidiFile {
            format: m.format,
            timing,
            tempos,
            time_signatures: vec![],
            key_signatures: vec![],
            tracks: vec![],
            extra_chunks: vec![],
        };
        if f.format > 2 {
            return Err(Error::input("format must be 0, 1, or 2"));
        }
        let map = f.tempo_map();
        let pos = |at: &AtJson, what: &str| resolve_at(at, &map).map_err(|m| ctxerr(what, m));
        for (i, e) in m.time_signatures.iter().enumerate() {
            let w = format!("time_signatures[{i}]");
            let d = e.data.denominator;
            if d == 0 || !d.is_power_of_two() || d > 1 << 16 {
                return Err(Error::input(format!("{w}: denominator must be a power of two")));
            }
            f.time_signatures.push(Timed {
                tick: pos(&e.at, &w)?,
                seq: e.at.seq,
                data: TimeSig {
                    numerator: e.data.numerator,
                    denominator: d,
                    clocks_per_click: e.data.clocks_per_click.unwrap_or(24),
                    notated_32nds: e.data.notated_32nds.unwrap_or(8),
                    track: e.data.track,
                },
            });
        }
        for (i, e) in m.key_signatures.iter().enumerate() {
            let w = format!("key_signatures[{i}]");
            let (sharps, minor) = match (e.data.sharps, &e.data.key) {
                (Some(s), _) => (s, e.data.minor),
                (None, Some(k)) => gm::parse_key_name(k)
                    .ok_or_else(|| Error::input(format!("{w}: unknown key '{k}'")))?,
                (None, None) => return Err(Error::input(format!("{w}: needs sharps or key"))),
            };
            f.key_signatures.push(Timed {
                tick: pos(&e.at, &w)?,
                seq: e.at.seq,
                data: KeySig {
                    sharps,
                    minor,
                    track: e.data.track,
                },
            });
        }
        for (ti, tj) in m.tracks.iter().enumerate() {
            let tw = format!("tracks[{ti}]");
            let mut t = Track::new();
            for (i, n) in tj.notes.iter().enumerate() {
                let w = format!("{tw}.notes[{i}]");
                let tick = pos(&n.at, &w)?;
                let duration = if let Some(b) = n.duration_beats {
                    if b < 0.0 || !b.is_finite() {
                        return Err(Error::input(format!("{w}: negative duration")));
                    }
                    map.beats_to_tick(b).round() as u64
                } else if let Some(d) = n.duration_ticks {
                    d
                } else if let Some(s) = n.duration_s {
                    if s < 0.0 || !s.is_finite() {
                        return Err(Error::input(format!("{w}: negative duration")));
                    }
                    map.seconds_to_ticks_at(tick as f64, s).round().max(0.0) as u64
                } else {
                    return Err(Error::input(format!(
                        "{w}: needs duration_beats, duration_ticks, or duration_s"
                    )));
                };
                let pitch = match (n.pitch, &n.name) {
                    (Some(p), _) => p,
                    (None, Some(name)) => units::parse_note_name(name)
                        .filter(|p| (0..128).contains(p))
                        .ok_or_else(|| Error::input(format!("{w}: unknown note name '{name}'")))?
                        as u8,
                    (None, None) => return Err(Error::input(format!("{w}: needs pitch or name"))),
                };
                check(pitch <= 127, &w, "pitch must be 0..127")?;
                check((1..=127).contains(&n.velocity), &w, "velocity must be 1..127")?;
                check((1..=16).contains(&n.channel), &w, "channel must be 1..16")?;
                let ov = n.off_velocity.unwrap_or(if n.off_note_on { 0 } else { 64 });
                check(ov <= 127, &w, "off_velocity must be 0..127")?;
                t.notes.push(Note {
                    tick,
                    duration,
                    pitch,
                    velocity: n.velocity,
                    channel: n.channel - 1,
                    off_velocity: ov,
                    off_as_note_on: n.off_note_on,
                    seq: n.at.seq,
                    off_seq: n.off_seq,
                });
            }
            for (i, e) in tj.controllers.iter().enumerate() {
                let w = format!("{tw}.controllers[{i}]");
                check((1..=16).contains(&e.data.channel), &w, "channel must be 1..16")?;
                check(e.data.controller <= 127 && e.data.value <= 127, &w, "values must be 0..127")?;
                t.controllers.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: Controller {
                        channel: e.data.channel - 1,
                        controller: e.data.controller,
                        value: e.data.value,
                    },
                });
            }
            for (i, e) in tj.program_changes.iter().enumerate() {
                let w = format!("{tw}.program_changes[{i}]");
                check((1..=16).contains(&e.data.channel), &w, "channel must be 1..16")?;
                check(e.data.program <= 127, &w, "program must be 0..127")?;
                t.programs.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: Program {
                        channel: e.data.channel - 1,
                        program: e.data.program,
                    },
                });
            }
            for (i, e) in tj.pitch_bends.iter().enumerate() {
                let w = format!("{tw}.pitch_bends[{i}]");
                check((1..=16).contains(&e.data.channel), &w, "channel must be 1..16")?;
                check((-8192..=8191).contains(&e.data.value), &w, "value must be -8192..8191")?;
                t.pitch_bends.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: PitchBend {
                        channel: e.data.channel - 1,
                        value: e.data.value,
                    },
                });
            }
            for (i, e) in tj.channel_pressure.iter().enumerate() {
                let w = format!("{tw}.channel_pressure[{i}]");
                check((1..=16).contains(&e.data.channel), &w, "channel must be 1..16")?;
                check(e.data.value <= 127, &w, "value must be 0..127")?;
                t.channel_pressure.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: ChannelPressure {
                        channel: e.data.channel - 1,
                        value: e.data.value,
                    },
                });
            }
            for (i, e) in tj.poly_pressure.iter().enumerate() {
                let w = format!("{tw}.poly_pressure[{i}]");
                check((1..=16).contains(&e.data.channel), &w, "channel must be 1..16")?;
                check(e.data.pitch <= 127 && e.data.value <= 127, &w, "values must be 0..127")?;
                t.poly_pressure.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: PolyPressure {
                        channel: e.data.channel - 1,
                        pitch: e.data.pitch,
                        value: e.data.value,
                    },
                });
            }
            for (i, e) in tj.meta.iter().enumerate() {
                let w = format!("{tw}.meta[{i}]");
                t.meta.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: e.data.to_meta().map_err(|m| ctxerr(&w, m))?,
                });
            }
            for (i, e) in tj.sysex.iter().enumerate() {
                let w = format!("{tw}.sysex[{i}]");
                t.sysex.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: Sysex {
                        escape: e.data.escape,
                        data: from_hex(&e.data.data).map_err(|m| ctxerr(&w, m))?,
                    },
                });
            }
            for (i, e) in tj.raw.iter().enumerate() {
                let w = format!("{tw}.raw[{i}]");
                let status = u8::from_str_radix(e.data.status.trim_start_matches("0x"), 16)
                    .ok()
                    .filter(|s| (0x8..=0xE).contains(s))
                    .ok_or_else(|| Error::input(format!("{w}: status must be 8..E (hex)")))?;
                check((1..=16).contains(&e.data.channel), &w, "channel must be 1..16")?;
                check(e.data.data1 <= 127 && e.data.data2 <= 127, &w, "data must be 0..127")?;
                t.raw.push(Timed {
                    tick: pos(&e.at, &w)?,
                    seq: e.at.seq,
                    data: RawChannel {
                        status,
                        channel: e.data.channel - 1,
                        data1: e.data.data1,
                        data2: e.data.data2,
                    },
                });
            }
            if let Some(name) = &tj.name {
                if t.name().as_deref() != Some(name.as_str()) {
                    t.set_name(name);
                }
            }
            t.has_end = !tj.missing_end;
            let last = t.last_event_tick();
            t.end_tick = tj.end_tick.unwrap_or(last).max(last);
            f.tracks.push(t);
        }
        if f.tracks.is_empty() {
            f.tracks.push(Track::new());
        }
        for c in &m.extra_chunks {
            let id = c.id.as_bytes();
            if id.len() != 4 {
                return Err(Error::input("extra_chunks: id must be 4 characters"));
            }
            f.extra_chunks
                .push((id.try_into().unwrap(), from_hex(&c.data).map_err(Error::input)?));
        }
        Ok(f)
    }
}

fn check(ok: bool, what: &str, msg: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::input(format!("{what}: {msg}")))
    }
}

fn ctxerr(what: &str, msg: impl std::fmt::Display) -> Error {
    Error::input(format!("{what}: {msg}"))
}

fn resolve_at(at: &AtJson, map: &TempoMap) -> std::result::Result<u64, String> {
    if let Some(b) = at.beat {
        if !b.is_finite() || b < 0.0 {
            return Err("beat must be a non-negative number".into());
        }
        if map.ppq().is_some() {
            return Ok(map.beats_to_tick(b).round() as u64);
        }
    }
    if let Some(t) = at.tick {
        return Ok(t);
    }
    if let Some(s) = at.time_s {
        if !s.is_finite() || s < 0.0 {
            return Err("time_s must be a non-negative number".into());
        }
        return Ok(map.seconds_to_tick(s).round() as u64);
    }
    if let Some(b) = at.beat {
        return Ok(map.beats_to_tick(b).round() as u64);
    }
    Err("needs beat, tick, or time_s".into())
}

pub fn to_hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 3);
    for (i, x) in b.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn from_hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() % 2 != 0 {
        return Err("hex data must have an even number of digits".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|_| format!("bad hex '{s}'")))
        .collect()
}

// ---------------------------------------------------------------------------------
// JSON wire types
// ---------------------------------------------------------------------------------

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Position of an event. On input `beat` wins over `tick`; `time_s` is a fallback.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AtJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_s: Option<f64>,
    /// Original order within the track; optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimedJson<T> {
    #[serde(flatten)]
    pub at: AtJson,
    #[serde(flatten)]
    pub data: T,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MidiJson {
    #[serde(default = "model_version")]
    pub ferraus_midi: u32,
    #[serde(default = "fmt1")]
    pub format: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppq: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smpte: Option<SmpteJson>,
    /// Informational; ignored on input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<DurationJson>,
    #[serde(default)]
    pub tempo_map: Vec<TimedJson<TempoJson>>,
    #[serde(default)]
    pub time_signatures: Vec<TimedJson<TimeSigJson>>,
    #[serde(default)]
    pub key_signatures: Vec<TimedJson<KeySigJson>>,
    #[serde(default)]
    pub tracks: Vec<TrackJson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_chunks: Vec<ChunkJson>,
}

fn model_version() -> u32 {
    MODEL_VERSION
}
fn fmt1() -> u16 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SmpteJson {
    pub fps: u8,
    pub ticks_per_frame: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DurationJson {
    pub ticks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats: Option<f64>,
    pub seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TempoJson {
    pub bpm: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub track: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimeSigJson {
    pub numerator: u8,
    pub denominator: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clocks_per_click: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notated_32nds: Option<u8>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub track: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct KeySigJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharps: Option<i8>,
    #[serde(default)]
    pub minor: bool,
    /// `Eb major`; used on input only when `sharps` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub track: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrackJson {
    /// 1-based, informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Informational.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_tick: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing_end: bool,
    #[serde(default)]
    pub notes: Vec<NoteJson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controllers: Vec<TimedJson<ControllerJson>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub program_changes: Vec<TimedJson<ProgramJson>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pitch_bends: Vec<TimedJson<PitchBendJson>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_pressure: Vec<TimedJson<ChannelPressureJson>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poly_pressure: Vec<TimedJson<PolyPressureJson>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meta: Vec<TimedJson<MetaJson>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sysex: Vec<TimedJson<SysexJson>>,
    /// Unpaired note-on/off messages, kept so nothing is lost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<TimedJson<RawJson>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct NoteJson {
    #[serde(flatten)]
    pub at: AtJson,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_beats: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ticks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub velocity: u8,
    #[serde(default = "one")]
    pub channel: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_velocity: Option<u8>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub off_note_on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_seq: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ControllerJson {
    pub channel: u8,
    pub controller: u8,
    pub value: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProgramJson {
    pub channel: u8,
    pub program: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PitchBendJson {
    pub channel: u8,
    pub value: i16,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelPressureJson {
    pub channel: u8,
    pub value: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PolyPressureJson {
    pub channel: u8,
    pub pitch: u8,
    pub value: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SysexJson {
    #[serde(default, skip_serializing_if = "is_false")]
    pub escape: bool,
    /// Hex bytes after the F0/F7 and length.
    pub data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RawJson {
    /// Status nibble in hex (`8` note-off, `9` note-on).
    pub status: String,
    pub channel: u8,
    pub data1: u8,
    #[serde(default)]
    pub data2: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChunkJson {
    pub id: String,
    pub data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MetaJson {
    /// Meta type byte (`3` track name, `5` lyric, `6` marker, ...).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub meta_type: Option<u8>,
    /// Name of the meta type (informational, or used when `type` is absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

const META_NAMES: &[(u8, &str)] = &[
    (0x00, "sequence_number"),
    (0x01, "text"),
    (0x02, "copyright"),
    (0x03, "track_name"),
    (0x04, "instrument_name"),
    (0x05, "lyric"),
    (0x06, "marker"),
    (0x07, "cue_point"),
    (0x08, "program_name"),
    (0x09, "device_name"),
    (0x20, "channel_prefix"),
    (0x21, "port"),
    (0x51, "tempo"),
    (0x54, "smpte_offset"),
    (0x58, "time_signature"),
    (0x59, "key_signature"),
    (0x7F, "sequencer_specific"),
];

impl MetaJson {
    fn from_meta(m: &Meta) -> MetaJson {
        let kind = META_NAMES
            .iter()
            .find(|(k, _)| *k == m.kind)
            .map(|(_, n)| n.to_string());
        let text = if (0x01..=0x0F).contains(&m.kind) {
            std::str::from_utf8(&m.data).ok().map(String::from)
        } else {
            None
        };
        MetaJson {
            meta_type: Some(m.kind),
            kind,
            data: if text.is_none() {
                Some(to_hex(&m.data))
            } else {
                None
            },
            text,
        }
    }

    fn to_meta(&self) -> std::result::Result<Meta, String> {
        let kind = match (self.meta_type, &self.kind) {
            (Some(t), _) => t,
            (None, Some(k)) => META_NAMES
                .iter()
                .find(|(_, n)| n == k)
                .map(|(t, _)| *t)
                .ok_or_else(|| format!("unknown meta kind '{k}'"))?,
            (None, None) => return Err("meta needs type or kind".into()),
        };
        if kind == 0x2F {
            return Err("End-of-Track is written automatically; use end_tick".into());
        }
        let data = match (&self.text, &self.data) {
            (Some(t), _) => t.as_bytes().to_vec(),
            (None, Some(d)) => from_hex(d)?,
            (None, None) => vec![],
        };
        Ok(Meta { kind, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_read_notes_seconds() {
        let notes = vec![
            SecondsNote {
                pitch: 60,
                start_s: 0.0,
                duration_s: 0.5,
                velocity: 100,
                channel: 1,
                track: 0,
            },
            SecondsNote {
                pitch: 64,
                start_s: 0.5,
                duration_s: 0.25,
                velocity: 90,
                channel: 1,
                track: 0,
            },
        ];
        let f = MidiFile::from_notes(&notes, 120.0, 480, Some(0));
        assert_eq!(f.tracks[0].notes[1].tick, 480);
        let back = MidiFile::from_smf_bytes(&f.to_smf_bytes()).unwrap();
        let ns = back.notes_seconds();
        assert_eq!(ns.len(), 2);
        assert!((ns[1].start_s - 0.5).abs() < 1e-9);
        assert!((ns[1].duration_s - 0.25).abs() < 1e-9);
        assert!((back.duration_seconds() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn minimal_agent_json() {
        let j = r#"{"ppq": 480, "tempo_map": [{"beat": 0, "bpm": 100}],
            "tracks": [{"name": "Lead", "notes": [
                {"beat": 0, "duration_beats": 1, "name": "C4", "velocity": 100},
                {"time_s": 1.2, "duration_s": 0.3, "pitch": 67, "velocity": 80, "channel": 2}
            ]}]}"#;
        let f = MidiFile::from_json_str(j).unwrap();
        assert_eq!(f.tracks[0].notes[0].pitch, 60);
        assert_eq!(f.tracks[0].notes[0].duration, 480);
        // 1.2 s at 100 BPM = 2 beats = 960 ticks
        assert_eq!(f.tracks[0].notes[1].tick, 960);
        assert_eq!(f.tracks[0].notes[1].channel, 1);
        assert_eq!(f.tracks[0].name().as_deref(), Some("Lead"));
        assert!(MidiFile::from_json_str(r#"{"tracks":[{"notes":[{"beat":0,"velocity":100}]}]}"#).is_err());
        assert!(MidiFile::from_json_str(r#"{"tracks":[{"notes":[{"beat":0,"duration_beats":1,"pitch":60,"velocity":0}]}]}"#).is_err());
    }

    #[test]
    fn beat_wins_over_tick() {
        let f = MidiFile::from_beat_notes(&[(60, 1.0, 1.0, 100, 1)], 120.0, 480);
        let mut v = f.to_json_value();
        v["tracks"][0]["notes"][0]["beat"] = serde_json::json!(2.0);
        let g = MidiFile::from_json_value(&v).unwrap();
        assert_eq!(g.tracks[0].notes[0].tick, 960);
    }

    #[test]
    fn hex() {
        assert_eq!(to_hex(&[0xF0, 0x01]), "f0 01");
        assert_eq!(from_hex("f001").unwrap(), vec![0xF0, 1]);
        assert!(from_hex("f").is_err());
    }
}
