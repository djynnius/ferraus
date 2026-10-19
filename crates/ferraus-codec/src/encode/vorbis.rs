//! Ogg Vorbis encoding via `vorbis_rs` (libvorbis/aoTuV, BSD). `--vbr on` targets the
//! bitrate in quality-VBR mode; `--vbr off` uses managed ABR.

use super::{EncodeReport, StreamEncoder, StreamSpec, WriteSeek};
use ferraus_core::{Error, Result};
use std::num::{NonZeroU32, NonZeroU8};
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoder, VorbisEncoderBuilder};

pub struct VorbisEnc {
    enc: Option<VorbisEncoder<Box<dyn WriteSeek>>>,
    spec: StreamSpec,
    bitrate: u32,
    frames: u64,
}

fn verr(e: vorbis_rs::VorbisError) -> Error {
    Error::processing(format!("Vorbis encoder: {e}"))
}

impl VorbisEnc {
    pub fn new(
        sink: Box<dyn WriteSeek>,
        spec: StreamSpec,
        bitrate: u32,
        vbr: bool,
        seed: u64,
    ) -> Result<VorbisEnc> {
        let sr = NonZeroU32::new(spec.sample_rate).ok_or_else(|| Error::usage("sample rate 0"))?;
        let ch = u8::try_from(spec.channels)
            .ok()
            .and_then(NonZeroU8::new)
            .ok_or_else(|| Error::unsupported("Vorbis supports 1-255 channels"))?;
        let br = NonZeroU32::new(bitrate.clamp(32_000, 500_000)).unwrap();
        let strategy = if vbr {
            VorbisBitrateManagementStrategy::Vbr { target_bitrate: br }
        } else {
            VorbisBitrateManagementStrategy::Abr { average_bitrate: br }
        };
        // deterministic stream serial derived from the seed
        let serial = (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) as i32;
        let enc = VorbisEncoderBuilder::new_with_serial(sr, ch, sink, serial)
            .bitrate_management_strategy(strategy)
            .build()
            .map_err(verr)?;
        Ok(VorbisEnc { enc: Some(enc), spec, bitrate, frames: 0 })
    }
}

impl StreamEncoder for VorbisEnc {
    fn write(&mut self, data: &[&[f32]]) -> Result<()> {
        let n = data.first().map(|c| c.len()).unwrap_or(0);
        if n == 0 {
            return Ok(());
        }
        let enc = self.enc.as_mut().unwrap();
        let ch = self.spec.channels.min(data.len());
        let mut s = 0;
        while s < n {
            let e = (s + 4096).min(n);
            let block: Vec<&[f32]> = data[..ch].iter().map(|c| &c[s..e]).collect();
            enc.encode_audio_block(&block).map_err(verr)?;
            s = e;
        }
        self.frames += n as u64;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<EncodeReport> {
        let mut w = self.enc.take().unwrap().finish().map_err(verr)?;
        std::io::Write::flush(&mut w)?;
        Ok(EncodeReport {
            codec: "vorbis".into(),
            bit_depth: None,
            bitrate: Some(self.bitrate),
            frames: self.frames,
            clipped: 0,
        })
    }
}
