//! MP3 encoding through a system libmp3lame (LGPL-2.1) loaded at run time. LAME is never
//! linked into ferraus; when it is missing, MP3 output falls back to the FFmpeg bridge.

use crate::dynlib;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

type Init = unsafe extern "C" fn() -> *mut c_void;
type SetInt = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type SetFloat = unsafe extern "C" fn(*mut c_void, f32) -> c_int;
type Params = unsafe extern "C" fn(*mut c_void) -> c_int;
type EncodeFloat =
    unsafe extern "C" fn(*mut c_void, *const f32, *const f32, c_int, *mut u8, c_int) -> c_int;
type Flush = unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int;
type TagFrame = unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> usize;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
type Version = unsafe extern "C" fn() -> *const c_char;

pub struct LameLib {
    init: Init,
    set_num_channels: SetInt,
    set_in_samplerate: SetInt,
    set_brate: SetInt,
    set_vbr: SetInt,
    set_vbr_quality: SetFloat,
    set_quality: SetInt,
    set_mode: SetInt,
    set_write_vbr_tag: SetInt,
    init_params: Params,
    encode: EncodeFloat,
    flush: Flush,
    lametag: TagFrame,
    close: Close,
    pub version: String,
    pub path: String,
}

static LIB: OnceLock<Option<LameLib>> = OnceLock::new();

/// The loaded libmp3lame, if available (`FERRAUS_LIBMP3LAME` overrides the search;
/// `FERRAUS_LIBMP3LAME_DISABLE=1` disables it).
pub fn lib() -> Option<&'static LameLib> {
    if !cfg!(feature = "mp3-lame") {
        return None;
    }
    LIB.get_or_init(|| {
        let (lib, path) = dynlib::open("FERRAUS_LIBMP3LAME", "mp3lame", "0")?;
        let lib: &'static libloading::Library = Box::leak(Box::new(lib));
        // SAFETY: signatures follow lame.h (3.100).
        unsafe {
            let ver: Version = dynlib::sym(lib, b"get_lame_version\0")?;
            Some(LameLib {
                init: dynlib::sym(lib, b"lame_init\0")?,
                set_num_channels: dynlib::sym(lib, b"lame_set_num_channels\0")?,
                set_in_samplerate: dynlib::sym(lib, b"lame_set_in_samplerate\0")?,
                set_brate: dynlib::sym(lib, b"lame_set_brate\0")?,
                set_vbr: dynlib::sym(lib, b"lame_set_VBR\0")?,
                set_vbr_quality: dynlib::sym(lib, b"lame_set_VBR_quality\0")?,
                set_quality: dynlib::sym(lib, b"lame_set_quality\0")?,
                set_mode: dynlib::sym(lib, b"lame_set_mode\0")?,
                set_write_vbr_tag: dynlib::sym(lib, b"lame_set_bWriteVbrTag\0")?,
                init_params: dynlib::sym(lib, b"lame_init_params\0")?,
                encode: dynlib::sym(lib, b"lame_encode_buffer_ieee_float\0")?,
                flush: dynlib::sym(lib, b"lame_encode_flush\0")?,
                lametag: dynlib::sym(lib, b"lame_get_lametag_frame\0")?,
                close: dynlib::sym(lib, b"lame_close\0")?,
                version: CStr::from_ptr(ver()).to_string_lossy().to_string(),
                path,
            })
        }
    })
    .as_ref()
}

/// LAME VBR quality (0 best .. 9 smallest) roughly matching a target bitrate.
pub fn vbr_quality_for(bitrate: u32) -> f32 {
    let kbps = bitrate as f32 / 1000.0;
    // (kbps, V-level) anchor points from LAME's documented averages
    let table = [(245.0, 0.0), (225.0, 1.0), (190.0, 2.0), (175.0, 3.0), (165.0, 4.0), (130.0, 5.0), (115.0, 6.0), (100.0, 7.0), (85.0, 8.0), (65.0, 9.0)];
    if kbps >= table[0].0 {
        return 0.0;
    }
    for w in table.windows(2) {
        let (hi, qa) = w[0];
        let (lo, qb) = w[1];
        if kbps <= hi && kbps >= lo {
            return qa + (hi - kbps) / (hi - lo) * (qb - qa);
        }
    }
    9.0
}

