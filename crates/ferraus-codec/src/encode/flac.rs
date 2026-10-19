//! Streaming FLAC encoder on top of the pure-Rust `flacenc` crate: frames are encoded and
//! written one block at a time, and STREAMINFO (sizes, sample count, MD5) is patched at the end.

use super::{io_err, EncodeReport, StreamEncoder, StreamSpec, WriteSeek};
use crate::pcm::{Dither, Quantizer};
use ferraus_core::{Error, Result};
use flacenc::bitsink::MemSink;
use flacenc::component::{BitRepr, ChannelAssignment, Frame, FrameHeader, FrameOffset, Stream, StreamInfo, Verbatim};
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};
use std::io::{BufWriter, Seek, SeekFrom, Write};

const BLOCK: usize = 4096;

pub struct FlacEncoder {
    w: BufWriter<Box<dyn WriteSeek>>,
    spec: StreamSpec,
    bits: u16,
    q: Quantizer,
    config: flacenc::error::Verified<flacenc::config::Encoder>,
    info: StreamInfo,
    fb: FrameBuf,
    ctx: Context,
    pending: Vec<i32>,
    ints: Vec<i32>,
    frame_no: usize,
    frames: u64,
    bytes: u64,
}

fn ferr(e: impl std::fmt::Debug) -> Error {
    Error::processing(format!("FLAC encoder: {e:?}"))
}

fn header_bytes(info: &StreamInfo) -> Result<Vec<u8>> {
    let stream = Stream::with_stream_info(info.clone());
    let mut sink = MemSink::<u8>::new();
    stream.write(&mut sink).map_err(ferr)?;
    Ok(sink.into_inner())
}

impl FlacEncoder {
    pub fn new(
        sink: Box<dyn WriteSeek>,
        spec: StreamSpec,
        bits: u16,
        dither: Dither,
        seed: u64,
    ) -> Result<FlacEncoder> {
        if spec.channels > 8 {
            return Err(Error::unsupported("FLAC supports at most 8 channels"));
        }
        let config = flacenc::config::Encoder::default().into_verified().map_err(|e| ferr(e.1))?;
        let info = StreamInfo::new(spec.sample_rate as usize, spec.channels, bits as usize).map_err(ferr)?;
        let mut enc = FlacEncoder {
            w: BufWriter::with_capacity(1 << 16, sink),
            spec,
            bits,
            q: Quantizer::new(bits, dither, seed),
            config,
            fb: FrameBuf::with_size(spec.channels, BLOCK).map_err(ferr)?,
            ctx: Context::new(bits as usize, spec.channels),
            info,
            pending: Vec::new(),
            ints: Vec::new(),
            frame_no: 0,
            frames: 0,
            bytes: 0,
        };
        let h = header_bytes(&enc.info)?;
        enc.w.write_all(&h).map_err(io_err)?;
        Ok(enc)
    }

    fn encode_block(&mut self, interleaved: &[i32]) -> Result<()> {
        (&mut self.fb, &mut self.ctx).fill_interleaved(interleaved).map_err(ferr)?;
        let frame =
            flacenc::encode_fixed_size_frame(&self.config, &self.fb, self.frame_no, &self.info).map_err(ferr)?;
        let mut sink = MemSink::<u8>::with_capacity(interleaved.len() * 16);
        frame.write(&mut sink).map_err(ferr)?;
        let mut b = sink.into_inner();
        let ch = self.spec.channels;
        let block = interleaved.len() / ch;
        // flacenc caps the Rice parameter at 14, which can make very noisy high-resolution
        // blocks far larger than raw PCM; fall back to a verbatim frame when that happens.
        let verbatim_bytes = block * ch * self.bits as usize / 8 + 32;
        let frame = if b.len() > verbatim_bytes {
            let header = FrameHeader::new(
                block,
                ChannelAssignment::Independent(ch as u8),
                self.bits as usize,
                self.spec.sample_rate as usize,
                FrameOffset::Frame(self.frame_no as u32),
            )
            .map_err(ferr)?;
            let mut subframes = Vec::with_capacity(ch);
            for c in 0..ch {
                let samples: Vec<i32> = interleaved.iter().skip(c).step_by(ch).copied().collect();
                subframes.push(Verbatim::new(&samples, self.bits as usize).map_err(ferr)?.into());
            }
            let vf = Frame::new(header, subframes.into_iter()).map_err(ferr)?;
            let mut sink = MemSink::<u8>::with_capacity(verbatim_bytes * 8);
            vf.write(&mut sink).map_err(ferr)?;
            b = sink.into_inner();
            vf
        } else {
            frame
        };
        self.info.update_frame_info(&frame);
        self.bytes += b.len() as u64;
        self.w.write_all(&b).map_err(io_err)?;
        self.frame_no += 1;
        Ok(())
    }
}

impl StreamEncoder for FlacEncoder {
    fn write(&mut self, data: &[&[f32]]) -> Result<()> {
        let ch = self.spec.channels;
        let mut ints = std::mem::take(&mut self.ints);
        self.q.quantize(&data[..ch.min(data.len())], &mut ints);
        self.frames += (ints.len() / ch) as u64;
        self.pending.extend_from_slice(&ints);
        self.ints = ints;
        let block = BLOCK * ch;
        let mut start = 0;
        let pending = std::mem::take(&mut self.pending);
        while pending.len() - start >= block {
            self.encode_block(&pending[start..start + block])?;
            start += block;
        }
        self.pending = pending[start..].to_vec();
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<EncodeReport> {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.encode_block(&rest)?;
        }
        if self.frame_no == 0 {
            self.info.set_block_sizes(BLOCK, BLOCK).map_err(ferr)?;
            self.info.set_frame_sizes(0, 0).map_err(ferr)?;
        } else if self.frame_no == 1 {
            // a single short frame: mirror the reference encoder (min = max = block size)
            let bs = self.info.max_block_size().max(16);
            self.info.set_block_sizes(bs, bs).map_err(ferr)?;
        } else {
            self.info.set_block_sizes(BLOCK, BLOCK).map_err(ferr)?;
        }
        self.info.set_md5_digest(&self.ctx.md5_digest());
        self.info.set_total_samples(self.frames as usize);
        let h = header_bytes(&self.info)?;
        let w = &mut self.w;
        w.seek(SeekFrom::Start(0)).map_err(io_err)?;
        w.write_all(&h).map_err(io_err)?;
        w.seek(SeekFrom::End(0)).map_err(io_err)?;
        w.flush().map_err(io_err)?;
        let secs = self.frames as f64 / self.spec.sample_rate.max(1) as f64;
        Ok(EncodeReport {
            codec: "flac".into(),
            bit_depth: Some(self.bits),
            bitrate: (secs > 0.0).then(|| ((self.bytes + 42) as f64 * 8.0 / secs) as u32),
            frames: self.frames,
            clipped: self.q.clipped,
        })
    }
}
