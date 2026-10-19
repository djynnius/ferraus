//! Decoding through Symphonia (plus the libopus decoder), as a packet-by-packet stream so
//! long files never have to be held in memory. Video containers are demuxed for their
//! audio: the default (or `track`-selected) audio track is decoded and every other stream
//! is ignored.

use ferraus_core::ctx::SourceInfo;
use ferraus_core::{AudioBuffer, Error, ErrorKind, Result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;
use symphonia::core::audio::AudioBufferRef;
use symphonia::core::codecs::{CodecRegistry, Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;

/// Symphonia's codecs plus the libopus-backed Opus decoder.
pub fn codecs() -> &'static CodecRegistry {
    static REG: OnceLock<CodecRegistry> = OnceLock::new();
    REG.get_or_init(|| {
        let mut r = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut r);
        r.register_all::<crate::opus::SymOpusDecoder>();
        r
    })
}

/// Embedded picture summary.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ArtInfo {
    pub mime: String,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
}

/// One elementary stream as Symphonia sees it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TrackInfo {
    pub index: usize,
    pub id: u32,
    pub kind: String,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    pub default: bool,
}

/// Sniff the container from the first bytes.
pub fn sniff_container(head: &[u8], ext: Option<&str>) -> Option<String> {
    let ext = ext.map(|e| e.to_ascii_lowercase());
    let c = if head.len() >= 12 && (&head[0..4] == b"RIFF" || &head[0..4] == b"RF64") && &head[8..12] == b"WAVE" {
        "wav"
    } else if head.len() >= 12 && &head[0..4] == b"FORM" && (&head[8..12] == b"AIFF" || &head[8..12] == b"AIFC") {
        "aiff"
    } else if head.starts_with(b"fLaC") {
        "flac"
    } else if head.starts_with(b"OggS") {
        "ogg"
    } else if head.starts_with(b"caff") {
        "caf"
    } else if head.len() >= 4 && head[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        let window = &head[..head.len().min(64)];
        if window.windows(4).any(|w| w == b"webm") {
            "webm"
        } else {
            "matroska"
        }
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        match &head[8..12] {
            b"qt  " => "mov",
            b"M4A " | b"M4B " | b"M4P " => "m4a",
            _ => match ext.as_deref() {
                Some("m4a") | Some("m4b") => "m4a",
                Some("mov") => "mov",
                _ => "mp4",
            },
        }
    } else if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0) {
        match ext.as_deref() {
            Some("aac") => "adts",
            Some("mp2") => "mp2",
            _ => "mp3",
        }
    } else {
        return ext.filter(|e| !e.is_empty());
    };
    Some(c.to_string())
}

fn std_key_name(k: StandardTagKey) -> Option<&'static str> {
    use StandardTagKey::*;
    Some(match k {
        TrackTitle => "title",
        Artist => "artist",
        Album => "album",
        AlbumArtist => "album_artist",
        Date | ReleaseDate | OriginalDate => "date",
        Genre => "genre",
        TrackNumber => "track",
        TrackTotal => "track_total",
        DiscNumber => "disc",
        DiscTotal => "disc_total",
        Comment => "comment",
        Composer => "composer",
        Bpm => "bpm",
        Copyright => "copyright",
        Encoder => "encoder",
        EncodedBy => "encoded_by",
        Lyrics => "lyrics",
        Label => "label",
        IdentIsrc => "isrc",
        Conductor => "conductor",
        Lyricist => "lyricist",
        Language => "language",
        _ => return None,
    })
}

fn collect_meta(rev: &MetadataRevision, tags: &mut BTreeMap<String, String>, art: &mut Vec<ArtInfo>) {
    for t in rev.tags() {
        let key = t.std_key.and_then(std_key_name).map(|s| s.to_string()).unwrap_or_else(|| t.key.to_ascii_lowercase());
        let val = t.value.to_string().trim_end_matches('\0').trim().to_string();
        if !val.is_empty() && !key.is_empty() {
            tags.entry(key).or_insert(val);
        }
    }
    for v in rev.visuals() {
        art.push(ArtInfo { mime: v.media_type.clone(), bytes: v.data.len(), usage: v.usage.map(|u| format!("{u:?}")) });
    }
}

