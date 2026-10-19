//! `to-json` then `from-json` reproduces a MIDI file's events exactly, on programmatically
//! generated files covering running status, several tracks and formats, SMPTE timing,
//! sysex (F0 and F7), every meta type (including malformed tempo/signature data), all
//! channel messages, overlapping and zero-length notes, unmatched note-ons/offs, and
//! unknown chunks. The generator writes bytes with its own encoder (running status on).

use ferraus_midi::smf::{Event, EventKind, RawTrack, Smf, Timing};
use ferraus_midi::MidiFile;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    fn bytes(&mut self, n: usize, max: u8) -> Vec<u8> {
        (0..n).map(|_| (self.below(max as u64 + 1)) as u8).collect()
    }
}

fn vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut stack = vec![(v & 0x7F) as u8];
    v >>= 7;
    while v > 0 {
        stack.push(((v & 0x7F) as u8) | 0x80);
        v >>= 7;
    }
    stack.reverse();
    out.extend(stack);
}

/// Independent encoder that uses running status for channel messages.
fn encode(smf: &Smf) -> Vec<u8> {
    let mut out = b"MThd".to_vec();
    out.extend(6u32.to_be_bytes());
    out.extend(smf.format.to_be_bytes());
    out.extend((smf.tracks.len() as u16).to_be_bytes());
    let div: u16 = match smf.timing {
        Timing::Ppq(p) => p,
        Timing::Smpte { fps, ticks_per_frame } => ((256 - fps as u16) << 8) | ticks_per_frame as u16,
    };
    out.extend(div.to_be_bytes());
    for (i, t) in smf.tracks.iter().enumerate() {
        let mut body = Vec::new();
        let mut last = 0;
        let mut running: Option<u8> = None;
        for e in &t.events {
            vlq(&mut body, (e.tick - last) as u32);
            last = e.tick;
            match &e.kind {
                EventKind::Channel { status, channel, data1, data2 } => {
                    let st = (status << 4) | channel;
                    if running != Some(st) {
                        body.push(st);
                        running = Some(st);
                    }
                    body.push(*data1);
                    if !matches!(status, 0xC | 0xD) {
                        body.push(*data2);
                    }
                }
                EventKind::Sysex { escape, data } => {
                    body.push(if *escape { 0xF7 } else { 0xF0 });
                    vlq(&mut body, data.len() as u32);
                    body.extend(data);
                    running = None;
                }
                EventKind::Meta { kind, data } => {
                    body.push(0xFF);
                    body.push(*kind);
                    vlq(&mut body, data.len() as u32);
                    body.extend(data);
                }
            }
        }
        out.extend(b"MTrk");
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(body);
        if i == 0 && !smf.extra_chunks.is_empty() {
            // unknown chunks may appear between tracks
        }
    }
    for (id, d) in &smf.extra_chunks {
        out.extend(id);
        out.extend((d.len() as u32).to_be_bytes());
        out.extend(d);
    }
    out
}

fn chan(status: u8, channel: u8, d1: u8, d2: u8) -> EventKind {
    EventKind::Channel { status, channel, data1: d1, data2: if matches!(status, 0xC | 0xD) { 0 } else { d2 } }
}

