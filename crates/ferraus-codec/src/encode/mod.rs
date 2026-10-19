//! Native streaming encoders. Every encoder accepts planar `f32` chunks of any size, so the
//! same code serves whole-buffer writes and the low-memory streaming `convert` path.

pub mod aiff;
pub mod flac;
pub mod mp3;
pub mod opus_ogg;
#[cfg(feature = "vorbis")]
pub mod vorbis;
pub mod wav;

use crate::pcm::Dither;
use ferraus_core::ctx::EncodeOptions;
use ferraus_core::{Error, Result};
use std::io::{Cursor, Seek, Write};
use std::sync::{Arc, Mutex};

/// Output formats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Wav,
    Aiff,
    Flac,
    Vorbis,
    Opus,
    Mp3,
    /// Encoded by the FFmpeg bridge (AAC/ALAC in `.m4a`, AC-3, WMA...). Holds the extension.
    Bridge(String),
}

impl Format {
    /// Map an extension or `--format` value to a format.
    pub fn from_name(name: &str) -> Option<Format> {
        Some(match name.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
            "wav" | "wave" => Format::Wav,
            "aiff" | "aif" | "aifc" => Format::Aiff,
            "flac" => Format::Flac,
            "ogg" | "oga" | "vorbis" => Format::Vorbis,
            "opus" => Format::Opus,
            "mp3" => Format::Mp3,
            e @ ("m4a" | "aac" | "alac" | "caf" | "ac3" | "wma") => Format::Bridge(e.to_string()),
            _ => return None,
        })
    }

    pub fn name(&self) -> &str {
        match self {
            Format::Wav => "wav",
            Format::Aiff => "aiff",
            Format::Flac => "flac",
            Format::Vorbis => "ogg",
            Format::Opus => "opus",
            Format::Mp3 => "mp3",
            Format::Bridge(e) => e,
        }
    }

    pub fn is_lossless(&self) -> bool {
        matches!(self, Format::Wav | Format::Aiff | Format::Flac)
            || matches!(self, Format::Bridge(e) if e == "alac" || e == "caf")
    }

    /// True when this build has a native (in-process) encoder available right now.
    pub fn native_available(&self) -> bool {
        match self {
            Format::Wav | Format::Aiff | Format::Flac => true,
            Format::Vorbis => cfg!(feature = "vorbis"),
            Format::Opus => crate::opus::lib().is_some(),
            Format::Mp3 => crate::lame::lib().is_some(),
            Format::Bridge(_) => false,
        }
    }

    /// Sample rate the encoder needs for input at `sr`.
    pub fn target_rate(&self, sr: u32) -> u32 {
        match self {
            Format::Opus => 48000,
            Format::Mp3 if sr > 48000 => 48000,
            Format::Flac if sr > 655_350 => 192_000,
            _ => sr,
        }
    }
}

/// `192` → 192 kbps; values of 1000 and above are bits per second.
pub fn normalize_bitrate(b: u32) -> u32 {
    if b < 1000 {
        b * 1000
    } else {
        b
    }
}

/// Integer bit depth for PCM formats: `opts.bit_depth`, else 24.
pub fn pcm_bits(opts: &EncodeOptions) -> u16 {
    opts.bit_depth.unwrap_or(24)
}

/// Stream parameters given to an encoder up front.
#[derive(Clone, Copy, Debug)]
pub struct StreamSpec {
    pub sample_rate: u32,
    pub channels: usize,
    /// Total frames if known (lets WAV skip reserving RF64 space).
    pub total_frames: Option<u64>,
}

/// What an encoder reports when done.
#[derive(Clone, Debug, Default)]
pub struct EncodeReport {
    pub codec: String,
    pub bit_depth: Option<u16>,
    pub bitrate: Option<u32>,
    pub frames: u64,
    /// Samples clipped while quantizing.
    pub clipped: u64,
}

/// A seekable byte sink.
pub trait WriteSeek: Write + Seek + Send {}
impl<T: Write + Seek + Send> WriteSeek for T {}

/// An in-memory sink whose bytes can be taken after the encoder consumed it.
#[derive(Clone, Default)]
pub struct SharedBuf(pub Arc<Mutex<Cursor<Vec<u8>>>>);

impl SharedBuf {
    pub fn take(&self) -> Vec<u8> {
        std::mem::take(self.0.lock().unwrap().get_mut())
    }
}

impl Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for SharedBuf {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

/// Incremental encoder.
pub trait StreamEncoder {
    /// Encode one planar chunk (each channel the same length).
    fn write(&mut self, data: &[&[f32]]) -> Result<()>;
    /// Finalize headers and flush.
    fn finish(self: Box<Self>) -> Result<EncodeReport>;
}

pub(crate) fn io_err(e: std::io::Error) -> Error {
    Error::from(e)
}

/// Build a native encoder for `fmt`. The caller must already have resampled to
/// [`Format::target_rate`].
pub fn native_encoder(
    fmt: &Format,
    sink: Box<dyn WriteSeek>,
    spec: StreamSpec,
    opts: &EncodeOptions,
) -> Result<Box<dyn StreamEncoder>> {
    if spec.channels == 0 {
        return Err(Error::usage("cannot encode audio with zero channels"));
    }
    let dither = Dither::parse(opts.dither.as_deref());
    let bitrate = normalize_bitrate(opts.bitrate.unwrap_or(192_000));
    Ok(match fmt {
        Format::Wav => {
            let bits = pcm_bits(opts);
            let float = opts.float || bits == 32 || bits == 64;
            Box::new(wav::WavEncoder::new(sink, spec, bits, float, dither, opts.seed)?)
        }
        Format::Aiff => Box::new(aiff::AiffEncoder::new(sink, spec, pcm_bits(opts), dither, opts.seed)?),
        Format::Flac => {
            let bits = match pcm_bits(opts) {
                b @ (8 | 16 | 24) => b,
                b if b > 24 => 24,
                b => b.max(8),
            };
            Box::new(flac::FlacEncoder::new(sink, spec, bits, dither, opts.seed)?)
        }
        #[cfg(feature = "vorbis")]
        Format::Vorbis => Box::new(vorbis::VorbisEnc::new(sink, spec, bitrate, opts.vbr, opts.seed)?),
        #[cfg(not(feature = "vorbis"))]
        Format::Vorbis => {
            return Err(Error::unsupported("this build has no Vorbis encoder")
                .with_hint(crate::ffmpeg_bridge::INSTALL_HINT))
        }
        Format::Opus => Box::new(opus_ogg::OggOpusEncoder::new(sink, spec, bitrate, opts.vbr, opts.seed)?),
        Format::Mp3 => Box::new(mp3::Mp3Encoder::new(sink, spec, bitrate, opts.vbr)?),
        Format::Bridge(e) => {
            return Err(Error::internal(format!(".{e} has no native encoder")));
        }
    })
}

/// Feed a whole buffer to an encoder in bounded chunks.
pub fn encode_all(enc: &mut dyn StreamEncoder, data: &[Vec<f32>]) -> Result<()> {
    const CHUNK: usize = 65536;
    let n = data.first().map(|c| c.len()).unwrap_or(0);
    let mut s = 0;
    while s < n {
        let e = (s + CHUNK).min(n);
        let chunk: Vec<&[f32]> = data.iter().map(|c| &c[s..e]).collect();
        enc.write(&chunk)?;
        s = e;
    }
    Ok(())
}
