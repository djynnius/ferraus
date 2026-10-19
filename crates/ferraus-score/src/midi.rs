//! Minimal Standard MIDI File reading and writing (via `midly`), with a flat note list,
//! tempo map, time and key signatures.

use ferraus_core::{Error, Result};
use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

#[derive(Clone, Debug, PartialEq)]
pub struct MidiNote {
    /// Track index in the file.
    pub track: usize,
    /// Channel 0..=15.
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct MidiTrack {
    pub name: Option<String>,
    /// Program change seen on this track (first one), with its channel.
    pub program: Option<(u8, u8)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MidiData {
    pub ppq: u32,
    pub notes: Vec<MidiNote>,
    pub tracks: Vec<MidiTrack>,
    /// (tick, microseconds per quarter)
    pub tempos: Vec<(u64, u32)>,
    /// (tick, numerator, denominator)
    pub time_sigs: Vec<(u64, u32, u32)>,
    /// (tick, sharps/flats, minor)
    pub key_sigs: Vec<(u64, i8, bool)>,
    /// (tick, track, text) lyric events
    pub lyrics: Vec<(u64, usize, String)>,
    /// Tick of the end of the last event.
    pub end_tick: u64,
    /// Extra raw events to write (program changes, controller events) per track:
    /// (tick, channel, controller-or-program kind).
    pub programs: Vec<(u64, usize, u8, u8)>,
    /// Click/metronome notes written on channel 10 are just ordinary notes.
    pub title: Option<String>,
}

impl Default for MidiData {
    fn default() -> Self {
        MidiData {
            ppq: 480,
            notes: Vec::new(),
            tracks: Vec::new(),
            tempos: Vec::new(),
            time_sigs: Vec::new(),
            key_sigs: Vec::new(),
            lyrics: Vec::new(),
            end_tick: 0,
            programs: Vec::new(),
            title: None,
        }
    }
}

impl MidiData {
    /// Seconds at a tick through the tempo map.
    pub fn tick_to_seconds(&self, tick: u64) -> f64 {
        let mut tempos = self.tempos.clone();
        tempos.sort_by_key(|t| t.0);
        let mut last_tick = 0u64;
        let mut us_per_q = 500_000f64;
        let mut secs = 0.0;
        for (t, us) in tempos {
            if t >= tick {
                break;
            }
            secs += (t - last_tick) as f64 * us_per_q / 1e6 / self.ppq as f64;
            last_tick = t;
            us_per_q = us as f64;
        }
        secs + (tick - last_tick) as f64 * us_per_q / 1e6 / self.ppq as f64
    }

    pub fn duration_seconds(&self) -> f64 {
        let end = self.notes.iter().map(|n| n.end).max().unwrap_or(0).max(self.end_tick);
        self.tick_to_seconds(end)
    }

    /// The tempo that governs the music: the first tempo, except that a lead-in segment of at
    /// most one beat (as `transcribe` writes to align its beat grid) is skipped.
    pub fn first_bpm(&self) -> Option<f64> {
        let mut t = self.tempos.clone();
        t.sort_by_key(|x| x.0);
        let pick = match t.as_slice() {
            [first, second, ..] if first.0 == 0 && second.0 > 0 && second.0 <= self.ppq as u64 => Some(second),
            _ => t.first(),
        };
        pick.map(|(_, us)| 60_000_000.0 / *us as f64)
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim().to_string()
}

/// Parse a Standard MIDI File.
pub fn read_midi(bytes: &[u8]) -> Result<MidiData> {
    let smf = Smf::parse(bytes).map_err(|e| Error::input(format!("not a valid MIDI file: {e}")))?;
    let ppq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as u32,
        Timing::Timecode(fps, sub) => {
            // convert SMPTE timing to a fixed tempo of 120 bpm with ppq = ticks per half second
            (fps.as_f32() * sub as f32 / 2.0).round().max(1.0) as u32
        }
    };
    let mut d = MidiData { ppq, ..MidiData::default() };
    for (ti, track) in smf.tracks.iter().enumerate() {
        let mut tick = 0u64;
        let mut open: std::collections::HashMap<(u8, u8), Vec<(u64, u8)>> = Default::default();
        let mut tr = MidiTrack::default();
        for ev in track {
            tick += ev.delta.as_int() as u64;
            match ev.kind {
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            open.entry((ch, key.as_int())).or_default().push((tick, vel.as_int()));
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            if let Some(v) = open.get_mut(&(ch, key.as_int())) {
                                if !v.is_empty() {
                                    let (s, vel) = v.remove(0);
                                    d.notes.push(MidiNote {
                                        track: ti,
                                        channel: ch,
                                        key: key.as_int(),
                                        velocity: vel,
                                        start: s,
                                        end: tick.max(s),
                                    });
                                }
                            }
                        }
                        MidiMessage::ProgramChange { program } if tr.program.is_none() => {
                            tr.program = Some((ch, program.as_int()));
                        }
                        _ => {}
                    }
                }
                TrackEventKind::Meta(m) => match m {
                    MetaMessage::Tempo(us) => d.tempos.push((tick, us.as_int())),
                    MetaMessage::TimeSignature(n, dpow, _, _) => {
                        d.time_sigs.push((tick, n as u32, 1u32 << dpow.min(6)))
                    }
                    MetaMessage::KeySignature(sf, minor) => d.key_sigs.push((tick, sf, minor)),
                    MetaMessage::TrackName(b) => {
                        let t = text(b);
                        if ti == 0 && d.title.is_none() && smf.tracks.len() > 1 && !t.is_empty() {
                            d.title = Some(t.clone());
                        }
                        tr.name = Some(t);
                    }
                    MetaMessage::Lyric(b) => d.lyrics.push((tick, ti, text(b))),
                    _ => {}
                },
                _ => {}
            }
        }
        // close hanging notes at the track end
        for ((ch, key), v) in open {
            for (s, vel) in v {
                d.notes.push(MidiNote { track: ti, channel: ch, key, velocity: vel, start: s, end: tick.max(s + 1) });
            }
        }
        d.end_tick = d.end_tick.max(tick);
        d.tracks.push(tr);
    }
    d.notes.sort_by_key(|n| (n.start, n.track, n.key));
    d.tempos.sort_by_key(|t| t.0);
    d.tempos.dedup_by_key(|t| t.0);
    d.time_sigs.sort_by_key(|t| t.0);
    d.key_sigs.sort_by_key(|t| t.0);
    Ok(d)
}

