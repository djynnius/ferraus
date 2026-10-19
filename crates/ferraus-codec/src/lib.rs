//! # ferraus-codec
//!
//! Decoding and encoding for every supported container.
//!
//! * Decoding: Symphonia (WAV, AIFF, FLAC, MP3, AAC/ALAC in MP4/M4A, Vorbis, ADPCM, CAF,
//!   MKV/WebM) plus Opus through a run-time-loaded libopus, including the audio track of
//!   MP4/MOV/MKV/WebM video files. Anything else falls back to the FFmpeg bridge when present.
//! * Encoding: native WAV, AIFF, FLAC (bit-exact, pure Rust), Ogg Vorbis, Ogg Opus, and MP3
//!   (run-time-loaded LAME); `.m4a` (AAC/ALAC) and other formats go through the FFmpeg bridge.
//! * [`CodecIo`] implements `ferraus_core::AudioIo` for the CLI and MCP server.
//! * [`convert_streaming`] converts packet by packet with bounded memory.
//! * Ops: `info`, `convert`, `tags` (see [`ops`]).

pub mod analysis;
pub mod decode;
mod dynlib;
pub mod encode;
pub mod ffmpeg_bridge;
pub mod lame;
mod ops;
pub mod opus;
pub mod pcm;
pub mod tags;

pub use decode::{ArtInfo, DecodeStream, TrackInfo};
pub use encode::{EncodeReport, Format};
pub use ops::ops;

use encode::{native_encoder, SharedBuf, StreamSpec};
use ferraus_core::ctx::{AudioIo, AudioReader, AudioWriter, EncodeOptions, SourceInfo};
use ferraus_core::resample::{resample, Quality};
use ferraus_core::{AudioBuffer, Error, ErrorKind, Result};
use std::path::Path;

/// The production `AudioIo` used by the CLI and MCP server.
pub struct CodecIo;

