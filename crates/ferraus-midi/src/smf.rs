//! A small, strict Standard MIDI File codec.
//!
//! Reads formats 0, 1, and 2 with running status, SysEx (`F0` and `F7` escape packets),
//! and every meta event; unknown chunks are kept verbatim. Writing never uses running
//! status (event-level identical, byte-level canonical). Inputs are untrusted: every
//! length is bounds-checked and nothing allocates more than the file holds.
//!
//! This is implemented here instead of using `midly` because `midly` is Unlicense, which the
//! project's cargo-deny policy does not allow, and because owning the codec lets the JSON
//! model keep every event exactly.

use ferraus_core::{Error, Result};

/// File timing: pulses per quarter note or SMPTE frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    Ppq(u16),
    /// `fps` is 24, 25, 29 (29.97 drop-frame), or 30.
    Smpte {
        fps: u8,
        ticks_per_frame: u8,
    },
}

impl Timing {
    /// Ticks per second for SMPTE timing.
    pub fn smpte_ticks_per_second(&self) -> Option<f64> {
        match *self {
            Timing::Ppq(_) => None,
            Timing::Smpte { fps, ticks_per_frame } => {
                let f = if fps == 29 { 29.97 } else { fps as f64 };
                Some(f * ticks_per_frame as f64)
            }
        }
    }
}

/// One event body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// Channel voice message. `status` is the high nibble (0x8..=0xE), `channel` 0..=15.
    /// `data2` is 0 for one-data-byte messages (program change, channel pressure).
    Channel { status: u8, channel: u8, data1: u8, data2: u8 },
    /// `F0` (normal) or `F7` (escape) SysEx packet; `data` excludes the length.
    Sysex { escape: bool, data: Vec<u8> },
    /// Meta event `FF type len data`.
    Meta { kind: u8, data: Vec<u8> },
}

impl EventKind {
    pub fn is_end_of_track(&self) -> bool {
        matches!(self, EventKind::Meta { kind: 0x2F, .. })
    }
}

/// An event at an absolute tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub tick: u64,
    pub kind: EventKind,
}

/// A track: events in file order with absolute ticks. The End-of-Track meta event is
/// included as the last event when the file had one.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RawTrack {
    pub events: Vec<Event>,
}

/// A parsed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Smf {
    pub format: u16,
    pub timing: Timing,
    pub tracks: Vec<RawTrack>,
    /// Non-`MTrk` chunks (id, body), kept so nothing is lost.
    pub extra_chunks: Vec<([u8; 4], Vec<u8>)>,
}

