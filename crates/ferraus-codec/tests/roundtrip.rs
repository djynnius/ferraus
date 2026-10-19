mod common;

use common::*;
use ferraus_codec::{read_path, write_bytes, write_file, CodecIo};
use ferraus_core::ctx::{AudioIo, EncodeOptions};
use ferraus_core::testing::{sine, tone_level, TempDir};

fn opts(fmt: &str, bits: Option<u16>) -> EncodeOptions {
    EncodeOptions { format: Some(fmt.into()), bit_depth: bits, vbr: true, ..Default::default() }
}

#[test]
fn wav_pcm_roundtrip_bit_exact() {
    let td = TempDir::new("wav-rt");
    for bits in [8u16, 16, 24] {
        let src = int_signal(44100, 2, 0.5, bits);
        let p = td.path(&format!("a{bits}.wav"));
        write_file(&p, &src, &opts("wav", Some(bits))).unwrap();
        let (back, info) = read_path(&p, None).unwrap();
        assert_eq!(info.bit_depth, Some(bits));
        assert_eq!(back, src, "{bits}-bit decode is exact");
        // re-encode: identical sample bytes
        let p2 = td.path(&format!("b{bits}.wav"));
        write_file(&p2, &back, &opts("wav", Some(bits))).unwrap();
        assert_eq!(wav_data(&std::fs::read(&p).unwrap()), wav_data(&std::fs::read(&p2).unwrap()));
    }
}

#[test]
fn wav_float_roundtrip_bit_exact() {
    let td = TempDir::new("wav-f");
    let src = ferraus_core::testing::noise(48000, 0.3, 0.9, 3);
    let p = td.path("f.wav");
    write_file(&p, &src, &EncodeOptions { float: true, ..opts("wav", Some(32)) }).unwrap();
    let (back, info) = read_path(&p, None).unwrap();
    assert_eq!(info.bit_depth, Some(32));
    assert_eq!(back, src);
}

#[test]
fn flac_roundtrip_bit_exact() {
    let td = TempDir::new("flac-rt");
    for bits in [16u16, 24] {
        let src = int_signal(48000, 2, 1.3, bits); // > 1 s: several frames plus a short last one
        let p = td.path(&format!("a{bits}.flac"));
        let rep = write_file(&p, &src, &opts("flac", Some(bits))).unwrap();
        assert_eq!(rep.bit_depth, Some(bits));
        let (back, info) = read_path(&p, None).unwrap();
        assert_eq!(info.codec.as_deref(), Some("flac"));
        assert_eq!(info.bit_depth, Some(bits));
        assert_eq!(back, src, "{bits}-bit FLAC is lossless");
        // never meaningfully larger than raw PCM, even for noisy high-resolution input
        let raw = (src.frames() * 2 * bits as usize / 8) as u64;
        assert!(std::fs::metadata(&p).unwrap().len() < raw + raw / 50, "{bits}-bit size");
    }
    // mono, tiny (single short frame)
    let src = int_signal(22050, 1, 0.01, 16);
    let p = td.path("tiny.flac");
    write_file(&p, &src, &opts("flac", Some(16))).unwrap();
    assert_eq!(read_path(&p, None).unwrap().0, src);
}

#[test]
fn flac_md5_matches_reference_decoder() {
    // the system ffmpeg verifies FLAC MD5 when decoding with -flags +bitexact; skip if absent
    let Some(_) = ffmpeg() else { return };
    let td = TempDir::new("flac-md5");
    let src = int_signal(44100, 2, 0.7, 16);
    let p = td.path("a.flac");
    write_file(&p, &src, &opts("flac", Some(16))).unwrap();
    let back = ferraus_codec::ffmpeg_bridge::decode_file(&p, None).unwrap();
    assert_eq!(back.frames(), src.frames());
    assert!(max_abs_diff(&back, &src) < 1e-7);
}

#[test]
fn aiff_roundtrip_bit_exact() {
    let td = TempDir::new("aiff");
    for bits in [16u16, 24] {
        let src = int_signal(44100, 2, 0.25, bits);
        let p = td.path(&format!("a{bits}.aiff"));
        write_file(&p, &src, &opts("aiff", Some(bits))).unwrap();
        let (back, info) = read_path(&p, None).unwrap();
        assert_eq!(info.container.as_deref(), Some("aiff"));
        assert_eq!(back, src);
    }
}

#[test]
fn default_bit_depth_is_24() {
    let td = TempDir::new("def24");
    let p = td.path("d.wav");
    CodecIo.write(&p, &sine(8000, 440.0, 0.1, 0.5), &opts("wav", None)).unwrap();
    assert_eq!(CodecIo.probe(&p).unwrap().bit_depth, Some(24));
}

#[test]
fn dither_is_deterministic_with_seed() {
    let src = sine(44100, 997.0, 0.2, 0.3);
    let enc = |seed: u64, dither: &str| {
        let o = EncodeOptions { seed, dither: Some(dither.into()), ..opts("wav", Some(16)) };
        write_bytes(&src, &o).unwrap().0
    };
    assert_eq!(enc(1, "triangular"), enc(1, "triangular"));
    assert_ne!(enc(1, "triangular"), enc(2, "triangular"));
    assert_eq!(enc(1, "none"), enc(2, "none"));
    assert_eq!(enc(5, "shaped"), enc(5, "shaped"));
}