/// Write a format-1 SMF: track 0 holds tempo/time/key meta events; one track per
/// `MidiData::tracks` entry holds its notes.
pub fn write_midi(d: &MidiData) -> Result<Vec<u8>> {
    let ntracks = d.tracks.len().max(d.notes.iter().map(|n| n.track + 1).max().unwrap_or(1));
    // (tick, order, kind)
    let mut conductor: Vec<(u64, u8, TrackEventKind<'_>)> = Vec::new();
    if let Some(t) = &d.title {
        conductor.push((0, 0, TrackEventKind::Meta(MetaMessage::TrackName(t.as_bytes()))));
    }
    for (t, n, den) in &d.time_sigs {
        let dpow = (*den as f64).log2().round() as u8;
        conductor.push((*t, 1, TrackEventKind::Meta(MetaMessage::TimeSignature(*n as u8, dpow, 24, 8))));
    }
    for (t, sf, minor) in &d.key_sigs {
        conductor.push((*t, 2, TrackEventKind::Meta(MetaMessage::KeySignature(*sf, *minor))));
    }
    for (t, us) in &d.tempos {
        conductor.push((*t, 3, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(*us)))));
    }
    let mut tracks: Vec<Vec<TrackEvent<'_>>> = Vec::new();
    tracks.push(finish(conductor, d.end_tick));
    for ti in 0..ntracks {
        let mut evs: Vec<(u64, u8, TrackEventKind<'_>)> = Vec::new();
        if let Some(tr) = d.tracks.get(ti) {
            if let Some(name) = &tr.name {
                evs.push((0, 0, TrackEventKind::Meta(MetaMessage::TrackName(name.as_bytes()))));
            }
            if let Some((ch, prog)) = tr.program {
                evs.push((
                    0,
                    1,
                    TrackEventKind::Midi {
                        channel: u4::new(ch & 15),
                        message: MidiMessage::ProgramChange { program: u7::new(prog & 127) },
                    },
                ));
            }
        }
        for (t, tr, ch, prog) in &d.programs {
            if *tr == ti {
                evs.push((
                    *t,
                    1,
                    TrackEventKind::Midi {
                        channel: u4::new(ch & 15),
                        message: MidiMessage::ProgramChange { program: u7::new(prog & 127) },
                    },
                ));
            }
        }
        for (t, tr, txt) in &d.lyrics {
            if *tr == ti {
                evs.push((*t, 2, TrackEventKind::Meta(MetaMessage::Lyric(txt.as_bytes()))));
            }
        }
        for n in d.notes.iter().filter(|n| n.track == ti) {
            // note-offs sort before note-ons at the same tick (order 2 < 3)
            evs.push((
                n.end,
                2,
                TrackEventKind::Midi {
                    channel: u4::new(n.channel & 15),
                    message: MidiMessage::NoteOff { key: u7::new(n.key & 127), vel: u7::new(0) },
                },
            ));
            evs.push((
                n.start,
                3,
                TrackEventKind::Midi {
                    channel: u4::new(n.channel & 15),
                    message: MidiMessage::NoteOn { key: u7::new(n.key & 127), vel: u7::new(n.velocity.clamp(1, 127)) },
                },
            ));
        }
        tracks.push(finish(evs, d.end_tick));
    }
    let smf =
        Smf { header: Header::new(Format::Parallel, Timing::Metrical(u15::new(d.ppq.min(32767) as u16))), tracks };
    let mut out = Vec::new();
    smf.write_std(&mut out).map_err(|e| Error::processing(format!("cannot write MIDI: {e}")))?;
    Ok(out)
}

