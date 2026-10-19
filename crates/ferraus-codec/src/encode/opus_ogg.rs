//! Ogg Opus writer (RFC 7845): libopus frames of 20 ms at 48 kHz, muxed with the pure-Rust
//! `ogg` crate. Input must already be 48 kHz; 1 or 2 channels.

use super::{EncodeReport, StreamEncoder, StreamSpec, WriteSeek};
use crate::opus::{OpusEncoder, OpusHead};
use ferraus_core::{Error, Result};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};

const FRAME: usize = 960;

pub struct OggOpusEncoder {
    enc: OpusEncoder,
    w: PacketWriter<'static, Box<dyn WriteSeek>>,
    serial: u32,
    pre_skip: u32,
    channels: usize,
    pcm: Vec<f32>,
    /// One packet held back so the last one can be flagged end-of-stream.
    held: Option<(Vec<u8>, u64)>,
    fed: u64,
    real: u64,
    packets: u64,
    bitrate: u32,
    bytes: u64,
}

fn oerr(e: impl std::fmt::Display) -> Error {
    Error::processing(format!("Opus encoder: {e}"))
}

fn tags_packet() -> Vec<u8> {
    let vendor = format!("ferraus {}", ferraus_core::FERRAUS_VERSION);
    let mut v = Vec::new();
    v.extend_from_slice(b"OpusTags");
    v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    v.extend_from_slice(vendor.as_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

impl OggOpusEncoder {
    pub fn new(
        sink: Box<dyn WriteSeek>,
        spec: StreamSpec,
        bitrate: u32,
        vbr: bool,
        seed: u64,
    ) -> Result<OggOpusEncoder> {
        if spec.sample_rate != 48000 {
            return Err(Error::internal("Opus encoder input must be resampled to 48 kHz"));
        }
        if spec.channels > 2 {
            return Err(Error::unsupported(format!(
                "Opus output supports 1 or 2 channels here, got {}",
                spec.channels
            ))
            .with_hint("add --channels 2"));
        }
        let bitrate = bitrate.clamp(6_000, 510_000);
        let mut enc = OpusEncoder::new(spec.channels, bitrate as i32, vbr).map_err(|e| {
            Error::unsupported(format!("Opus encoding unavailable: {e}"))
                .with_hint(crate::ffmpeg_bridge::INSTALL_HINT)
        })?;
        let pre_skip = enc.lookahead();
        let serial = (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32 | 1;
        let mut w = PacketWriter::new(sink);
        let head = OpusHead::build(spec.channels as u8, pre_skip as u16, 48000);
        w.write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(oerr)?;
        w.write_packet(tags_packet(), serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(oerr)?;
        Ok(OggOpusEncoder {
            enc,
            w,
            serial,
            pre_skip,
            channels: spec.channels,
            pcm: Vec::new(),
            held: None,
            fed: 0,
            real: 0,
            packets: 0,
            bitrate,
            bytes: 0,
        })
    }

    fn emit(&mut self, packet: Vec<u8>, granule: u64) -> Result<()> {
        if let Some((p, g)) = self.held.take() {
            self.packets += 1;
            let info = if self.packets % 50 == 0 {
                PacketWriteEndInfo::EndPage
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            self.bytes += p.len() as u64;
            self.w.write_packet(p, self.serial, info, g).map_err(oerr)?;
        }
        self.held = Some((packet, granule));
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        let fs = FRAME * self.channels;
        let mut off = 0;
        while self.pcm.len() - off >= fs {
            let pkt = self.enc.encode(&self.pcm[off..off + fs], FRAME).map_err(oerr)?;
            off += fs;
            self.fed += FRAME as u64;
            let g = self.fed;
            self.emit(pkt, g)?;
        }
        self.pcm.drain(..off);
        Ok(())
    }
}

impl StreamEncoder for OggOpusEncoder {
    fn write(&mut self, data: &[&[f32]]) -> Result<()> {
        let n = data.first().map(|c| c.len()).unwrap_or(0);
        self.pcm.reserve(n * self.channels);
        for f in 0..n {
            for c in 0..self.channels {
                self.pcm.push(data[c.min(data.len() - 1)][f]);
            }
        }
        self.real += n as u64;
        self.drain()
    }

    fn finish(mut self: Box<Self>) -> Result<EncodeReport> {
        // feed pre-skip worth of silence so the encoder flushes its lookahead, then pad
        let needed = self.real + self.pre_skip as u64;
        let fed_total = self.fed + (self.pcm.len() / self.channels) as u64;
        let mut pad = needed.saturating_sub(fed_total) as usize;
        let rem = ((fed_total as usize) + pad) % FRAME;
        if rem != 0 {
            pad += FRAME - rem;
        }
        self.pcm.extend(std::iter::repeat_n(0.0, pad * self.channels));
        self.drain()?;
        let end_g = self.pre_skip as u64 + self.real;
        if let Some((p, _)) = self.held.take() {
            self.bytes += p.len() as u64;
            self.w
                .write_packet(p, self.serial, PacketWriteEndInfo::EndStream, end_g)
                .map_err(oerr)?;
        } else {
            self.w
                .write_packet(Vec::new(), self.serial, PacketWriteEndInfo::EndStream, end_g)
                .map_err(oerr)?;
        }
        let mut inner = self.w.into_inner();
        std::io::Write::flush(&mut inner)?;
        let secs = self.real as f64 / 48000.0;
        Ok(EncodeReport {
            codec: "opus".into(),
            bit_depth: None,
            bitrate: if secs > 0.0 { Some((self.bytes as f64 * 8.0 / secs) as u32) } else { Some(self.bitrate) },
            frames: self.real,
            clipped: 0,
        })
    }
}