fn gen_track(r: &mut Rng, conductor: bool) -> RawTrack {
    let mut ev: Vec<Event> = Vec::new();
    let mut tick = 0u64;
    let mut pending: Vec<(u64, u8, u8, bool, u8)> = Vec::new(); // (off_tick, ch, pitch, as_note_on, offvel)
    let n = 20 + r.below(120);
    if conductor || r.chance(30) {
        ev.push(Event { tick: 0, kind: EventKind::Meta { kind: 0x03, data: b"Track \xE2\x99\xAB".to_vec() } });
    }
    for _ in 0..n {
        tick += match r.below(4) {
            0 => 0,
            1 => r.below(30),
            2 => r.below(480),
            _ => r.below(2000),
        };
        // emit pending offs that are due, in order
        pending.sort_by_key(|p| p.0);
        while let Some(p) = pending.first().cloned() {
            if p.0 > tick {
                break;
            }
            pending.remove(0);
            let kind = if p.3 { chan(0x9, p.1, p.2, 0) } else { chan(0x8, p.1, p.2, p.4) };
            ev.push(Event { tick: p.0, kind });
        }
        let ch = r.below(16) as u8;
        let kind = match r.below(22) {
            0..=6 => {
                let pitch = if r.chance(20) { 60 } else { r.below(128) as u8 };
                let vel = 1 + r.below(127) as u8;
                let dur = if r.chance(10) { 0 } else { r.below(1000) };
                if r.chance(5) {
                    // hanging note-on, never released
                } else {
                    pending.push((tick + dur, ch, pitch, r.chance(50), r.below(128) as u8));
                }
                chan(0x9, ch, pitch, vel)
            }
            7 => chan(0x8, ch, r.below(128) as u8, r.below(128) as u8), // unmatched off
            8 => chan(0x9, ch, r.below(128) as u8, 0),                  // unmatched on-vel-0
            9 | 10 => chan(0xB, ch, r.below(128) as u8, r.below(128) as u8),
            11 => chan(0xC, ch, r.below(128) as u8, 0),
            12 => chan(0xE, ch, r.below(128) as u8, r.below(128) as u8),
            13 => chan(0xD, ch, r.below(128) as u8, 0),
            14 => chan(0xA, ch, r.below(128) as u8, r.below(128) as u8),
            15 => {
                let mut d = {
                    let n = r.below(20) as usize;
                    r.bytes(n, 0x7F)
                };
                d.push(0xF7);
                EventKind::Sysex { escape: false, data: d }
            }
            16 => EventKind::Sysex {
                escape: true,
                data: {
                    let n = r.below(6) as usize;
                    r.bytes(n, 0xFF)
                },
            },
            17 => {
                let kind = 1 + r.below(9) as u8;
                let data = if r.chance(70) {
                    format!("text {} é", r.below(1000)).into_bytes()
                } else {
                    {
                        let n = r.below(10) as usize;
                        r.bytes(n, 0xFF)
                    } // possibly invalid UTF-8
                };
                EventKind::Meta { kind, data }
            }
            18 => {
                // tempo (sometimes malformed)
                if r.chance(85) {
                    let us = 200_000 + r.below(1_000_000) as u32;
                    EventKind::Meta { kind: 0x51, data: us.to_be_bytes()[1..].to_vec() }
                } else {
                    EventKind::Meta {
                        kind: 0x51,
                        data: {
                            let n = if r.chance(50) { 3 } else { 2 };
                            r.bytes(n, 0)
                        },
                    }
                }
            }
            19 => {
                if r.chance(85) {
                    EventKind::Meta { kind: 0x58, data: vec![1 + r.below(12) as u8, r.below(5) as u8, 24, 8] }
                } else {
                    EventKind::Meta { kind: 0x58, data: vec![4, 40, 24, 8] }
                }
            }
            20 => {
                if r.chance(85) {
                    EventKind::Meta { kind: 0x59, data: vec![(r.below(15) as i8 - 7) as u8, r.below(2) as u8] }
                } else {
                    EventKind::Meta { kind: 0x59, data: vec![0, 5] }
                }
            }
            _ => {
                let kind = [0x00u8, 0x20, 0x21, 0x54, 0x7F, 0x60][r.below(6) as usize];
                EventKind::Meta {
                    kind,
                    data: {
                        let n = r.below(6) as usize;
                        r.bytes(n, 0xFF)
                    },
                }
            }
        };
        ev.push(Event { tick, kind });
    }
    pending.sort_by_key(|p| p.0);
    for p in pending {
        let kind = if p.3 { chan(0x9, p.1, p.2, 0) } else { chan(0x8, p.1, p.2, p.4) };
        let t = p.0.max(ev.last().map(|e| e.tick).unwrap_or(0));
        ev.push(Event { tick: t, kind });
    }
    let last = ev.last().map(|e| e.tick).unwrap_or(0);
    ev.push(Event { tick: last + r.below(500), kind: EventKind::Meta { kind: 0x2F, data: vec![] } });
    RawTrack { events: ev }
}

