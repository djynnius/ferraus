//! Media bridge commands against the system FFmpeg (tests skip when it is absent).

use ferraus_codec::CodecIo;
use ferraus_core::ctx::{Globals, NoProgress};
use ferraus_core::exec::{Invocation, Runtime};
use ferraus_core::testing::{tone_level, TempDir};
use ferraus_core::{Envelope, Registry};
use ferraus_media::StreamKind;
use std::path::Path;
use std::process::Command;

static IO: CodecIo = CodecIo;
static NP: NoProgress = NoProgress;

fn invoke(cmd: &str, args: &[(&str, &str)], inputs: &[&Path], g: Globals) -> Envelope {
    let mut r = Registry::new();
    r.extend(ferraus_media::ops());
    let reg: &'static Registry = Box::leak(Box::new(r));
    let presets = |_: &str, _: &str| None;
    let rt = Runtime { registry: reg, io: &IO, progress: &NP, presets: &presets, cancel: Default::default() };
    rt.invoke(Invocation {
        command: cmd.into(),
        args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        inputs: inputs.iter().map(|p| p.display().to_string()).collect(),
        globals: g,
        argv: vec![],
        stdin: None,
    })
    .envelope
}

fn g() -> Globals {
    Globals { sidecar: false, ..Globals::default() }
}

fn out(p: &Path) -> Globals {
    Globals { output: Some(p.display().to_string()), ..g() }
}

fn have_ffmpeg() -> bool {
    ferraus_media::ffmpeg_path().is_some()
}

fn gen(args: &[&str]) -> bool {
    let Some(bin) = ferraus_media::ffmpeg_path() else { return false };
    let mut full = vec!["-hide_banner", "-loglevel", "error", "-y"];
    full.extend_from_slice(args);
    Command::new(bin).args(&full).status().map(|s| s.success()).unwrap_or(false)
}

/// A 2-second MKV with a video stream, two audio streams (440 Hz eng, 1500 Hz fra), and subtitles.
fn movie(td: &TempDir) -> Option<std::path::PathBuf> {
    let srt = td.path("s.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();
    let p = td.path("movie.mkv");
    let (s, p_s) = (srt.display().to_string(), p.display().to_string());
    gen(&[
        "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=2",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2",
        "-f", "lavfi", "-i", "sine=frequency=1500:sample_rate=44100:duration=2",
        "-i", &s,
        "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s",
        "-c:v", "mpeg4", "-c:a:0", "aac", "-c:a:1", "flac", "-c:s", "srt",
        "-metadata:s:a:0", "language=eng", "-metadata:s:a:1", "language=fra",
        &p_s,
    ])
    .then_some(p)
}

#[test]
fn ops_are_documented() {
    for op in ferraus_media::ops() {
        assert!(!op.about.is_empty(), "{}", op.name);
        assert!(op.examples.len() >= 2, "{}", op.name);
        assert_eq!(op.category, ferraus_core::Category::Media);
    }
}

#[test]
fn ffmpeg_passthrough_records_command_line() {
    if !have_ffmpeg() {
        return;
    }
    let env = invoke("ffmpeg", &[("args", "-hide_banner"), ("args", "-version")], &[], g());
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["exit_status"], 0);
    assert!(env.result["text"].as_str().unwrap().contains("ffmpeg version"));
    assert!(env.result["command_line"].as_str().unwrap().ends_with("-hide_banner -version"));
    assert_eq!(env.result["argv"].as_array().unwrap().len(), 3);
    assert!(env.params["command_line"].is_string(), "command line is in params (and so in the sidecar)");
}

