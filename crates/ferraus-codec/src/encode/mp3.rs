//! MP3 output through the dynamically loaded LAME encoder.

use super::{io_err, EncodeReport, StreamEncoder, StreamSpec, WriteSeek};
use crate::lame::LameEncoder;
use ferraus_core::{Error, Result};
use std::io::{BufWriter, Seek, SeekFrom, Write};

pub struct Mp3Encoder {
    enc: LameEncoder,
    w: BufWriter<Box<dyn WriteSeek>>,
    channels: usize,
    bitrate: u32,
    frames: u64,
    start: u64,
}

/// The error returned when MP3 is requested but neither LAME nor FFmpeg is available.
pub fn unavailable() -> Error {
    Error::unsupported("MP3 encoding needs libmp3lame (loaded at run time) or the FFmpeg bridge; neither was found")
        .with_hint(crate::ffmpeg_bridge::INSTALL_HINT)
}

impl Mp3Encoder {
    pub fn new(sink: Box<dyn WriteSeek>, spec: StreamSpec, bitrate: u32, vbr: bool) -> Result<Mp3Encoder> {
        if spec.channels > 2 {
            return Err(Error::unsupported(format!("MP3 supports 1 or 2 channels, got {}", spec.channels))
                .with_hint("add --channels 2"));
        }
        if crate::lame::lib().is_none() {
            return Err(unavailable());
        }
        let enc = LameEncoder::new(spec.sample_rate, spec.channels, bitrate, vbr)
            .map_err(|e| Error::unsupported(format!("MP3 encoder: {e}")))?;
        let mut w = BufWriter::with_capacity(1 << 16, sink);
        let start = w.stream_position().map_err(io_err)?;
        Ok(Mp3Encoder { enc, w, channels: spec.channels, bitrate, frames: 0, start })
    }
}

impl StreamEncoder for Mp3Encoder {
    fn write(&mut self, data: &[&[f32]]) -> Result<()> {
        let n = data.first().map(|c| c.len()).unwrap_or(0);
        let mut s = 0;
        while s < n {
            let e = (s + 8192).min(n);
            let l = &data[0][s..e];
            let r = if self.channels == 2 { data.get(1).map(|c| &c[s..e]) } else { None };
            let bytes = self.enc.encode(l, r).map_err(Error::processing)?;
            self.w.write_all(bytes).map_err(io_err)?;
            s = e;
        }
        self.frames += n as u64;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<EncodeReport> {
        let tail = self.enc.flush().map_err(Error::processing)?;
        self.w.write_all(&tail).map_err(io_err)?;
        let tag = self.enc.lametag();
        if !tag.is_empty() {
            self.w.seek(SeekFrom::Start(self.start)).map_err(io_err)?;
            self.w.write_all(&tag).map_err(io_err)?;
            self.w.seek(SeekFrom::End(0)).map_err(io_err)?;
        }
        self.w.flush().map_err(io_err)?;
        Ok(EncodeReport {
            codec: "mp3".into(),
            bit_depth: None,
            bitrate: Some(self.bitrate),
            frames: self.frames,
            clipped: 0,
        })
    }
}
