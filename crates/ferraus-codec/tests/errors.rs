mod common;

use common::*;
use ferraus_codec::{read_path, write_bytes, write_file, CodecIo};
use ferraus_core::ctx::{AudioIo, EncodeOptions};
use ferraus_core::testing::TempDir;

#[test]
fn garbage_is_unsupported_exit_4() {
    let td = TempDir::new("garbage");
    let p = td.path("x.wav");
    std::fs::write(&p, b"this is definitely not audio, just some text in a file").unwrap();
    let e = read_path(&p, None).unwrap_err();
    assert_eq!(e.exit_code, 4, "{e:?}");
}

#[test]
fn missing_and_empty_are_exit_3() {
    let td = TempDir::new("missing");
    let e = CodecIo.read(&td.path("nope.wav")).unwrap_err();
    assert_eq!(e.exit_code, 3);
    let p = td.path("empty.flac");
    std::fs::write(&p, b"").unwrap();
    let e = CodecIo.read(&p).unwrap_err();
    assert_eq!(e.exit_code, 3);
    let e = CodecIo.read_bytes(&[], Some("wav")).unwrap_err();
    assert_eq!(e.exit_code, 3);
}

#[test]
fn unknown_output_format_is_exit_4() {
    let td = TempDir::new("badfmt");
    let b = int_signal(8000, 1, 0.01, 16);
    let e = write_file(&td.path("a.xyz"), &b, &EncodeOptions::default()).unwrap_err();
    assert_eq!(e.exit_code, 4);
    let e = write_bytes(&b, &EncodeOptions { format: Some("wav".into()), bit_depth: Some(12), ..Default::default() }).unwrap_err();
    assert_eq!(e.exit_code, 4);
}

#[test]
fn opus_output_rejects_surround() {
    if ferraus_codec::opus::lib().is_none() {
        return;
    }
    let b = int_signal(48000, 6, 0.05, 16);
    let e = write_bytes(&b, &EncodeOptions { format: Some("opus".into()), ..Default::default() }).unwrap_err();
    assert_eq!(e.exit_code, 4);
    assert!(e.hint.unwrap().contains("--channels 2"));
}

fn lcg(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *seed >> 33
}

/// Fuzz-style: truncated and corrupted encodings of every native format never panic.
#[test]
fn truncated_and_corrupt_inputs_never_panic() {
    let b = int_signal(16000, 2, 0.15, 16);
    let mut files: Vec<(&str, Vec<u8>)> = Vec::new();
    for fmt in ["wav", "flac", "aiff", "ogg"] {
        let o = EncodeOptions { format: Some(fmt.into()), bit_depth: Some(16), ..Default::default() };
        files.push((fmt, write_bytes(&b, &o).unwrap().0));
    }
    if ferraus_codec::opus::lib().is_some() {
        let o = EncodeOptions { format: Some("opus".into()), ..Default::default() };
        let b48 = int_signal(48000, 2, 0.1, 16);
        files.push(("opus", write_bytes(&b48, &o).unwrap().0));
    }
    if ferraus_codec::lame::lib().is_some() {
        let o = EncodeOptions { format: Some("mp3".into()), ..Default::default() };
        files.push(("mp3", write_bytes(&b, &o).unwrap().0));
    }
    let mut seed = 42u64;
    for (fmt, bytes) in &files {
        // truncations
        for k in 0..24 {
            let cut = (lcg(&mut seed) as usize) % bytes.len();
            let _ = CodecIo.read_bytes(&bytes[..cut], Some(fmt));
            let _ = CodecIo.read_bytes(&bytes[..cut], None);
            let _ = k;
        }
        // random slices and bit flips
        for _ in 0..24 {
            let a = (lcg(&mut seed) as usize) % bytes.len();
            let e = a + (lcg(&mut seed) as usize) % (bytes.len() - a);
            let _ = CodecIo.read_bytes(&bytes[a..e], Some(fmt));
            let mut m = bytes.clone();
            for _ in 0..8 {
                let i = (lcg(&mut seed) as usize) % m.len();
                m[i] ^= 1 << (lcg(&mut seed) % 8);
            }
            let _ = CodecIo.read_bytes(&m, Some(fmt));
        }
    }
    // pure noise
    for n in [1usize, 4, 12, 64, 1000, 5000] {
        let v: Vec<u8> = (0..n).map(|_| lcg(&mut seed) as u8).collect();
        for h in [None, Some("wav"), Some("mp3"), Some("flac"), Some("ogg"), Some("mp4"), Some("mkv")] {
            let _ = CodecIo.read_bytes(&v, h);
        }
    }
}

#[test]
fn truncated_wav_decodes_what_is_there() {
    let b = int_signal(8000, 1, 0.5, 16);
    let bytes = write_bytes(&b, &EncodeOptions { format: Some("wav".into()), bit_depth: Some(16), ..Default::default() }).unwrap().0;
    let half = &bytes[..bytes.len() / 2];
    let (got, _) = CodecIo.read_bytes(half, Some("wav")).unwrap();
    assert!(got.frames() > 1000 && got.frames() < b.frames());
    assert_eq!(&got.data[0][..500], &b.data[0][..500]);
}