/// True for lossy codecs (used to pick a default output bit depth).
pub fn is_lossy_codec(codec: &str) -> bool {
    matches!(codec, "mp3" | "mp2" | "mp1" | "aac" | "vorbis" | "opus" | "adpcm_ima" | "adpcm_ms" | "ac3" | "wma")
        || codec.starts_with("adpcm")
}

fn sym_err(e: SymError, what: &str) -> Error {
    match e {
        SymError::IoError(io) => {
            if io.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::new(ErrorKind::InputMissing, "input_truncated", format!("{what}: file is truncated or empty"))
            } else {
                Error::from(io)
            }
        }
        SymError::Unsupported(m) => Error::unsupported(format!("{what}: unsupported format or codec ({m})")),
        SymError::DecodeError(m) => {
            Error::new(ErrorKind::InputMissing, "input_corrupt", format!("{what}: corrupt data ({m})"))
        }
        SymError::LimitError(m) => Error::new(ErrorKind::InputMissing, "input_corrupt", format!("{what}: {m}")),
        SymError::ResetRequired => Error::processing(format!("{what}: decoder reset required")),
        SymError::SeekError(_) => Error::processing(format!("{what}: seek error")),
    }
}

/// A packet-by-packet decoder over one audio track.
pub struct DecodeStream {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    /// Source description (duration may be refined after a full decode).
    pub info: SourceInfo,
    /// Embedded pictures.
    pub art: Vec<ArtInfo>,
    /// Every stream in the container.
    pub tracks: Vec<TrackInfo>,
    /// Total frames if the container declares it.
    pub n_frames: Option<u64>,
    /// Packets that failed to decode and were skipped.
    pub skipped_packets: u64,
    decoded: u64,
    label: String,
}

impl DecodeStream {
    /// Open a file. `track` picks the n-th audio track (default: the container's default).
    pub fn open(path: &Path, track: Option<usize>) -> Result<DecodeStream> {
        let mut f = File::open(path)?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        if size == 0 {
            return Err(Error::new(ErrorKind::InputMissing, "input_empty", format!("{} is empty", path.display())));
        }
        let mut head = vec![0u8; 64];
        let n = read_up_to(&mut f, &mut head)?;
        head.truncate(n);
        drop(f);
        let ext = path.extension().map(|e| e.to_string_lossy().to_string());
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        Self::from_source(mss, ext.as_deref(), &head, size, path.display().to_string(), track)
    }

