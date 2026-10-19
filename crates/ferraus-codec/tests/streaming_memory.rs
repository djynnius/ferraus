//! The streaming convert path keeps memory bounded regardless of file length. A counting
//! global allocator measures peak heap use while a long file converts.
//!
//! The default test converts 10 minutes of 48 kHz stereo (about 115 MB of 16-bit PCM);
//! `three_hour_convert_under_300mb` is the full acceptance check (`cargo test --release
//! -p ferraus-codec --test streaming_memory -- --ignored`).

use ferraus_codec::encode::{native_encoder, Format, StreamSpec};
use ferraus_codec::{convert_streaming, read_path};
use ferraus_core::ctx::EncodeOptions;
use ferraus_core::testing::TempDir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let c = CUR.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(c, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CUR.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Write a long 16-bit stereo WAV in chunks (never holding it in memory).
fn write_long_wav(path: &Path, secs: u64, sr: u32) {
    let opts = EncodeOptions { bit_depth: Some(16), ..Default::default() };
    let file = std::fs::File::create(path).unwrap();
    let total = secs * sr as u64;
    let spec = StreamSpec { sample_rate: sr, channels: 2, total_frames: Some(total) };
    let mut enc = native_encoder(&Format::Wav, Box::new(file), spec, &opts).unwrap();
    let chunk = 48000usize;
    let mut done = 0u64;
    let mut l = vec![0f32; chunk];
    let mut r = vec![0f32; chunk];
    while done < total {
        let n = chunk.min((total - done) as usize);
        for i in 0..n {
            let t = (done + i as u64) as f32 / sr as f32;
            l[i] = ((t * 440.0 * std::f32::consts::TAU).sin() * 16384.0).round() / 32768.0;
            r[i] = ((t * 660.0 * std::f32::consts::TAU).sin() * 16384.0).round() / 32768.0;
        }
        enc.write(&[&l[..n], &r[..n]]).unwrap();
        done += n as u64;
    }
    enc.finish().unwrap();
}

fn run(secs: u64, out_ext: &str, limit_bytes: usize) {
    let td = TempDir::new("stream-mem");
    let src = td.path("long.wav");
    write_long_wav(&src, secs, 48000);
    let src_len = std::fs::metadata(&src).unwrap().len();
    let out = td.path(&format!("long.out.{out_ext}"));
    let opts = EncodeOptions { format: Some(out_ext.into()), bit_depth: Some(16), ..Default::default() };
    let base = CUR.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let stats = convert_streaming(&src, &out, &opts, None, None, &mut |_| Ok(())).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - base;
    eprintln!(
        "{secs}s -> {out_ext}: file {} MB, peak heap {:.1} MB, chunks {}, max chunk {} frames",
        src_len / 1_000_000,
        peak as f64 / 1e6,
        stats.chunks,
        stats.max_chunk_frames
    );
    assert_eq!(stats.frames, secs * 48000);
    assert!(stats.max_chunk_frames <= 65536, "decoder chunks are bounded");
    assert!(stats.chunks > 100);
    assert!(peak < limit_bytes, "peak heap {peak} bytes over {limit_bytes}");
    assert!((peak as u64) < src_len / 4, "memory does not scale with the file");
}

#[test]
fn ten_minute_wav_to_wav_streams_in_bounded_memory() {
    run(600, "wav", 32 << 20);
    // and the output is intact (spot-check a short file the same way)
    let td = TempDir::new("stream-ok");
    let src = td.path("s.wav");
    write_long_wav(&src, 3, 48000);
    let out = td.path("s.flac");
    let opts = EncodeOptions { format: Some("flac".into()), bit_depth: Some(16), ..Default::default() };
    convert_streaming(&src, &out, &opts, None, None, &mut |_| Ok(())).unwrap();
    assert_eq!(read_path(&src, None).unwrap().0, read_path(&out, None).unwrap().0);
}

#[test]
#[ignore = "long: 3 hours of audio; run in release mode"]
fn three_hour_convert_under_300mb() {
    run(3 * 3600, "flac", 300 << 20);
}
