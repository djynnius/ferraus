//! Streaming WAV writer: PCM 8/16/24/32-bit, IEEE float 32/64. Sizes are patched on
//! `finish`; outputs over 4 GiB are rewritten as RF64 using a reserved `JUNK` chunk.

use super::{io_err, EncodeReport, StreamEncoder, StreamSpec, WriteSeek};
use crate::pcm::{Dither, Quantizer};
use ferraus_core::{Error, Result};
use std::io::{BufWriter, Seek, SeekFrom, Write};

pub struct WavEncoder {
    w: BufWriter<Box<dyn WriteSeek>>,
    spec: StreamSpec,
    bits: u16,
    float: bool,
    q: Quantizer,
    ints: Vec<i32>,
    bytes: Vec<u8>,
    data_len: u64,
    frames: u64,
    has_junk: bool,
}

const JUNK_LEN: u32 = 28;

impl WavEncoder {
    pub fn new(
        sink: Box<dyn WriteSeek>,
        spec: StreamSpec,
        bits: u16,
        float: bool,
        dither: Dither,
        seed: u64,
    ) -> Result<WavEncoder> {
        let (float, bits) = if float {
            (true, if bits == 64 { 64 } else { 32 })
        } else if [8, 16, 24, 32].contains(&bits) {
            (false, bits)
        } else {
            return Err(Error::unsupported(format!(
                "WAV bit depth {bits} is not supported (8, 16, 24, 32, or float)"
            )));
        };
        let bps = (bits / 8) as u64;
        let est = spec
            .total_frames
            .map(|f| f * bps * spec.channels as u64)
            .unwrap_or(u64::MAX);
        let has_junk = est > 0xF000_0000;
        let mut enc = WavEncoder {
            w: BufWriter::with_capacity(1 << 16, sink),
            spec,
            bits,
            float,
            q: Quantizer::new(bits, dither, seed),
            ints: Vec::new(),
            bytes: Vec::new(),
            data_len: 0,
            frames: 0,
            has_junk,
        };
        enc.header()?;
        Ok(enc)
    }

    fn header(&mut self) -> Result<()> {
        let ch = self.spec.channels as u16;
        let bps = self.bits / 8;
        let tag: u16 = if self.float { 3 } else { 1 };
        let extensible = !self.float && (ch > 2 || self.bits > 16);
        let fmt_len: u32 = if extensible { 40 } else { 16 };
        let mut h = Vec::with_capacity(80);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(b"WAVE");
        if self.has_junk {
            h.extend_from_slice(b"JUNK");
            h.extend_from_slice(&JUNK_LEN.to_le_bytes());
            h.extend_from_slice(&[0u8; JUNK_LEN as usize]);
        }
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&fmt_len.to_le_bytes());
        h.extend_from_slice(&(if extensible { 0xFFFEu16 } else { tag }).to_le_bytes());
        h.extend_from_slice(&ch.to_le_bytes());
        h.extend_from_slice(&self.spec.sample_rate.to_le_bytes());
        h.extend_from_slice(&(self.spec.sample_rate * ch as u32 * bps as u32).to_le_bytes());
        h.extend_from_slice(&(ch * bps).to_le_bytes());
        h.extend_from_slice(&self.bits.to_le_bytes());
        if extensible {
            h.extend_from_slice(&22u16.to_le_bytes());
            h.extend_from_slice(&self.bits.to_le_bytes()); // valid bits
            let mask: u32 = match ch {
                1 => 0x4,
                2 => 0x3,
                n => ((1u64 << n.min(18)) - 1) as u32,
            };
            h.extend_from_slice(&mask.to_le_bytes());
            // KSDATAFORMAT_SUBTYPE_PCM
            h.extend_from_slice(&[
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00,
                0x38, 0x9B, 0x71,
            ]);
        }
        h.extend_from_slice(b"data");
        h.extend_from_slice(&0u32.to_le_bytes());
        self.w.write_all(&h).map_err(io_err)
    }

    fn header_len(&self) -> u64 {
        let extensible = !self.float && (self.spec.channels > 2 || self.bits > 16);
        12 + if self.has_junk { 8 + JUNK_LEN as u64 } else { 0 }
            + 8
            + if extensible { 40 } else { 16 }
            + 8
    }
}