    /// Decode from memory (stdin) with an optional format hint (`wav`, `mp3`, ...).
    pub fn from_bytes(bytes: Vec<u8>, hint: Option<&str>, track: Option<usize>) -> Result<DecodeStream> {
        if bytes.is_empty() {
            return Err(Error::new(ErrorKind::InputMissing, "input_empty", "input is empty"));
        }
        let head = bytes[..bytes.len().min(64)].to_vec();
        let size = bytes.len() as u64;
        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), MediaSourceStreamOptions::default());
        Self::from_source(mss, hint, &head, size, "-".to_string(), track)
    }

    fn from_source(
        mss: MediaSourceStream,
        ext: Option<&str>,
        head: &[u8],
        size: u64,
        label: String,
        track: Option<usize>,
    ) -> Result<DecodeStream> {
        let mut hint = Hint::new();
        if let Some(e) = ext {
            hint.with_extension(e);
        }
        let fmt_opts = FormatOptions { enable_gapless: true, ..Default::default() };
        let mut probed = symphonia::default::get_probe()
            .format(&hint, mss, &fmt_opts, &MetadataOptions::default())
            .map_err(|e| match e {
                SymError::Unsupported(_) => {
                    Error::unsupported(format!("{label}: not a recognized audio or media file"))
                }
                other => sym_err(other, &label),
            })?;
        let mut tags = BTreeMap::new();
        let mut art = Vec::new();
        if let Some(rev) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
            collect_meta(rev, &mut tags, &mut art);
        }
        let mut reader = probed.format;
        if let Some(rev) = reader.metadata().current() {
            collect_meta(rev, &mut tags, &mut art);
        }
        let default_id = reader.default_track().map(|t| t.id);
        let mut tracks = Vec::new();
        let mut audio: Vec<u32> = Vec::new();
        for (i, t) in reader.tracks().iter().enumerate() {
            let p = &t.codec_params;
            let is_audio = p.codec != CODEC_TYPE_NULL;
            if is_audio {
                audio.push(t.id);
            }
            let codec = codecs().get_codec(p.codec).map(|d| d.short_name.to_string()).unwrap_or_else(|| {
                if p.codec == CODEC_TYPE_NULL {
                    "unknown".into()
                } else {
                    format!("{}", p.codec)
                }
            });
            tracks.push(TrackInfo {
                index: i,
                id: t.id,
                kind: if is_audio { "audio".into() } else { "data".into() },
                codec,
                language: t.language.clone(),
                sample_rate: p.sample_rate,
                channels: p.channels.map(|c| c.count()),
                duration_s: match (p.n_frames, p.sample_rate) {
                    (Some(n), Some(sr)) if sr > 0 => Some(n as f64 / sr as f64),
                    _ => None,
                },
                default: Some(t.id) == default_id,
            });
        }
        if audio.is_empty() {
            return Err(Error::unsupported(format!("{label}: no audio track found")));
        }
        let track_id = match track {
            Some(n) => *audio.get(n).ok_or_else(|| {
                Error::usage(format!("{label} has {} audio track(s); --track {n} is out of range", audio.len()))
            })?,
            None => default_id.filter(|d| audio.contains(d)).unwrap_or(audio[0]),
        };
        let t = reader.tracks().iter().find(|t| t.id == track_id).unwrap();
        let params = t.codec_params.clone();
        let decoder = codecs().make(&params, &DecoderOptions::default()).map_err(|e| {
            let name = tracks.iter().find(|x| x.id == track_id).map(|x| x.codec.clone()).unwrap_or_default();
            match e {
                SymError::Unsupported(m) => {
                    let mut err = Error::unsupported(format!("{label}: no decoder for codec '{name}' ({m})"));
                    if name == "opus" {
                        err = err.with_hint("install libopus (e.g. brew install opus / apt install libopus0) or run: ferraus doctor --install-ffmpeg");
                    }
                    err
                }
                other => sym_err(other, &label),
            }
        })?;
        let dp = decoder.codec_params();
        let sr = params.sample_rate.or(dp.sample_rate).unwrap_or(0);
        let channels = params.channels.or(dp.channels).map(|c| c.count()).unwrap_or(0);
        let codec_name = tracks.iter().find(|x| x.id == track_id).map(|x| x.codec.clone());
        let n_frames = params.n_frames;
        let duration_s = match n_frames {
            Some(n) if sr > 0 => n as f64 / sr as f64,
            _ => 0.0,
        };
        let bit_depth = params.bits_per_sample.map(|b| b as u16).or(match params.sample_format {
            Some(symphonia::core::sample::SampleFormat::F32) => Some(32),
            Some(symphonia::core::sample::SampleFormat::F64) => Some(64),
            _ => match codec_name.as_deref() {
                Some(c) if c.starts_with("pcm_f32") => Some(32),
                Some(c) if c.starts_with("pcm_f64") => Some(64),
                _ => None,
            },
        });
        let lossless_pcm = codec_name.as_deref().map(|c| c.starts_with("pcm")).unwrap_or(false);
        let bitrate = if lossless_pcm {
            bit_depth.map(|b| sr * channels as u32 * b as u32)
        } else if duration_s > 0.0 && size > 0 {
            Some((size as f64 * 8.0 / duration_s) as u32)
        } else {
            None
        };
        if !art.is_empty() {
            let a = &art[0];
            tags.insert("embedded_art".into(), format!("{} picture(s), first {} {} bytes", art.len(), a.mime, a.bytes));
        }
        let info = SourceInfo {
            path: label.clone(),
            container: sniff_container(head, ext),
            codec: codec_name,
            duration_s,
            sample_rate: sr,
            channels,
            bit_depth,
            bitrate,
            tags,
        };
        Ok(DecodeStream {
            reader,
            decoder,
            track_id,
            info,
            art,
            tracks,
            n_frames,
            skipped_packets: 0,
            decoded: 0,
            label,
        })
    }

    /// Decode the next packet's worth of audio; `None` at end of stream.
    pub fn next_chunk(&mut self) -> Result<Option<AudioBuffer>> {
        let mut consecutive_errors = 0u32;
        loop {
            let packet = match self.reader.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(SymError::ResetRequired) => return Ok(None),
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) if self.decoded > 0 => return Ok(None),
                Err(e) => return Err(sym_err(e, &self.label)),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let b = to_planar(&buf);
                    if b.frames() == 0 {
                        continue;
                    }
                    if self.info.sample_rate == 0 {
                        self.info.sample_rate = b.sample_rate;
                    }
                    if self.info.channels == 0 {
                        self.info.channels = b.channels();
                    }
                    self.decoded += b.frames() as u64;
                    return Ok(Some(b));
                }
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.skipped_packets += 1;
                    consecutive_errors += 1;
                    if consecutive_errors > 64 && self.decoded == 0 {
                        return Err(Error::new(
                            ErrorKind::InputMissing,
                            "input_corrupt",
                            format!("{}: no decodable audio", self.label),
                        ));
                    }
                }
                Err(e) => return Err(sym_err(e, &self.label)),
            }
        }
    }

    /// Frames decoded so far.
    pub fn decoded_frames(&self) -> u64 {
        self.decoded
    }

    /// Decode everything into one buffer.
    pub fn read_all(mut self) -> Result<(AudioBuffer, SourceInfo)> {
        let cap = self.n_frames.unwrap_or(0).min(1 << 28) as usize;
        let mut data: Vec<Vec<f32>> = Vec::new();
        let mut sr = self.info.sample_rate;
        while let Some(b) = self.next_chunk()? {
            sr = b.sample_rate;
            if data.is_empty() {
                data = (0..b.channels()).map(|_| Vec::with_capacity(cap)).collect();
            }
            let b = if b.channels() != data.len() { b.with_channels(data.len()) } else { b };
            for (d, s) in data.iter_mut().zip(&b.data) {
                d.extend_from_slice(s);
            }
        }
        if data.is_empty() {
            data = vec![Vec::new(); self.info.channels.max(1)];
        }
        let buf = AudioBuffer { sample_rate: sr.max(1), data };
        let mut info = self.info;
        info.sample_rate = buf.sample_rate;
        info.channels = buf.channels();
        info.duration_s = buf.duration_s();
        Ok((buf, info))
    }
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Convert a Symphonia buffer to planar `f32`, exactly for integer formats up to 24 bits.
pub fn to_planar(buf: &AudioBufferRef) -> AudioBuffer {
    macro_rules! conv {
        ($b:expr, $f:expr) => {{
            let b = $b;
            let sr = b.spec().rate;
            let planes = b.planes();
            let data: Vec<Vec<f32>> = planes.planes().iter().map(|p| p.iter().map($f).collect()).collect();
            AudioBuffer { sample_rate: sr, data }
        }};
    }
    const S31: f64 = 2147483648.0;
    match buf {
        AudioBufferRef::U8(b) => conv!(b, |v: &u8| (*v as f32 - 128.0) / 128.0),
        AudioBufferRef::U16(b) => conv!(b, |v: &u16| (*v as f32 - 32768.0) / 32768.0),
        AudioBufferRef::U24(b) => {
            conv!(b, |v: &symphonia::core::sample::u24| (v.inner() as f32 - 8388608.0) / 8388608.0)
        }
        AudioBufferRef::U32(b) => conv!(b, |v: &u32| ((*v as f64 - S31) / S31) as f32),
        AudioBufferRef::S8(b) => conv!(b, |v: &i8| *v as f32 / 128.0),
        AudioBufferRef::S16(b) => conv!(b, |v: &i16| *v as f32 / 32768.0),
        AudioBufferRef::S24(b) => conv!(b, |v: &symphonia::core::sample::i24| v.inner() as f32 / 8388608.0),
        AudioBufferRef::S32(b) => conv!(b, |v: &i32| (*v as f64 / S31) as f32),
        AudioBufferRef::F32(b) => conv!(b, |v: &f32| *v),
        AudioBufferRef::F64(b) => conv!(b, |v: &f64| *v as f32),
    }
}

/// Probe without decoding audio.
pub fn probe(path: &Path) -> Result<DecodeStream> {
    DecodeStream::open(path, None)
}