/// Number of data bytes for a channel status nibble.
pub fn data_len(status: u8) -> usize {
    match status {
        0xC | 0xD => 1,
        _ => 2,
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::new(ferraus_core::ErrorKind::InputMissing, "invalid_midi", format!("not a valid MIDI file: {}", msg.into()))
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8> {
        let v = *self.b.get(self.pos).ok_or_else(|| bad("unexpected end of track"))?;
        self.pos += 1;
        Ok(v)
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }
    fn vlq(&mut self) -> Result<u32> {
        let mut v: u32 = 0;
        for i in 0..4 {
            let c = self.u8()?;
            v = (v << 7) | (c & 0x7F) as u32;
            if c & 0x80 == 0 {
                return Ok(v);
            }
            if i == 3 {
                break;
            }
        }
        Err(bad("variable-length number longer than 4 bytes"))
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.b.len() {
            return Err(bad("event data runs past the end of the track"));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn done(&self) -> bool {
        self.pos >= self.b.len()
    }
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl Smf {
    /// Parse SMF bytes (an optional RIFF `RMID` wrapper is accepted).
    pub fn parse(bytes: &[u8]) -> Result<Smf> {
        let bytes = unwrap_rmid(bytes);
        if bytes.len() < 14 || &bytes[0..4] != b"MThd" {
            return Err(bad("missing MThd header"));
        }
        let hlen = be32(&bytes[4..8]) as usize;
        if hlen < 6 || 8 + hlen > bytes.len() {
            return Err(bad("short header"));
        }
        let format = be16(&bytes[8..10]);
        let ntracks = be16(&bytes[10..12]) as usize;
        let div = be16(&bytes[12..14]);
        let timing = if div & 0x8000 != 0 {
            let fps = (-((div >> 8) as u8 as i8) as i16) as u8;
            Timing::Smpte { fps, ticks_per_frame: (div & 0xFF) as u8 }
        } else {
            if div == 0 {
                return Err(bad("division is zero"));
            }
            Timing::Ppq(div)
        };
        if format > 2 {
            return Err(bad(format!("unknown format {format}")));
        }
        let mut pos = 8 + hlen;
        let mut tracks = Vec::new();
        let mut extra = Vec::new();
        while pos + 8 <= bytes.len() {
            let id: [u8; 4] = bytes[pos..pos + 4].try_into().unwrap();
            let len = be32(&bytes[pos + 4..pos + 8]) as usize;
            let start = pos + 8;
            let end = start.checked_add(len).ok_or_else(|| bad("chunk too long"))?;
            if end > bytes.len() {
                // tolerate a truncated final track only if it is an MTrk and parses to EOT
                if &id == b"MTrk" {
                    tracks.push(parse_track(&bytes[start..])?);
                    break;
                }
                return Err(bad("chunk runs past the end of the file"));
            }
            if &id == b"MTrk" {
                tracks.push(parse_track(&bytes[start..end])?);
            } else {
                extra.push((id, bytes[start..end].to_vec()));
            }
            pos = end;
        }
        if tracks.is_empty() && ntracks > 0 {
            return Err(bad("no MTrk chunks"));
        }
        Ok(Smf { format, timing, tracks, extra_chunks: extra })
    }

    /// Serialize to SMF bytes (no running status).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&self.format.to_be_bytes());
        out.extend_from_slice(&(self.tracks.len().min(u16::MAX as usize) as u16).to_be_bytes());
        let div: u16 = match self.timing {
            Timing::Ppq(p) => p & 0x7FFF,
            Timing::Smpte { fps, ticks_per_frame } => (((-(fps as i8)) as u8 as u16) << 8) | ticks_per_frame as u16,
        };
        out.extend_from_slice(&div.to_be_bytes());
        for t in &self.tracks {
            let body = write_track(t);
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        for (id, body) in &self.extra_chunks {
            out.extend_from_slice(id);
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(body);
        }
        out
    }
}

fn unwrap_rmid(bytes: &[u8]) -> &[u8] {
    if bytes.len() > 20 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"RMID" {
        let mut pos = 12;
        while pos + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
            if &bytes[pos..pos + 4] == b"data" {
                let end = (pos + 8 + len).min(bytes.len());
                return &bytes[pos + 8..end];
            }
            pos += 8 + len + (len & 1);
        }
    }
    bytes
}

fn parse_track(b: &[u8]) -> Result<RawTrack> {
    let mut r = Reader { b, pos: 0 };
    let mut tick: u64 = 0;
    let mut running: Option<u8> = None;
    let mut events = Vec::new();
    while !r.done() {
        let delta = r.vlq()?;
        tick += delta as u64;
        let first = r.peek().ok_or_else(|| bad("missing event after delta time"))?;
        let kind = if first == 0xFF {
            r.u8()?;
            let kind = r.u8()?;
            let len = r.vlq()? as usize;
            let data = r.bytes(len)?.to_vec();
            EventKind::Meta { kind, data }
        } else if first == 0xF0 || first == 0xF7 {
            r.u8()?;
            let len = r.vlq()? as usize;
            let data = r.bytes(len)?.to_vec();
            EventKind::Sysex { escape: first == 0xF7, data }
        } else if first >= 0xF0 {
            return Err(bad(format!("system message 0x{first:02X} inside a track")));
        } else {
            let status = if first & 0x80 != 0 {
                r.u8()?;
                running = Some(first);
                first
            } else {
                running.ok_or_else(|| bad("data byte without a status (running status unset)"))?
            };
            let n = data_len(status >> 4);
            let d1 = r.u8()?;
            let d2 = if n == 2 { r.u8()? } else { 0 };
            if d1 & 0x80 != 0 || d2 & 0x80 != 0 {
                return Err(bad("data byte with the high bit set"));
            }
            EventKind::Channel { status: status >> 4, channel: status & 0x0F, data1: d1, data2: d2 }
        };
        let eot = kind.is_end_of_track();
        events.push(Event { tick, kind });
        if eot {
            break;
        }
    }
    Ok(RawTrack { events })
}

pub fn write_vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut buf = [0u8; 5];
    let mut i = 4;
    buf[i] = (v & 0x7F) as u8;
    v >>= 7;
    while v > 0 {
        i -= 1;
        buf[i] = ((v & 0x7F) as u8) | 0x80;
        v >>= 7;
    }
    out.extend_from_slice(&buf[i..]);
}

fn write_track(t: &RawTrack) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last = 0u64;
    for e in &t.events {
        let tick = e.tick.max(last);
        let delta = (tick - last).min(0x0FFF_FFFF) as u32;
        last = tick;
        write_vlq(&mut out, delta);
        match &e.kind {
            EventKind::Channel { status, channel, data1, data2 } => {
                out.push((status << 4) | (channel & 0x0F));
                out.push(data1 & 0x7F);
                if data_len(*status) == 2 {
                    out.push(data2 & 0x7F);
                }
            }
            EventKind::Sysex { escape, data } => {
                out.push(if *escape { 0xF7 } else { 0xF0 });
                write_vlq(&mut out, data.len() as u32);
                out.extend_from_slice(data);
            }
            EventKind::Meta { kind, data } => {
                out.push(0xFF);
                out.push(*kind);
                write_vlq(&mut out, data.len() as u32);
                out.extend_from_slice(data);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_roundtrip() {
        for v in [0u32, 1, 127, 128, 255, 16383, 16384, 0x0FFF_FFFF] {
            let mut o = Vec::new();
            write_vlq(&mut o, v);
            let mut r = Reader { b: &o, pos: 0 };
            assert_eq!(r.vlq().unwrap(), v);
            assert!(r.done());
        }
    }

    #[test]
    fn running_status_and_garbage() {
        // header + one track: note on 60, running-status note on 64, EOT
        let mut f = b"MThd\0\0\0\x06\0\0\0\x01\x01\xE0".to_vec();
        let trk = [0x00, 0x90, 60, 100, 0x10, 64, 90, 0x00, 0xFF, 0x2F, 0x00];
        f.extend_from_slice(b"MTrk");
        f.extend_from_slice(&(trk.len() as u32).to_be_bytes());
        f.extend_from_slice(&trk);
        let s = Smf::parse(&f).unwrap();
        assert_eq!(s.timing, Timing::Ppq(480));
        assert_eq!(s.tracks[0].events.len(), 3);
        assert_eq!(s.tracks[0].events[1].tick, 16);
        assert!(Smf::parse(b"hello").is_err());
        assert!(Smf::parse(&f[..20]).is_err());
        let back = Smf::parse(&s.to_bytes()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn smpte_division() {
        let s = Smf {
            format: 0,
            timing: Timing::Smpte { fps: 25, ticks_per_frame: 40 },
            tracks: vec![RawTrack {
                events: vec![Event { tick: 0, kind: EventKind::Meta { kind: 0x2F, data: vec![] } }],
            }],
            extra_chunks: vec![],
        };
        assert_eq!(Smf::parse(&s.to_bytes()).unwrap(), s);
        assert_eq!(s.timing.smpte_ticks_per_second(), Some(1000.0));
    }
}