pub struct LameEncoder {
    lib: &'static LameLib,
    gfp: *mut c_void,
    channels: usize,
    out: Vec<u8>,
}

// SAFETY: the LAME state is only used through &mut self.
unsafe impl Send for LameEncoder {}

impl LameEncoder {
    pub fn new(sample_rate: u32, channels: usize, bitrate: u32, vbr: bool) -> Result<LameEncoder, String> {
        let lib = lib().ok_or("libmp3lame not found")?;
        if channels == 0 || channels > 2 {
            return Err(format!("MP3 supports 1 or 2 channels, got {channels}"));
        }
        // SAFETY: lame_init returns a fresh state or null.
        let gfp = unsafe { (lib.init)() };
        if gfp.is_null() {
            return Err("lame_init failed".into());
        }
        // SAFETY: gfp is valid; setters take plain ints.
        let rc = unsafe {
            (lib.set_num_channels)(gfp, channels as c_int);
            (lib.set_in_samplerate)(gfp, sample_rate as c_int);
            (lib.set_mode)(gfp, if channels == 1 { 3 } else { 1 });
            (lib.set_quality)(gfp, 2);
            (lib.set_write_vbr_tag)(gfp, 1);
            if vbr {
                (lib.set_vbr)(gfp, 4); // vbr_mtrh
                (lib.set_vbr_quality)(gfp, vbr_quality_for(bitrate));
            } else {
                (lib.set_vbr)(gfp, 0);
                (lib.set_brate)(gfp, (bitrate / 1000).clamp(8, 320) as c_int);
            }
            (lib.init_params)(gfp)
        };
        if rc < 0 {
            // SAFETY: close the state we created.
            unsafe { (lib.close)(gfp) };
            return Err(format!("lame_init_params failed ({rc}); unsupported sample rate or bitrate?"));
        }
        Ok(LameEncoder { lib, gfp, channels, out: Vec::new() })
    }

    /// Encode planar samples in [-1, 1]; returns MP3 bytes produced so far.
    pub fn encode(&mut self, left: &[f32], right: Option<&[f32]>) -> Result<&[u8], String> {
        let n = left.len();
        let cap = n * 5 / 4 + 7200;
        self.out.resize(cap, 0);
        let r = if self.channels == 2 { right.unwrap_or(left) } else { left };
        // SAFETY: both channel slices hold n samples; out has cap bytes.
        let k = unsafe {
            (self.lib.encode)(self.gfp, left.as_ptr(), r.as_ptr(), n as c_int, self.out.as_mut_ptr(), cap as c_int)
        };
        if k < 0 {
            return Err(format!("lame_encode_buffer_ieee_float failed ({k})"));
        }
        Ok(&self.out[..k as usize])
    }

    /// Flush the encoder; returns the final bytes.
    pub fn flush(&mut self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; 7200];
        // SAFETY: buf has 7200 bytes.
        let k = unsafe { (self.lib.flush)(self.gfp, buf.as_mut_ptr(), buf.len() as c_int) };
        if k < 0 {
            return Err(format!("lame_encode_flush failed ({k})"));
        }
        buf.truncate(k as usize);
        Ok(buf)
    }

    /// The LAME/Xing tag frame that should replace the first frame of the file.
    pub fn lametag(&mut self) -> Vec<u8> {
        let mut buf = vec![0u8; 2880];
        // SAFETY: buf has 2880 bytes.
        let k = unsafe { (self.lib.lametag)(self.gfp, buf.as_mut_ptr(), buf.len()) };
        if k > buf.len() {
            return Vec::new();
        }
        buf.truncate(k);
        buf
    }
}

impl Drop for LameEncoder {
    fn drop(&mut self) {
        // SAFETY: created by lame_init.
        unsafe { (self.lib.close)(self.gfp) };
    }
}
