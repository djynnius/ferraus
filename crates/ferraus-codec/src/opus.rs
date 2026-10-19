//! Opus through a system libopus loaded at run time (BSD licensed). Symphonia 0.5 parses
//! Ogg/MKV/MP4 Opus streams but has no Opus decoder, so this module provides one as a
//! Symphonia `Decoder`, plus the encoder used for `.opus` output.

use crate::dynlib;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;
use symphonia::core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia::core::codecs::{
    CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_OPUS,
};
use symphonia::core::errors::{decode_error, unsupported_error, Result as SymResult};
use symphonia::core::formats::Packet;

pub const OPUS_APPLICATION_AUDIO: c_int = 2049;
pub const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
pub const OPUS_SET_VBR_REQUEST: c_int = 4006;
pub const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
pub const OPUS_GET_LOOKAHEAD_REQUEST: c_int = 4027;

type EncCreate = unsafe extern "C" fn(i32, c_int, c_int, *mut c_int) -> *mut c_void;
type EncCtl = unsafe extern "C" fn(*mut c_void, c_int, ...) -> c_int;
type EncFloat = unsafe extern "C" fn(*mut c_void, *const f32, c_int, *mut u8, i32) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);
type MsDecCreate =
    unsafe extern "C" fn(i32, c_int, c_int, c_int, *const u8, *mut c_int) -> *mut c_void;
type MsDecFloat = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut f32, c_int, c_int) -> c_int;
type VersionFn = unsafe extern "C" fn() -> *const c_char;

/// Function table of a loaded libopus.
pub struct OpusLib {
    enc_create: EncCreate,
    enc_ctl: EncCtl,
    enc_float: EncFloat,
    enc_destroy: Destroy,
    dec_create: MsDecCreate,
    dec_float: MsDecFloat,
    dec_destroy: Destroy,
    pub version: String,
    pub path: String,
}

static LIB: OnceLock<Option<OpusLib>> = OnceLock::new();

/// The loaded libopus, if one is available (`FERRAUS_LIBOPUS` overrides the search;
/// `FERRAUS_LIBOPUS_DISABLE=1` disables it).
pub fn lib() -> Option<&'static OpusLib> {
    if !cfg!(feature = "opus-dyn") {
        return None;
    }
    LIB.get_or_init(|| {
        let (lib, path) = dynlib::open("FERRAUS_LIBOPUS", "opus", "0")?;
        let lib: &'static libloading::Library = Box::leak(Box::new(lib));
        // SAFETY: signatures follow opus.h / opus_multistream.h.
        unsafe {
            let version: VersionFn = dynlib::sym(lib, b"opus_get_version_string\0")?;
            let v = CStr::from_ptr(version()).to_string_lossy().to_string();
            Some(OpusLib {
                enc_create: dynlib::sym(lib, b"opus_encoder_create\0")?,
                enc_ctl: dynlib::sym(lib, b"opus_encoder_ctl\0")?,
                enc_float: dynlib::sym(lib, b"opus_encode_float\0")?,
                enc_destroy: dynlib::sym(lib, b"opus_encoder_destroy\0")?,
                dec_create: dynlib::sym(lib, b"opus_multistream_decoder_create\0")?,
                dec_float: dynlib::sym(lib, b"opus_multistream_decode_float\0")?,
                dec_destroy: dynlib::sym(lib, b"opus_multistream_decoder_destroy\0")?,
                version: v,
                path,
            })
        }
    })
    .as_ref()
}

/// A libopus encoder (48 kHz, 1 or 2 channels).
pub struct OpusEncoder {
    lib: &'static OpusLib,
    st: *mut c_void,
    pub channels: usize,
}

// SAFETY: the encoder state is only touched through &mut self.
unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn new(channels: usize, bitrate: i32, vbr: bool) -> Result<OpusEncoder, String> {
        let lib = lib().ok_or("libopus not found")?;
        let mut err: c_int = 0;
        // SAFETY: valid arguments; checked for null/error below.
        let st = unsafe { (lib.enc_create)(48000, channels as c_int, OPUS_APPLICATION_AUDIO, &mut err) };
        if st.is_null() || err != 0 {
            return Err(format!("opus_encoder_create failed ({err})"));
        }
        // SAFETY: valid encoder; ctl requests take an opus_int32 argument.
        unsafe {
            (lib.enc_ctl)(st, OPUS_SET_BITRATE_REQUEST, bitrate);
            (lib.enc_ctl)(st, OPUS_SET_VBR_REQUEST, if vbr { 1i32 } else { 0i32 });
            (lib.enc_ctl)(st, OPUS_SET_COMPLEXITY_REQUEST, 10i32);
        }
        Ok(OpusEncoder { lib, st, channels })
    }

    /// Encoder lookahead in samples (the Ogg Opus pre-skip).
    pub fn lookahead(&mut self) -> u32 {
        let mut v: i32 = 312;
        // SAFETY: GET_LOOKAHEAD writes one opus_int32.
        unsafe {
            (self.lib.enc_ctl)(self.st, OPUS_GET_LOOKAHEAD_REQUEST, &mut v as *mut i32);
        }
        v.max(0) as u32
    }

    /// Encode one frame of interleaved samples (`frame_size` per channel, e.g. 960).
    pub fn encode(&mut self, pcm: &[f32], frame_size: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; 4000];
        // SAFETY: pcm holds frame_size*channels samples; out has 4000 bytes.
        let n = unsafe {
            (self.lib.enc_float)(self.st, pcm.as_ptr(), frame_size as c_int, out.as_mut_ptr(), out.len() as i32)
        };
        if n < 0 {
            return Err(format!("opus_encode_float failed ({n})"));
        }
        out.truncate(n as usize);
        Ok(out)
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_encoder_create.
        unsafe { (self.lib.enc_destroy)(self.st) }
    }
}

