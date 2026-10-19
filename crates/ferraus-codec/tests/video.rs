//! Decoding the audio track of video containers. Fixtures are synthesized with the system
//! ffmpeg; the tests skip when it is absent.

mod common;

use common::*;
use ferraus_codec::{read_path, DecodeStream};
use ferraus_core::testing::{tone_level, TempDir};

fn make(td: &TempDir, name: &str, vcodec: &[&str], acodec: &[&str]) -> Option<std::path::PathBuf> {
    let p = td.path(name);
    let mut args = vec![
        "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1",
        "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000:duration=1",
    ];
    args.extend_from_slice(vcodec);
    args.extend_from_slice(acodec);
    args.extend_from_slice(&["-shortest"]);
    let s = p.display().to_string();
    args.push(&s);
    ffmpeg_gen(&args).then_some(p)
}

fn check(p: &std::path::Path, expect_codec: &str) {
    let ds = DecodeStream::open(p, None).unwrap();
    assert_eq!(ds.info.codec.as_deref(), Some(expect_codec), "{}", p.display());
    let (buf, info) = read_path(p, None).unwrap();
    assert_eq!(info.sample_rate, 48000);
    assert!((buf.duration_s() - 1.0).abs() < 0.1, "duration {}", buf.duration_s());
    let mid = &buf.data[0][4800..43200];
    let lvl = tone_level(mid, 48000, 880.0);
    assert!(lvl > 0.05, "880 Hz present: {lvl}");
}

#[test]
fn mp4_aac_audio_track() {
    let td = TempDir::new("mp4");
    let Some(p) = make(&td, "v.mp4", &["-c:v", "mpeg4"], &["-c:a", "aac", "-b:a", "128k"]) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    check(&p, "aac");
}

#[test]
fn mov_aac_audio_track() {
    let td = TempDir::new("mov");
    let Some(p) = make(&td, "v.mov", &["-c:v", "mpeg4"], &["-c:a", "aac"]) else { return };
    check(&p, "aac");
}

#[test]
fn mov_alac_falls_back_to_ffmpeg() {
    // Symphonia 0.5 does not parse ALAC inside QuickTime 'wave' sample entries; read_path
    // falls back to the FFmpeg bridge for such files.
    let td = TempDir::new("mov-alac");
    let Some(p) = make(&td, "v.mov", &["-c:v", "mpeg4"], &["-c:a", "alac"]) else { return };
    let (buf, info) = read_path(&p, None).unwrap();
    assert_eq!(info.sample_rate, 48000);
    assert!(tone_level(&buf.data[0][4800..43200], 48000, 880.0) > 0.05);
}

#[test]
fn m4a_alac_native() {
    let td = TempDir::new("m4a-alac");
    let p = td.path("a.m4a");
    let s = p.display().to_string();
    if !ffmpeg_gen(&["-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000:duration=1", "-c:a", "alac", &s]) {
        return;
    }
    check(&p, "alac");
}

#[test]
fn mkv_flac_audio_track() {
    let td = TempDir::new("mkv");
    let Some(p) = make(&td, "v.mkv", &["-c:v", "mpeg4"], &["-c:a", "flac"]) else { return };
    let ds = DecodeStream::open(&p, None).unwrap();
    assert!(ds.tracks.len() >= 2, "video and audio streams are both listed: {:?}", ds.tracks);
    check(&p, "flac");
}

#[test]
fn webm_opus_audio_track() {
    if ferraus_codec::opus::lib().is_none() {
        return;
    }
    let td = TempDir::new("webm");
    let Some(p) = make(&td, "v.webm", &["-c:v", "libvpx", "-b:v", "50k"], &["-c:a", "libopus", "-b:a", "96k"]) else { return };
    check(&p, "opus");
}

#[test]
fn second_audio_track_selection() {
    let td = TempDir::new("tracks");
    let p = td.path("two.mkv");
    let s = p.display().to_string();
    let ok = ffmpeg_gen(&[
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
        "-f", "lavfi", "-i", "sine=frequency=1500:sample_rate=48000:duration=0.5",
        "-map", "0:a", "-map", "1:a", "-c:a", "flac", &s,
    ]);
    if !ok {
        return;
    }
    let (a, _) = read_path(&p, None).unwrap();
    let (b, _) = read_path(&p, Some(1)).unwrap();
    assert!(tone_level(&a.data[0][2000..20000], 48000, 440.0) > 0.08);
    assert!(tone_level(&b.data[0][2000..20000], 48000, 1500.0) > 0.08);
    let e = read_path(&p, Some(5)).unwrap_err();
    assert_eq!(e.exit_code, 2);
}