impl StreamEncoder for WavEncoder {
    fn write(&mut self, data: &[&[f32]]) -> Result<()> {
        let frames = data.first().map(|c| c.len()).unwrap_or(0);
        let ch = self.spec.channels;
        self.bytes.clear();
        if self.float {
            self.bytes.reserve(frames * ch * (self.bits as usize / 8));
            for f in 0..frames {
                for c in data.iter().take(ch) {
                    if self.bits == 64 {
                        self.bytes.extend_from_slice(&(c[f] as f64).to_le_bytes());
                    } else {
                        self.bytes.extend_from_slice(&c[f].to_le_bytes());
                    }
                }
            }
        } else {
            self.q.quantize(&data[..ch.min(data.len())], &mut self.ints);
            self.bytes.reserve(self.ints.len() * (self.bits as usize / 8));
            for &v in &self.ints {
                match self.bits {
                    8 => self.bytes.push((v + 128) as u8),
                    16 => self.bytes.extend_from_slice(&(v as i16).to_le_bytes()),
                    24 => self.bytes.extend_from_slice(&v.to_le_bytes()[..3]),
                    _ => self.bytes.extend_from_slice(&v.to_le_bytes()),
                }
            }
        }
        self.w.write_all(&self.bytes).map_err(io_err)?;
        self.data_len += self.bytes.len() as u64;
        self.frames += frames as u64;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<EncodeReport> {
        if self.data_len & 1 == 1 {
            self.w.write_all(&[0]).map_err(io_err)?;
        }
        let hl = self.header_len();
        let riff = hl - 8 + self.data_len + (self.data_len & 1);
        let w = &mut self.w;
        if riff > u32::MAX as u64 {
            if !self.has_junk {
                return Err(Error::processing(
                    "WAV output exceeds 4 GiB; write FLAC or use a lower bit depth",
                ));
            }
            w.seek(SeekFrom::Start(0)).map_err(io_err)?;
            w.write_all(b"RF64").map_err(io_err)?;
            w.write_all(&u32::MAX.to_le_bytes()).map_err(io_err)?;
            w.write_all(b"WAVEds64").map_err(io_err)?;
            w.write_all(&JUNK_LEN.to_le_bytes()).map_err(io_err)?;
            w.write_all(&riff.to_le_bytes()).map_err(io_err)?;
            w.write_all(&self.data_len.to_le_bytes()).map_err(io_err)?;
            w.write_all(&self.frames.to_le_bytes()).map_err(io_err)?;
            w.write_all(&0u32.to_le_bytes()).map_err(io_err)?;
            w.seek(SeekFrom::Start(hl - 4)).map_err(io_err)?;
            w.write_all(&u32::MAX.to_le_bytes()).map_err(io_err)?;
        } else {
            w.seek(SeekFrom::Start(4)).map_err(io_err)?;
            w.write_all(&(riff as u32).to_le_bytes()).map_err(io_err)?;
            w.seek(SeekFrom::Start(hl - 4)).map_err(io_err)?;
            w.write_all(&(self.data_len as u32).to_le_bytes()).map_err(io_err)?;
        }
        w.seek(SeekFrom::End(0)).map_err(io_err)?;
        w.flush().map_err(io_err)?;
        Ok(EncodeReport {
            codec: match (self.float, self.bits) {
                (true, b) => format!("pcm_f{b}le"),
                (false, 8) => "pcm_u8".into(),
                (false, b) => format!("pcm_s{b}le"),
            },
            bit_depth: Some(self.bits),
            bitrate: Some(self.spec.sample_rate * self.spec.channels as u32 * self.bits as u32),
            frames: self.frames,
            clipped: self.q.clipped,
        })
    }
}