#[test]
fn stdin_bytes_and_stdout_bytes() {
    let src = int_signal(16000, 1, 0.2, 16);
    for fmt in ["wav", "flac"] {
        let bytes = CodecIo.write_bytes(&src, &opts(fmt, Some(16))).unwrap();
        let (back, _) = CodecIo.read_bytes(&bytes, Some(fmt)).unwrap();
        assert_eq!(back, src, "{fmt}");
        // without a hint the format is sniffed
        let (back, _) = CodecIo.read_bytes(&bytes, None).unwrap();
        assert_eq!(back, src);
    }
}

#[test]
fn can_encode_is_truthful() {
    for e in ["wav", "flac", "aiff"] {
        assert!(CodecIo.can_encode(e));
    }
    assert!(!CodecIo.can_encode("xyz"));
    assert_eq!(CodecIo.can_encode("m4a"), ffmpeg().is_some());
}

fn lossy_check(fmt: &str, min_snr: f64) {
    let td = TempDir::new(&format!("lossy-{fmt}"));
    let src = ferraus_core::testing::stereo(sine(48000, 1000.0, 1.0, 0.5));
    let p = td.path(&format!("a.{fmt}"));
    write_file(&p, &src, &EncodeOptions { bitrate: Some(160_000), ..opts(fmt, None) }).unwrap();
    let (back, info) = read_path(&p, None).unwrap();
    assert_eq!(info.channels, 2);
    assert_eq!(back.sample_rate, 48000);
    let len_err = (back.frames() as i64 - src.frames() as i64).abs();
    assert!(len_err < 3000, "{fmt}: length {} vs {}", back.frames(), src.frames());
    let lvl = tone_level(&back.data[0][4800..43200], 48000, 1000.0);
    assert!((lvl - 0.5).abs() < 0.05, "{fmt}: tone level {lvl}");
    let (_, snr) = aligned_snr(&src.data[0][4800..40000], &back.data[0][4800..], 2000);
    assert!(snr > min_snr, "{fmt}: snr {snr}");
}

#[test]
fn vorbis_decode_after_encode() {
    lossy_check("ogg", 20.0);
}

#[test]
fn opus_decode_after_encode() {
    if ferraus_codec::opus::lib().is_none() && ffmpeg().is_none() {
        eprintln!("skipping: no libopus and no ffmpeg");
        return;
    }
    if ferraus_codec::opus::lib().is_none() {
        return; // decoding .opus natively needs libopus
    }
    lossy_check("opus", 15.0);
}

#[test]
fn mp3_decode_after_encode() {
    if ferraus_codec::lame::lib().is_none() && ffmpeg().is_none() {
        // without LAME or FFmpeg, MP3 output must be a clear exit-4 error naming the bridge
        let td = TempDir::new("mp3-missing");
        let e = write_file(&td.path("a.mp3"), &sine(44100, 440.0, 0.1, 0.5), &opts("mp3", None)).unwrap_err();
        assert_eq!(e.exit_code, 4);
        assert!(e.hint.unwrap().contains("--install-ffmpeg"));
        return;
    }
    lossy_check("mp3", 15.0);
}

/// With LAME, libopus, and FFmpeg all hidden (child process), MP3/Opus/M4A output are
/// exit-4 errors whose hint names the FFmpeg bridge installer.
#[test]
fn missing_encoders_are_exit_4_with_bridge_hint() {
    if std::env::var_os("FERRAUS_TEST_CHILD").is_some() {
        let td = TempDir::new("no-encoders");
        assert!(!CodecIo.can_encode("mp3") && !CodecIo.can_encode("m4a") && !CodecIo.can_encode("opus"));
        for ext in ["mp3", "opus", "m4a"] {
            let e = write_file(&td.path(&format!("a.{ext}")), &sine(48000, 440.0, 0.1, 0.5), &opts(ext, None)).unwrap_err();
            assert_eq!(e.exit_code, 4, "{ext}");
            assert!(e.hint.as_deref().unwrap_or("").contains("ferraus doctor --install-ffmpeg"), "{ext}: {e:?}");
        }
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "missing_encoders_are_exit_4_with_bridge_hint", "--quiet"])
        .env("FERRAUS_TEST_CHILD", "1")
        .env("FERRAUS_LIBMP3LAME_DISABLE", "1")
        .env("FERRAUS_LIBOPUS_DISABLE", "1")
        .env("FERRAUS_NO_FFMPEG", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn m4a_via_bridge() {
    if ffmpeg().is_none() {
        let td = TempDir::new("m4a-missing");
        let e = write_file(&td.path("a.m4a"), &sine(44100, 440.0, 0.1, 0.5), &opts("m4a", None)).unwrap_err();
        assert_eq!(e.exit_code, 4);
        return;
    }
    lossy_check("m4a", 15.0);
}

#[test]
fn quantizing_counts_clips() {
    let mut b = sine(8000, 100.0, 0.1, 1.5);
    b.data[0][0] = 0.0;
    let (_, rep) = write_bytes(&b, &opts("wav", Some(16))).unwrap();
    assert!(rep.clipped > 0);
}