impl AudioIo for CodecIo {
    fn read(&self, path: &Path) -> Result<(AudioBuffer, SourceInfo)> {
        read_path(path, None)
    }
    fn read_bytes(&self, bytes: &[u8], hint: Option<&str>) -> Result<(AudioBuffer, SourceInfo)> {
        DecodeStream::from_bytes(bytes.to_vec(), hint, None)?.read_all()
    }
    fn probe(&self, path: &Path) -> Result<SourceInfo> {
        probe_info(path)
    }
    fn write(&self, path: &Path, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<()> {
        write_file(path, buf, opts).map(|_| ())
    }
    fn write_bytes(&self, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<Vec<u8>> {
        write_bytes(buf, opts).map(|(b, _)| b)
    }
    fn can_encode(&self, ext: &str) -> bool {
        can_encode(ext)
    }
    fn open_reader(&self, path: &Path) -> Result<Option<Box<dyn AudioReader>>> {
        match DecodeStream::open(path, None) {
            Ok(ds) => Ok(Some(Box::new(StreamReader { ds, channels: 0 }))),
            // read() falls back to FFmpeg for these; so does the caller of a `None`
            Err(e) if e.kind == ErrorKind::Unsupported && ffmpeg_bridge::locate().is_some() => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn open_writer(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: usize,
        frames: u64,
        opts: &EncodeOptions,
    ) -> Result<Option<Box<dyn AudioWriter>>> {
        let fmt = resolve_format(Some(path), opts)?;
        let rate = fmt.target_rate(sample_rate);
        if rate != sample_rate || needs_bridge(&fmt, rate)? {
            return Ok(None);
        }
        let file = std::fs::File::create(path)?;
        let spec = StreamSpec { sample_rate, channels, total_frames: Some(frames) };
        let enc = native_encoder(&fmt, Box::new(file), spec, opts)?;
        Ok(Some(Box::new(StreamWriter { enc, path: path.to_path_buf(), tags: opts.tags.clone() })))
    }
}

/// [`DecodeStream`] as an `AudioReader`: chunks converted to the first chunk's channel count,
/// exactly as [`DecodeStream::read_all`] concatenates them.
struct StreamReader {
    ds: DecodeStream,
    channels: usize,
}

impl AudioReader for StreamReader {
    fn info(&self) -> &SourceInfo {
        &self.ds.info
    }
    fn frames_hint(&self) -> Option<u64> {
        self.ds.n_frames
    }
    fn next_chunk(&mut self) -> Result<Option<AudioBuffer>> {
        let Some(b) = self.ds.next_chunk()? else { return Ok(None) };
        if self.channels == 0 {
            self.channels = b.channels();
        }
        Ok(Some(if b.channels() != self.channels { b.with_channels(self.channels) } else { b }))
    }
}

/// A native encoder as an `AudioWriter` (what [`write_file`] does, block by block).
struct StreamWriter {
    enc: Box<dyn encode::StreamEncoder>,
    path: std::path::PathBuf,
    tags: std::collections::BTreeMap<String, String>,
}

impl AudioWriter for StreamWriter {
    fn write(&mut self, planes: &[&[f32]]) -> Result<()> {
        self.enc.write(planes)
    }
    fn finish(self: Box<Self>) -> Result<()> {
        let StreamWriter { enc, path, tags } = *self;
        enc.finish()?;
        if !tags.is_empty() {
            let _ = tags::apply(&path, &tags);
        }
        Ok(())
    }
}

/// True when this build (and machine) can write `ext`.
pub fn can_encode(ext: &str) -> bool {
    match Format::from_name(ext) {
        None => false,
        Some(Format::Bridge(_)) => ffmpeg_bridge::locate().is_some(),
        Some(f @ (Format::Mp3 | Format::Opus)) => f.native_available() || ffmpeg_bridge::locate().is_some(),
        Some(f) => f.native_available(),
    }
}

/// Extensions this build can decode natively (FFmpeg may add more).
pub const DECODE_EXTENSIONS: &[&str] = &[
    "wav", "wave", "aif", "aiff", "aifc", "flac", "mp3", "mp2", "m4a", "m4b", "mp4", "mov", "aac", "ogg", "oga",
    "opus", "mkv", "mka", "webm", "caf",
];

/// Decode a file, selecting the n-th audio track; falls back to FFmpeg for unsupported codecs.
pub fn read_path(path: &Path, track: Option<usize>) -> Result<(AudioBuffer, SourceInfo)> {
    match DecodeStream::open(path, track) {
        Ok(ds) => ds.read_all(),
        Err(e) if e.kind == ErrorKind::Unsupported && ffmpeg_bridge::locate().is_some() => {
            let buf = ffmpeg_bridge::decode_file(path, track).map_err(|_| e.clone())?;
            let ext = path.extension().map(|x| x.to_string_lossy().to_string());
            let info = SourceInfo {
                path: path.display().to_string(),
                container: ext,
                codec: Some("ffmpeg".into()),
                duration_s: buf.duration_s(),
                sample_rate: buf.sample_rate,
                channels: buf.channels(),
                ..Default::default()
            };
            Ok((buf, info))
        }
        Err(e) => Err(e),
    }
}

/// Describe a file without decoding all of it (decodes only when the container has no length).
pub fn probe_info(path: &Path) -> Result<SourceInfo> {
    let mut ds = DecodeStream::open(path, None)?;
    if ds.info.duration_s <= 0.0 {
        if let Ok(tf) = lofty::read_from_path(path) {
            use lofty::prelude::AudioFile;
            let d = tf.properties().duration().as_secs_f64();
            if d > 0.0 {
                ds.info.duration_s = d;
            }
        }
    }
    if ds.info.duration_s <= 0.0 {
        let mut frames = 0u64;
        let mut sr = ds.info.sample_rate;
        while let Some(b) = ds.next_chunk()? {
            frames += b.frames() as u64;
            sr = b.sample_rate;
        }
        if sr > 0 {
            ds.info.duration_s = frames as f64 / sr as f64;
            ds.info.sample_rate = sr;
        }
    }
    let mut info = ds.info;
    if info.bitrate.is_none() && info.duration_s > 0.0 {
        if let Ok(m) = std::fs::metadata(path) {
            info.bitrate = Some((m.len() as f64 * 8.0 / info.duration_s) as u32);
        }
    }
    Ok(info)
}

fn resolve_format(path: Option<&Path>, opts: &EncodeOptions) -> Result<Format> {
    let name = opts
        .format
        .clone()
        .or_else(|| path.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "wav".into());
    Format::from_name(&name).ok_or_else(|| {
        Error::unsupported(format!(
            "cannot encode '.{name}'; supported: wav, aiff, flac, ogg, opus, mp3, and m4a/aac/alac via the FFmpeg bridge"
        ))
        .with_hint("ferraus convert in.wav -o out.flac")
    })
}

/// True when the native encoder can't be used and FFmpeg must encode.
fn needs_bridge(fmt: &Format, sample_rate: u32) -> Result<bool> {
    if sample_rate > fmt.native_max_rate() {
        if ffmpeg_bridge::locate().is_some() {
            return Ok(true);
        }
        return Err(Error::unsupported(format!(
            "the native {} encoder supports up to {} Hz; {sample_rate} Hz needs the FFmpeg bridge",
            fmt.name(),
            fmt.native_max_rate()
        ))
        .with_hint(ffmpeg_bridge::INSTALL_HINT));
    }
    match fmt {
        Format::Bridge(e) => {
            if ffmpeg_bridge::locate().is_none() {
                return Err(ffmpeg_bridge::missing(&format!("encoding .{e}")));
            }
            Ok(true)
        }
        Format::Mp3 | Format::Opus if !fmt.native_available() => {
            if ffmpeg_bridge::locate().is_some() {
                Ok(true)
            } else if *fmt == Format::Mp3 {
                Err(encode::mp3::unavailable())
            } else {
                Err(Error::unsupported(
                    "Opus encoding needs libopus (loaded at run time) or the FFmpeg bridge; neither was found",
                )
                .with_hint(ffmpeg_bridge::INSTALL_HINT))
            }
        }
        _ => Ok(false),
    }
}

fn prepare_buffer<'a>(fmt: &Format, buf: &'a AudioBuffer) -> std::borrow::Cow<'a, AudioBuffer> {
    let rate = fmt.target_rate(buf.sample_rate);
    if rate != buf.sample_rate {
        std::borrow::Cow::Owned(resample(buf, rate, Quality::High))
    } else {
        std::borrow::Cow::Borrowed(buf)
    }
}

/// Encode a whole buffer to a file. Returns what the encoder reports.
pub fn write_file(path: &Path, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<EncodeReport> {
    let fmt = resolve_format(Some(path), opts)?;
    if needs_bridge(&fmt, fmt.target_rate(buf.sample_rate))? {
        let mut o = opts.clone();
        o.format = Some(fmt.name().to_string());
        ffmpeg_bridge::encode_file(buf, path, &o)?;
        return Ok(EncodeReport {
            codec: format!("{} (ffmpeg)", fmt.name()),
            frames: buf.frames() as u64,
            ..Default::default()
        });
    }
    let b = prepare_buffer(&fmt, buf);
    let file = std::fs::File::create(path)?;
    let spec = StreamSpec { sample_rate: b.sample_rate, channels: b.channels(), total_frames: Some(b.frames() as u64) };
    let mut enc = native_encoder(&fmt, Box::new(file), spec, opts)?;
    encode::encode_all(enc.as_mut(), &b.data)?;
    let report = enc.finish()?;
    if !opts.tags.is_empty() {
        let _ = tags::apply(path, &opts.tags);
    }
    Ok(report)
}

/// Encode a whole buffer to bytes (stdout).
pub fn write_bytes(buf: &AudioBuffer, opts: &EncodeOptions) -> Result<(Vec<u8>, EncodeReport)> {
    let fmt = resolve_format(None, opts)?;
    if needs_bridge(&fmt, fmt.target_rate(buf.sample_rate))? {
        let tmp = std::env::temp_dir().join(format!("ferraus-{}-{}.{}", std::process::id(), nanos(), fmt.name()));
        let r = write_file(&tmp, buf, opts);
        let bytes = std::fs::read(&tmp);
        let _ = std::fs::remove_file(&tmp);
        return Ok((bytes?, r?));
    }
    let b = prepare_buffer(&fmt, buf);
    let sink = SharedBuf::default();
    let spec = StreamSpec { sample_rate: b.sample_rate, channels: b.channels(), total_frames: Some(b.frames() as u64) };
    let mut enc = native_encoder(&fmt, Box::new(sink.clone()), spec, opts)?;
    encode::encode_all(enc.as_mut(), &b.data)?;
    let report = enc.finish()?;
    Ok((sink.take(), report))
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// Statistics from [`convert_streaming`].
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct StreamStats {
    pub frames: u64,
    pub chunks: u64,
    /// Largest decoded chunk, in frames (memory stays proportional to this, not file length).
    pub max_chunk_frames: usize,
    pub skipped_packets: u64,
    #[serde(skip)]
    pub report: EncodeReport,
    #[serde(skip)]
    pub source: SourceInfo,
}

/// True when `convert` can stream `src_rate` audio to `fmt` (no resampling, native encoder).
pub fn can_stream(fmt: &Format, src_rate: u32, out_rate: Option<u32>) -> bool {
    fmt.native_available()
        && src_rate <= fmt.native_max_rate()
        && out_rate.is_none_or(|r| r == src_rate)
        && fmt.target_rate(src_rate) == src_rate
}

/// Decode `input` packet by packet and encode to `output` without holding the whole
/// signal in memory. `channels` remixes each chunk; `progress` is called with the fraction
/// done and may abort by returning an error.
pub fn convert_streaming(
    input: &Path,
    output: &Path,
    opts: &EncodeOptions,
    channels: Option<usize>,
    track: Option<usize>,
    progress: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<StreamStats> {
    let mut ds = DecodeStream::open(input, track)?;
    let fmt = resolve_format(Some(output), opts)?;
    if !can_stream(&fmt, ds.info.sample_rate, None) {
        return Err(Error::internal(format!("{} cannot be streamed at {} Hz", fmt.name(), ds.info.sample_rate)));
    }
    let ch = channels.unwrap_or(ds.info.channels).max(1);
    let file = std::fs::File::create(output)?;
    let spec = StreamSpec { sample_rate: ds.info.sample_rate, channels: ch, total_frames: ds.n_frames };
    let mut enc = native_encoder(&fmt, Box::new(file), spec, opts)?;
    let mut stats = StreamStats::default();
    let total = ds.n_frames.unwrap_or(0);
    while let Some(chunk) = ds.next_chunk()? {
        let chunk = if chunk.channels() != ch { chunk.with_channels(ch) } else { chunk };
        stats.frames += chunk.frames() as u64;
        stats.chunks += 1;
        stats.max_chunk_frames = stats.max_chunk_frames.max(chunk.frames());
        let planes: Vec<&[f32]> = chunk.data.iter().map(|c| c.as_slice()).collect();
        enc.write(&planes)?;
        if stats.chunks % 64 == 0 {
            progress(if total > 0 { stats.frames as f64 / total as f64 } else { 0.0 })?;
        }
    }
    stats.report = enc.finish()?;
    stats.skipped_packets = ds.skipped_packets;
    let mut info = ds.info.clone();
    info.duration_s = stats.frames as f64 / info.sample_rate.max(1) as f64;
    stats.source = info;
    if !opts.tags.is_empty() {
        let _ = tags::apply(output, &opts.tags);
    }
    progress(1.0)?;
    Ok(stats)
}