/// Parsed `OpusHead` identification header.
#[derive(Clone, Debug)]
pub struct OpusHead {
    pub channels: usize,
    pub pre_skip: u16,
    pub input_rate: u32,
    pub gain_db: f32,
    pub streams: u8,
    pub coupled: u8,
    pub mapping: Vec<u8>,
}

impl OpusHead {
    pub fn parse(b: &[u8]) -> Option<OpusHead> {
        if b.len() < 19 || &b[0..8] != b"OpusHead" {
            return None;
        }
        let channels = b[9] as usize;
        if channels == 0 {
            return None;
        }
        let pre_skip = u16::from_le_bytes([b[10], b[11]]);
        let input_rate = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
        let gain = i16::from_le_bytes([b[16], b[17]]);
        let family = b[18];
        let (streams, coupled, mapping) = if family == 0 {
            if channels > 2 {
                return None;
            }
            (1u8, (channels == 2) as u8, (0..channels as u8).collect())
        } else {
            if b.len() < 21 + channels {
                return None;
            }
            (b[19], b[20], b[21..21 + channels].to_vec())
        };
        Some(OpusHead {
            channels,
            pre_skip,
            input_rate,
            gain_db: gain as f32 / 256.0,
            streams,
            coupled,
            mapping,
        })
    }

    /// An `OpusHead` for channel mapping family 0.
    pub fn build(channels: u8, pre_skip: u16, input_rate: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(19);
        v.extend_from_slice(b"OpusHead");
        v.push(1);
        v.push(channels);
        v.extend_from_slice(&pre_skip.to_le_bytes());
        v.extend_from_slice(&input_rate.to_le_bytes());
        v.extend_from_slice(&0i16.to_le_bytes());
        v.push(0);
        v
    }
}

const MAX_FRAMES: usize = 5760; // 120 ms at 48 kHz

/// Symphonia decoder backed by libopus (multistream API, so every mapping family works).
pub struct SymOpusDecoder {
    lib: &'static OpusLib,
    st: *mut c_void,
    params: CodecParameters,
    channels: usize,
    gain: f32,
    pcm: Vec<f32>,
    buf: AudioBuffer<f32>,
}

// SAFETY: the decoder state is only touched through &mut self.
unsafe impl Send for SymOpusDecoder {}
unsafe impl Sync for SymOpusDecoder {}

fn channel_mask(n: usize) -> Channels {
    match n {
        1 => Channels::FRONT_LEFT,
        2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
        n => Channels::from_bits_truncate(((1u64 << n.min(31)) - 1) as u32),
    }
}

impl Decoder for SymOpusDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> SymResult<Self> {
        let lib = match lib() {
            Some(l) => l,
            None => return unsupported_error("opus: libopus not found"),
        };
        let head = params
            .extra_data
            .as_deref()
            .and_then(OpusHead::parse)
            .or_else(|| {
                let ch = params.channels.map(|c| c.count()).unwrap_or(2);
                OpusHead::parse(&OpusHead::build(ch.min(2) as u8, 0, 48000))
            });
        let head = match head {
            Some(h) => h,
            None => return decode_error("opus: invalid OpusHead"),
        };
        let mut err: c_int = 0;
        // SAFETY: mapping has `channels` entries.
        let st = unsafe {
            (lib.dec_create)(
                48000,
                head.channels as c_int,
                head.streams as c_int,
                head.coupled as c_int,
                head.mapping.as_ptr(),
                &mut err,
            )
        };
        if st.is_null() || err != 0 {
            return decode_error("opus: decoder creation failed");
        }
        let spec = SignalSpec::new(48000, channel_mask(head.channels));
        let mut p = params.clone();
        p.with_sample_rate(48000).with_channels(spec.channels);
        Ok(SymOpusDecoder {
            lib,
            st,
            params: p,
            channels: head.channels,
            gain: 10f32.powf(head.gain_db / 20.0),
            pcm: vec![0.0; MAX_FRAMES * head.channels],
            buf: AudioBuffer::new(MAX_FRAMES as u64, spec),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[CodecDescriptor {
            codec: CODEC_TYPE_OPUS,
            short_name: "opus",
            long_name: "Opus (libopus)",
            inst_func: |params, opt| Ok(Box::new(SymOpusDecoder::try_new(params, opt)?)),
        }]
    }

    fn reset(&mut self) {}

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> SymResult<AudioBufferRef<'_>> {
        self.buf.clear();
        // SAFETY: pcm has MAX_FRAMES*channels floats.
        let n = unsafe {
            (self.lib.dec_float)(
                self.st,
                packet.data.as_ptr(),
                packet.data.len() as i32,
                self.pcm.as_mut_ptr(),
                MAX_FRAMES as c_int,
                0,
            )
        };
        if n < 0 {
            return decode_error("opus: corrupt packet");
        }
        let n = n as usize;
        self.buf.render_reserved(Some(n));
        let ch = self.channels;
        let g = self.gain;
        {
            let mut planes = self.buf.planes_mut();
            for (c, plane) in planes.planes().iter_mut().enumerate() {
                for (i, s) in plane.iter_mut().enumerate().take(n) {
                    *s = self.pcm[i * ch + c] * g;
                }
            }
        }
        self.buf.trim(packet.trim_start() as usize, packet.trim_end() as usize);
        Ok(self.buf.as_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}

impl Drop for SymOpusDecoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_multistream_decoder_create.
        unsafe { (self.lib.dec_destroy)(self.st) }
    }
}