#[test]
fn ffmpeg_passthrough_writes_files_and_fails_cleanly() {
    if !have_ffmpeg() {
        return;
    }
    let td = TempDir::new("ff-pass");
    let o = td.path("tone.wav");
    let os = o.display().to_string();
    let args = ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=1000:duration=0.2", &os];
    let pairs: Vec<(&str, &str)> = args.iter().map(|a| ("args", *a)).collect();
    let env = invoke("ffmpeg", &pairs, &[], g());
    assert!(env.ok, "{:?}", env.error);
    assert!(o.exists());
    // a failing run is a processing error carrying ffmpeg's stderr
    let env = invoke("ffmpeg", &[("args", "-hide_banner"), ("args", "-i"), ("args", "/definitely/not/here.wav")], &[], g());
    let e = env.error.unwrap();
    assert_eq!(e.exit_code, 1);
    assert_eq!(e.code, "ffmpeg_failed");
    assert!(e.message.contains("not/here.wav"));
}

#[test]
fn ffprobe_passthrough() {
    if ferraus_codec::ffmpeg_bridge::locate_ffprobe().is_none() {
        return;
    }
    let env = invoke("ffprobe", &[("args", "-version")], &[], g());
    assert!(env.ok, "{:?}", env.error);
    assert!(env.result["text"].as_str().unwrap().contains("ffprobe version"));
}

#[test]
fn media_info_and_streams_list_every_kind() {
    let td = TempDir::new("media-streams");
    let Some(m) = movie(&td) else { return };
    let env = invoke("media info", &[], &[&m], g());
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["source"], "ffprobe");
    assert!(env.result["format"]["format_name"].as_str().unwrap().contains("matroska"));
    let env = invoke("media streams", &[], &[&m], g());
    assert!(env.ok, "{:?}", env.error);
    let r = &env.result;
    assert_eq!(r["counts"]["video"], 1);
    assert_eq!(r["counts"]["audio"], 2);
    assert_eq!(r["counts"]["subtitle"], 1);
    let streams: Vec<ferraus_media::MediaStream> = serde_json::from_value(r["streams"].clone()).unwrap();
    assert_eq!(streams[0].kind, StreamKind::Video);
    let fra = streams.iter().find(|s| s.language.as_deref() == Some("fra")).unwrap();
    assert_eq!((fra.kind, fra.kind_index, fra.codec.as_str()), (StreamKind::Audio, 1, "flac"));
    assert!(r["text"].as_str().unwrap().contains("subtitle:0"));
}

#[test]
fn extract_audio_native_and_copy() {
    let td = TempDir::new("media-extract");
    let Some(m) = movie(&td) else { return };
    // second audio track, decoded natively to WAV
    let wav = td.path("fra.wav");
    let env = invoke("media extract-audio", &[("track", "1")], &[&m], out(&wav));
    assert!(env.ok, "{:?}", env.error);
    let (b, info) = ferraus_codec::read_path(&wav, None).unwrap();
    assert_eq!(info.sample_rate, 44100);
    assert!(tone_level(&b.data[0][4410..60000], 44100, 1500.0) > 0.08);
    assert!((b.duration_s() - 2.0).abs() < 0.05);
    // stream copy of the AAC track keeps the codec; default extension follows it
    let env = invoke("media extract-audio", &[("copy", "true")], &[&m], g());
    assert!(env.ok, "{:?}", env.error);
    let o = env.result["output"].as_str().unwrap();
    assert!(o.ends_with("movie.media-extract-audio.m4a"), "{o}");
    let ds = ferraus_codec::DecodeStream::open(Path::new(o), None).unwrap();
    assert_eq!(ds.info.codec.as_deref(), Some("aac"));
    assert!(env.params["command_line"].as_str().unwrap().contains("-c:a copy"));
    // out-of-range track
    let env = invoke("media extract-audio", &[("track", "7")], &[&m], out(&td.path("x.wav")));
    assert_eq!(env.error.unwrap().exit_code, 2);
}

