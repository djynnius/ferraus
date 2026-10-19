//! Streaming AIFF writer (big-endian signed PCM 8/16/24/32).

use super::{io_err, EncodeReport, StreamEncoder, StreamSpec, WriteSeek};
use crate::pcm::{Dither, Quantizer};
use ferraus_core::{Error, Result};
use std::io::{BufWriter, Seek, SeekFrom, Write};

pub struct AiffEncoder {
    w: BufWriter<Box<dyn WriteSeek>>,
    spec: StreamSpec,
    bits: u16,
    q: Quantizer,
    ints: Vec<i32>,
    bytes: Vec<u8>,
    data_len: u64,
    frames: u64,
}

/// 80-bit IEEE extended representation of an integer sample rate.
pub fn ext80(v: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if v == 0 {
        return out;
    }
    let n = v as u64;
    let shift = n.leading_zeros();
    let mant = n << shift;
    let exp = (16383 + 63 - shift) as u16;
    out[0..2].copy_from_slice(&exp.to_be_bytes());
    out[2..10].copy_from_slice(&mant.to_be_bytes());
    out
}

const HEADER_LEN: u64 = 12 + 26 + 16;

impl AiffEncoder {
    pub fn new(
        sink: Box<dyn WriteSeek>,
        spec: StreamSpec,
        bits: u16,
        dither: Dither,
        seed: u64,
    ) -> Result<AiffEncoder> {
        let bits = match bits {
            8 | 16 | 24 | 32 => bits,
            64 => 32,
            b => {
                return Err(Error::unsupported(format!(
                    "AIFF bit depth {b} is not supported (8, 16, 24, 32)"
                )))
            }
        };
        let mut enc = AiffEncoder {
            w: BufWriter::with_capacity(1 << 16, sink),
            spec,
            bits,
            q: Quantizer::new(bits, dither, seed),
            ints: Vec::new(),
            bytes: Vec::new(),
            data_len: 0,
            frames: 0,
        };
        let mut h = Vec::with_capacity(HEADER_LEN as usize);
        h.extend_from_slice(b"FORM");
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(b"AIFF");
        h.extend_from_slice(b"COMM");
        h.extend_from_slice(&18u32.to_be_bytes());
        h.extend_from_slice(&(spec.channels as u16).to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&bits.to_be_bytes());
        h.extend_from_slice(&ext80(spec.sample_rate));
        h.extend_from_slice(b"SSND");
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes()); // offset
        h.extend_from_slice(&0u32.to_be_bytes()); // block size
        enc.w.write_all(&h).map_err(io_err)?;
        Ok(enc)
    }
}

impl StreamEncoder for AiffEncoder {
    fn write(&mut self, data: &[&[f32]]) -> Result<()> {
        let ch = self.spec.channels.min(data.len());
        self.q.quantize(&data[..ch], &mut self.ints);
        self.bytes.clear();
        for &v in &self.ints {
            match self.bits {
                8 => self.bytes.push(v as i8 as u8),
                16 => self.bytes.extend_from_slice(&(v as i16).to_be_bytes()),
                24 => self.bytes.extend_from_slice(&v.to_be_bytes()[1..]),
                _ => self.bytes.extend_from_slice(&v.to_be_bytes()),
            }
        }
        self.w.write_all(&self.bytes).map_err(io_err)?;
        self.data_len += self.bytes.len() as u64;
        self.frames += data.first().map(|c| c.len()).unwrap_or(0) as u64;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<EncodeReport> {
        if self.data_len & 1 == 1 {
            self.w.write_all(&[0]).map_err(io_err)?;
        }
        let form = HEADER_LEN - 8 + self.data_len + (self.data_len & 1);
        if form > u32::MAX as u64 {
            return Err(Error::processing("AIFF output exceeds 4 GiB; write FLAC instead"));
        }
        let w = &mut self.w;
        w.seek(SeekFrom::Start(4)).map_err(io_err)?;
        w.write_all(&(form as u32).to_be_bytes()).map_err(io_err)?;
        w.seek(SeekFrom::Start(22)).map_err(io_err)?;
        w.write_all(&(self.frames as u32).to_be_bytes()).map_err(io_err)?;
        w.seek(SeekFrom::Start(42)).map_err(io_err)?;
        w.write_all(&((self.data_len + 8) as u32).to_be_bytes()).map_err(io_err)?;
        w.seek(SeekFrom::End(0)).map_err(io_err)?;
        w.flush().map_err(io_err)?;
        Ok(EncodeReport {
            codec: format!("pcm_s{}be", self.bits),
            bit_depth: Some(self.bits),
            bitrate: Some(self.spec.sample_rate * self.spec.channels as u32 * self.bits as u32),
            frames: self.frames,
            clipped: self.q.clipped,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ext80_44100() {
        assert_eq!(
            super::ext80(44100),
            [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]
        );
    }
}