fn finish(mut evs: Vec<(u64, u8, TrackEventKind<'_>)>, end: u64) -> Vec<TrackEvent<'_>> {
    evs.sort_by_key(|e| (e.0, e.1));
    let mut out = Vec::with_capacity(evs.len() + 1);
    let mut last = 0u64;
    for (t, _, kind) in evs {
        out.push(TrackEvent { delta: u28::new((t - last) as u32), kind });
        last = t;
    }
    let end = end.max(last);
    out.push(TrackEvent { delta: u28::new((end - last) as u32), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip() {
        let d = MidiData {
            ppq: 480,
            notes: vec![
                MidiNote { track: 0, channel: 0, key: 60, velocity: 90, start: 0, end: 480 },
                MidiNote { track: 0, channel: 0, key: 64, velocity: 80, start: 480, end: 960 },
                MidiNote { track: 1, channel: 9, key: 36, velocity: 100, start: 0, end: 120 },
            ],
            tracks: vec![
                MidiTrack { name: Some("Piano".into()), program: Some((0, 0)) },
                MidiTrack { name: Some("Drums".into()), program: None },
            ],
            tempos: vec![(0, 500_000)],
            time_sigs: vec![(0, 3, 4)],
            key_sigs: vec![(0, 1, false)],
            end_tick: 1440,
            ..MidiData::default()
        };
        let bytes = write_midi(&d).unwrap();
        let r = read_midi(&bytes).unwrap();
        assert_eq!(r.ppq, 480);
        assert_eq!(r.time_sigs, vec![(0, 3, 4)]);
        assert_eq!(r.key_sigs, vec![(0, 1, false)]);
        assert_eq!(r.notes.len(), 3);
        // track 0 of the file is the conductor track
        let n: Vec<(u8, u64, u64)> = r.notes.iter().map(|n| (n.key, n.start, n.end)).collect();
        assert!(n.contains(&(60, 0, 480)) && n.contains(&(64, 480, 960)) && n.contains(&(36, 0, 120)));
        assert_eq!(r.first_bpm(), Some(120.0));
        assert!((r.duration_seconds() - 1.5).abs() < 1e-9);
    }
}