#[test]
fn replace_audio_and_remux() {
    let td = TempDir::new("media-replace");
    let v = td.path("clip.mp4");
    let vs = v.display().to_string();
    if !gen(&["-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1", "-f", "lavfi", "-i", "sine=frequency=440:duration=1", "-c:v", "mpeg4", "-c:a", "aac", &vs]) {
        return;
    }
    let a = td.path("new.wav");
    ferraus_core::testing::write_wav(&a, &ferraus_core::testing::sine(48000, 1200.0, 1.0, 0.5));
    let o = td.path("clip-new.mp4");
    let env = invoke("media replace-audio", &[], &[&v, &a], out(&o));
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["audio_codec"], "aac");
    let (list, _) = ferraus_media::streams::streams(&o).unwrap();
    assert_eq!(list.iter().filter(|s| s.kind == StreamKind::Video).count(), 1);
    assert_eq!(list.iter().filter(|s| s.kind == StreamKind::Audio).count(), 1);
    let (b, _) = ferraus_codec::read_path(&o, None).unwrap();
    assert!(tone_level(&b.data[0][4800..40000], b.sample_rate, 1200.0) > 0.3);
    // keep the original as a second track
    let o2 = td.path("clip-both.mkv");
    let env = invoke("media replace-audio", &[("keep-original", "true")], &[&v, &a], out(&o2));
    assert!(env.ok, "{:?}", env.error);
    let (list, _) = ferraus_media::streams::streams(&o2).unwrap();
    assert_eq!(list.iter().filter(|s| s.kind == StreamKind::Audio).count(), 2);
    // remux mp4 -> mkv without re-encoding
    let env = invoke("media remux", &[], &[&v], g());
    assert!(env.ok, "{:?}", env.error);
    let r = td.path("clip.media-remux.mkv");
    assert!(r.exists());
    let (list, _) = ferraus_media::streams::streams(&r).unwrap();
    assert_eq!(list.len(), 2);
    assert!(env.result["command_line"].as_str().unwrap().contains("-c copy"));
    // dry run writes nothing
    let env = invoke("media remux", &[], &[&v], Globals { output: Some(td.path("dry.mov").display().to_string()), dry_run: true, ..g() });
    assert!(env.ok);
    assert!(!td.path("dry.mov").exists());
}

/// With FFmpeg hidden (child process) the bridge commands fail with exit 4 and the install hint;
/// `media streams` still works natively for audio files.
#[test]
fn without_ffmpeg_errors_point_to_installer() {
    if std::env::var_os("FERRAUS_TEST_CHILD").is_some() {
        assert!(ferraus_media::ffmpeg_path().is_none());
        assert_eq!(ferraus_media::ffmpeg_status()["found"], false);
        let env = invoke("ffmpeg", &[("args", "-version")], &[], g());
        let e = env.error.unwrap();
        assert_eq!(e.exit_code, 4);
        assert_eq!(e.hint.as_deref(), Some("ferraus doctor --install-ffmpeg"));
        let td = TempDir::new("noff");
        let w = td.path("a.wav");
        ferraus_core::testing::write_wav(&w, &ferraus_core::testing::sine(8000, 440.0, 0.2, 0.5));
        let env = invoke("media remux", &[], &[&w], g());
        assert_eq!(env.error.unwrap().exit_code, 4);
        let env = invoke("media replace-audio", &[], &[&w, &w], g());
        assert_eq!(env.error.unwrap().exit_code, 4);
        let env = invoke("media streams", &[], &[&w], g());
        assert!(env.ok, "{:?}", env.error);
        assert_eq!(env.result["source"], "symphonia");
        assert_eq!(env.result["counts"]["audio"], 1);
        assert_eq!(env.warnings[0].code, "ffprobe_missing");
        let env = invoke("media extract-audio", &[], &[&w], out(&td.path("b.flac")));
        assert!(env.ok, "native extraction needs no ffmpeg: {:?}", env.error);
        return;
    }
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "without_ffmpeg_errors_point_to_installer", "--quiet"])
        .env("FERRAUS_TEST_CHILD", "1")
        .env("FERRAUS_NO_FFMPEG", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn status_reports_license() {
    let s = ferraus_media::ffmpeg_status();
    if s["found"] == true {
        assert!(s["version"].as_str().unwrap().starts_with("ffmpeg version"));
        assert!(s["license"].as_str().unwrap().contains("GPL"));
    } else {
        assert_eq!(s["hint"], "ferraus doctor --install-ffmpeg");
    }
}