fn gen_file(seed: u64) -> Smf {
    let mut r = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let format = r.below(3) as u16;
    let ntracks = if format == 0 { 1 } else { 1 + r.below(5) as usize };
    let timing = if r.chance(15) {
        Timing::Smpte { fps: [24u8, 25, 29, 30][r.below(4) as usize], ticks_per_frame: 4 + r.below(80) as u8 }
    } else {
        Timing::Ppq([96u16, 120, 384, 480, 960, 1000][r.below(6) as usize])
    };
    let tracks = (0..ntracks).map(|i| gen_track(&mut r, i == 0)).collect();
    let extra_chunks = if r.chance(30) {
        vec![(*b"XFIH", {
            let n = r.below(12) as usize;
            r.bytes(n, 0xFF)
        })]
    } else {
        vec![]
    };
    Smf { format, timing, tracks, extra_chunks }
}

#[test]
fn parser_reads_running_status_files_exactly() {
    for seed in 1..=60 {
        let smf = gen_file(seed);
        let bytes = encode(&smf);
        let parsed = Smf::parse(&bytes).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(parsed, smf, "seed {seed}: parse mismatch");
    }
}

#[test]
fn json_round_trip_reproduces_events_exactly() {
    let mut total_events = 0;
    for seed in 1..=60 {
        let smf = gen_file(seed);
        let parsed = Smf::parse(&encode(&smf)).unwrap();
        let model = MidiFile::from_smf(&parsed);
        // to-json then from-json
        let text = model.to_json_string();
        let back = MidiFile::from_json_str(&text).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(back, model, "seed {seed}: model mismatch after JSON");
        let written = back.to_smf_bytes();
        let reparsed = Smf::parse(&written).unwrap();
        assert_eq!(reparsed.tracks.len(), parsed.tracks.len());
        for (ti, (a, b)) in parsed.tracks.iter().zip(&reparsed.tracks).enumerate() {
            assert_eq!(a.events.len(), b.events.len(), "seed {seed} track {ti}: event count");
            for (i, (x, y)) in a.events.iter().zip(&b.events).enumerate() {
                assert_eq!(x, y, "seed {seed} track {ti} event {i}");
            }
            total_events += a.events.len();
        }
        assert_eq!(reparsed, parsed, "seed {seed}");
    }
    assert!(total_events > 5000, "corpus too small: {total_events}");
}

#[test]
fn smf_to_smf_is_stable_and_compact() {
    for seed in 100..120 {
        let smf = gen_file(seed);
        let parsed = Smf::parse(&encode(&smf)).unwrap();
        let once = MidiFile::from_smf(&parsed).to_smf_bytes();
        let twice = MidiFile::from_smf_bytes(&once).unwrap().to_smf_bytes();
        assert_eq!(once, twice, "seed {seed}");
    }
}

#[test]
fn truncated_and_garbage_inputs_fail_cleanly() {
    let bytes = encode(&gen_file(7));
    for cut in [0, 5, 13, 14, 20, 30, bytes.len() / 2] {
        let _ = Smf::parse(&bytes[..cut.min(bytes.len())]);
    }
    let mut r = Rng(99);
    for _ in 0..200 {
        let mut b = bytes.clone();
        for _ in 0..5 {
            let i = r.below(b.len() as u64) as usize;
            b[i] = r.below(256) as u8;
        }
        // must not panic
        if let Ok(f) = MidiFile::from_smf_bytes(&b) {
            let _ = MidiFile::from_json_str(&f.to_json_string());
        }
    }
}
