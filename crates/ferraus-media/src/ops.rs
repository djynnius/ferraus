//! Media bridge commands.

use crate::bridge::{self, command_line, run_job, run_streaming};
use crate::streams::{self, MediaStream, StreamKind};
use ferraus_codec::{can_stream, convert_streaming, ffmpeg_bridge, read_path, Format};
use ferraus_core::ctx::{tmp_path, InputRecord, OutputRecord};
use ferraus_core::exec::conform_output;
use ferraus_core::prelude::*;
use serde_json::Map;
use std::path::{Path, PathBuf};

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("ffmpeg", Category::Media, "Run the managed FFmpeg with any arguments (pass-through)", ffmpeg_op)
            .about("Passes every argument after `--` straight to the FFmpeg binary the bridge uses (FERRAUS_FFMPEG / ffmpeg.path, \
                    the managed install from `ferraus doctor --install-ffmpeg`, or ffmpeg on PATH). FFmpeg's log streams to stderr; \
                    text it prints on stdout is returned as the result. The exact command line is recorded in the JSON envelope. \
                    A non-zero exit is an error carrying FFmpeg's last log lines.")
            .param(param("args", ParamKind::List).repeatable().help("Arguments passed to ffmpeg verbatim; put them after `--`"))
            .positional("args")
            .example("ferraus ffmpeg -- -i clip.mov -vf scale=1280:-2 -c:a copy clip-720p.mp4", "Any FFmpeg job, here a video resize")
            .example("ferraus ffmpeg -- -h", "Print FFmpeg's own help")
            .example("ferraus ffmpeg --json -- -i in.mkv -map 0:s:0 subs.srt", "Extract subtitles and get the command line back as JSON"),
        OpSpec::custom("ffprobe", Category::Media, "Run the managed ffprobe with any arguments (pass-through)", ffprobe_op)
            .about("Passes every argument after `--` to the bridge's ffprobe binary and returns what it prints. \
                    For a structured summary use `ferraus media info` or `ferraus media streams`.")
            .param(param("args", ParamKind::List).repeatable().help("Arguments passed to ffprobe verbatim; put them after `--`"))
            .positional("args")
            .example("ferraus ffprobe -- -v error -show_format -of json movie.mp4", "Container details as ffprobe JSON")
            .example("ferraus ffprobe -- -hide_banner -show_streams song.flac", "Stream details in ffprobe's text format"),
        OpSpec::custom("media info", Category::Media, "Container and stream details as JSON (ffprobe -show_format -show_streams)", media_info)
            .about("Runs ffprobe with -show_format -show_streams -show_chapters and returns its JSON, plus a normalized `inventory` \
                    of streams. Without FFmpeg, audio-capable containers are described natively (Symphonia) with a warning.")
            .inputs(Inputs::ONE_ANY)
            .example("ferraus media info movie.mkv --json", "Every stream, chapter, and container field as JSON")
            .example("ferraus media info podcast.m4a", "Format, duration, bitrate, and tags of an audio file"),
        OpSpec::custom("media streams", Category::Media, "List every stream in a container: video, audio, subtitle, data", media_streams)
            .about("Lists each elementary stream with its kind, codec, language, duration, and per-kind index (the N in \
                    `media extract-audio --track N`). Uses ffprobe when available, otherwise Symphonia (audio containers only).")
            .inputs(Inputs::ONE_ANY)
            .example("ferraus media streams movie.mkv", "Video, audio (with languages), subtitle and attachment streams")
            .example("ferraus media streams interview.mp4 --json", "The stream inventory as JSON for an agent"),
        OpSpec::custom("media extract-audio", Category::Media, "Pull an audio track out of a video or multi-track file", extract_audio)
            .about("Decodes the chosen audio track natively (Symphonia) and writes it in any format ferraus encodes (default WAV, \
                    or the -o extension). --copy stream-copies the track without re-encoding through FFmpeg (the default extension \
                    then follows the codec: .m4a for AAC/ALAC, .opus, .mp3, .flac, .ogg, .ac3, otherwise .mka).")
            .inputs(Inputs::ONE_ANY)
            .param(param("track", ParamKind::Int).default("0").range("0..").help("Which audio track (0 = first audio stream; see `media streams`)"))
            .param(param("copy", ParamKind::Flag).help("Stream-copy the track bit-for-bit via FFmpeg instead of decoding [default: off]"))
            .example("ferraus media extract-audio interview.mp4 -o interview.wav", "Audio track to WAV, decoded natively")
            .example("ferraus media extract-audio movie.mkv --track 1 -o commentary.flac", "Second audio track to FLAC")
            .example("ferraus media extract-audio lecture.mp4 --copy", "Copy the AAC track to lecture.media-extract-audio.m4a without re-encoding"),
        OpSpec::custom("media replace-audio", Category::Media, "Write a copy of a video with a new audio track", replace_audio)
            .about("Takes a video and an audio file and writes the video with the audio replaced (video stream copied, never \
                    re-encoded). The audio is copied when the container accepts its codec, otherwise encoded: AAC for MP4/MOV, \
                    Opus for WebM, FLAC for MKV. Needs the FFmpeg bridge.")
            .inputs(Inputs::exactly(2, InputKind::Any))
            .param(
                param("codec", ParamKind::Choice)
                    .choices(&["auto", "copy", "aac", "opus", "mp3", "flac", "pcm"])
                    .default("auto")
                    .help("Audio codec in the output: auto picks copy when compatible, else the container's usual codec"),
            )
            .param(param("keep-original", ParamKind::Flag).help("Keep the original audio as a second track [default: off]"))
            .param(param("shortest", ParamKind::Flag).help("End the output when the shorter of video and audio ends [default: off]"))
            .example("ferraus media replace-audio clip.mp4 mastered.wav -o clip-mastered.mp4", "Swap in the mastered mix (AAC 192k)")
            .example("ferraus media replace-audio talk.mkv clean.flac --keep-original", "New audio first, original kept as track 2"),
        OpSpec::custom("media remux", Category::Media, "Change the container without re-encoding (ffmpeg -c copy)", remux)
            .about("Copies every stream into a new container chosen by the -o extension or --format (default .mkv). Nothing is \
                    re-encoded, so it is fast and lossless; it fails when the target container cannot hold a stream's codec.")
            .inputs(Inputs::ONE_ANY)
            .example("ferraus media remux recording.mkv -o recording.mp4", "MKV to MP4 without re-encoding")
            .example("ferraus media remux broadcast.ts", "Transport stream to broadcast.media-remux.mkv"),
    ]
}

fn passthrough(a: &Args, ctx: &mut OpCtx, probe: bool) -> Result<Value> {
    let name = if probe { "ffprobe" } else { "ffmpeg" };
    let bin = if probe {
        ffmpeg_bridge::require_ffprobe("ferraus ffprobe")?
    } else {
        ffmpeg_bridge::require("ferraus ffmpeg")?
    };
    let args = a.all("args");
    let cmd = command_line(&bin, &args);
    let argv: Vec<String> = std::iter::once(bin.display().to_string()).chain(args.iter().cloned()).collect();
    ctx.note("command_line", &cmd);
    if ctx.globals.dry_run {
        return Ok(json!({"argv": argv, "command_line": cmd}));
    }
    let out = run_streaming(&bin, &args, true)?;
    if !out.success {
        return Err(bridge::failed(name, &cmd, &out));
    }
    let mut r = json!({"argv": argv, "command_line": cmd, "exit_status": out.status});
    if !out.stdout.is_empty() {
        match String::from_utf8(out.stdout) {
            Ok(text) => {
                r["text"] = json!(text);
            }
            Err(e) => {
                // binary output (e.g. `-f wav -`) goes to stdout untouched
                let bytes = e.into_bytes();
                r["stdout_bytes"] = json!(bytes.len());
                ctx.stdout.get_or_insert_with(Vec::new).extend(bytes);
            }
        }
    }
    Ok(r)
}

fn ffmpeg_op(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    passthrough(a, ctx, false)
}

fn ffprobe_op(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    passthrough(a, ctx, true)
}

fn input_file(ctx: &mut OpCtx, i: usize) -> Result<PathBuf> {
    let p = ctx.input(i)?.to_path_buf();
    if p.as_os_str() == "-" {
        return Err(Error::usage(format!("'{}' needs a file path, not stdin", ctx.command)));
    }
    ctx.check_allowed(&p)?;
    if !p.exists() {
        return Err(Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("input not found: {}", p.display()),
        ));
    }
    Ok(p)
}

fn media_info(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = input_file(ctx, 0)?;
    ctx.note_input(&p);
    if ffmpeg_bridge::locate_ffprobe().is_some() {
        let (mut v, cmd) = streams::ffprobe_json(&p)?;
        ctx.note("command_line", &cmd);
        let inv = streams::parse_ffprobe(&v);
        v["inventory"] = json!(inv);
        v["source"] = json!("ffprobe");
        return Ok(v);
    }
    ctx.warn(
        "ffprobe_missing",
        format!("ffprobe not found; showing the native description (install with: {})", ffmpeg_bridge::INSTALL_HINT),
    );
    let ds = ferraus_codec::DecodeStream::open(&p, None)?;
    let inv = streams::native_streams(&p)?;
    Ok(json!({
        "format": {
            "filename": p.display().to_string(),
            "format_name": ds.info.container,
            "duration": ds.info.duration_s,
            "bit_rate": ds.info.bitrate,
            "tags": ds.info.tags,
        },
        "inventory": inv,
        "source": "symphonia",
    }))
}

fn media_streams(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = input_file(ctx, 0)?;
    ctx.note_input(&p);
    let (list, source) = streams::streams(&p)?;
    if source == "symphonia" {
        ctx.warn(
            "ffprobe_missing",
            format!(
                "ffprobe not found; only streams Symphonia understands are listed (install with: {})",
                ffmpeg_bridge::INSTALL_HINT
            ),
        );
    }
    let count = |k: StreamKind| list.iter().filter(|s| s.kind == k).count();
    let text = list.iter().map(describe).collect::<Vec<_>>().join("\n");
    Ok(json!({
        "path": p.display().to_string(),
        "source": source,
        "counts": {"video": count(StreamKind::Video), "audio": count(StreamKind::Audio), "subtitle": count(StreamKind::Subtitle), "data": count(StreamKind::Data)},
        "streams": list,
        "text": text,
    }))
}

fn describe(s: &MediaStream) -> String {
    let kind = match s.kind {
        StreamKind::Video => "video",
        StreamKind::Audio => "audio",
        StreamKind::Subtitle => "subtitle",
        StreamKind::Data => "data",
    };
    let mut d = format!("#{} {kind}:{} {}", s.index, s.kind_index, s.codec);
    if let (Some(w), Some(h)) = (s.width, s.height) {
        d.push_str(&format!(" {w}x{h}"));
    }
    if let Some(f) = s.frame_rate {
        d.push_str(&format!(" {f}fps"));
    }
    if let Some(sr) = s.sample_rate {
        d.push_str(&format!(" {sr}Hz"));
    }
    if let Some(c) = s.channels {
        d.push_str(&format!(" {c}ch"));
    }
    if let Some(l) = &s.language {
        d.push_str(&format!(" [{l}]"));
    }
    if let Some(t) = &s.title {
        d.push_str(&format!(" \"{t}\""));
    }
    if s.default {
        d.push_str(" (default)");
    }
    d
}

fn copy_ext(codec: &str) -> &'static str {
    match codec {
        "aac" | "alac" => "m4a",
        "mp3" => "mp3",
        "opus" => "opus",
        "vorbis" => "ogg",
        "flac" => "flac",
        "ac3" => "ac3",
        "eac3" => "eac3",
        c if c.starts_with("pcm") => "wav",
        _ => "mka",
    }
}

fn audio_codec_of(p: &Path, track: usize) -> Option<String> {
    if let Ok((list, _)) = streams::streams(p) {
        return list.into_iter().find(|s| s.kind == StreamKind::Audio && s.kind_index == track).map(|s| s.codec);
    }
    None
}

fn record(ctx: &mut OpCtx, out: &Path) {
    ctx.outputs.push(OutputRecord {
        path: out.display().to_string(),
        role: None,
        duration_s: None,
        sample_rate: None,
        channels: None,
        extra: Map::new(),
    });
    ctx.progress.output(&out.display().to_string(), None);
}

/// Run an ffmpeg job writing `out` through a temporary file (renamed on success).
fn ffmpeg_to(ctx: &mut OpCtx, what: &str, mut args: Vec<String>, out: &Path) -> Result<String> {
    let tmp = tmp_path(out);
    args.push("-y".into());
    args.push(tmp.display().to_string());
    ctx.temp_files.push(tmp.clone());
    let res = run_job(what, args);
    let (cmd, _) = match res {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if ctx.globals.in_place && out.exists() {
        std::fs::copy(out, PathBuf::from(format!("{}.bak", out.display())))?;
    }
    std::fs::rename(&tmp, out)?;
    // record the command with the final path for provenance
    let shown =
        cmd.replace(&bridge::shell_quote(&tmp.display().to_string()), &bridge::shell_quote(&out.display().to_string()));
    ctx.note("command_line", &shown);
    record(ctx, out);
    Ok(shown)
}

fn out_ext_or(ctx: &OpCtx, default: &str) -> String {
    if let Some(f) = &ctx.globals.format {
        return f.to_ascii_lowercase();
    }
    if let Some(o) = ctx.globals.output.as_deref().filter(|o| *o != "-") {
        if let Some(e) = Path::new(o).extension() {
            return e.to_string_lossy().to_lowercase();
        }
    }
    default.to_string()
}

fn extract_audio(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = input_file(ctx, 0)?;
    let track = a.usize("track")?;
    if a.flag("copy") {
        let codec = audio_codec_of(&p, track).ok_or_else(|| {
            Error::usage(format!("{} has no audio track {track}", p.display()))
                .with_hint(format!("ferraus media streams {}", p.display()))
        })?;
        let ext = out_ext_or(ctx, copy_ext(&codec));
        let out = ctx.output_path(None, &ext)?;
        ctx.prepare_output(&out)?;
        ctx.note_input(&p);
        let args: Vec<String> = vec![
            "-i".into(),
            p.display().to_string(),
            "-map".into(),
            format!("0:a:{track}"),
            "-vn".into(),
            "-sn".into(),
            "-dn".into(),
            "-c:a".into(),
            "copy".into(),
        ];
        if ctx.globals.dry_run {
            record(ctx, &out);
            return Ok(json!({"output": out.display().to_string(), "codec": codec, "copied": true}));
        }
        let cmd = ffmpeg_to(ctx, "media extract-audio --copy", args, &out)?;
        return Ok(json!({"output": out.display().to_string(), "codec": codec, "copied": true, "command_line": cmd}));
    }
    let ext = out_ext_or(ctx, "wav");
    let fmt = Format::from_name(&ext).ok_or_else(|| {
        Error::unsupported(format!("cannot encode '.{ext}'"))
            .with_hint("use .wav, .flac, .ogg, .opus, .mp3, .aiff, or --copy")
    })?;
    let out = ctx.output_path(None, &ext)?;
    ctx.prepare_output(&out)?;
    let mut opts = ctx.encode_options(&out);
    opts.format = Some(ext.clone());
    if ctx.globals.dry_run {
        record(ctx, &out);
        return Ok(json!({"output": out.display().to_string(), "track": track, "format": fmt.name()}));
    }
    // stream when possible (long videos), else decode fully
    let probe = ferraus_codec::DecodeStream::open(&p, Some(track));
    let streamable =
        probe.as_ref().map(|ds| can_stream(&fmt, ds.info.sample_rate, ctx.globals.sample_rate)).unwrap_or(false);
    let codec = probe.as_ref().ok().and_then(|d| d.info.codec.clone());
    if let Ok(ds) = &probe {
        if opts.bit_depth.is_none() {
            opts.bit_depth = ds.info.bit_depth.or(Some(
                if codec.as_deref().map(ferraus_codec::decode::is_lossy_codec).unwrap_or(false) { 16 } else { 24 },
            ));
        }
    }
    drop(probe);
    if streamable {
        let tmp = tmp_path(&out);
        ctx.temp_files.push(tmp.clone());
        let cancel = ctx.cancel.clone();
        let progress = ctx.progress;
        let command = ctx.command.clone();
        let mut cb = |f: f64| -> Result<()> {
            cancel.check()?;
            progress.progress(&command, f);
            Ok(())
        };
        let stats =
            convert_streaming(&p, &tmp, &opts, ctx.globals.channels, Some(track), &mut cb).inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })?;
        std::fs::rename(&tmp, &out)?;
        let s = &stats.source;
        ctx.input_records.push(InputRecord {
            path: p.display().to_string(),
            duration_s: Some(s.duration_s),
            sample_rate: Some(s.sample_rate),
            channels: Some(s.channels),
        });
        ctx.outputs.push(OutputRecord {
            path: out.display().to_string(),
            role: None,
            duration_s: Some(s.duration_s),
            sample_rate: Some(s.sample_rate),
            channels: Some(ctx.globals.channels.unwrap_or(s.channels)),
            extra: Map::new(),
        });
        ctx.progress.output(&out.display().to_string(), None);
        return Ok(
            json!({"output": out.display().to_string(), "track": track, "format": fmt.name(), "source_codec": codec, "duration_s": s.duration_s, "streamed": true}),
        );
    }
    let (buf, info) = read_path(&p, Some(track))?;
    ctx.input_records.push(InputRecord {
        path: p.display().to_string(),
        duration_s: Some(info.duration_s),
        sample_rate: Some(info.sample_rate),
        channels: Some(info.channels),
    });
    let buf = conform_output(buf, &ctx.globals);
    ctx.write_audio_with(&out, &buf, None, &opts)?;
    Ok(
        json!({"output": out.display().to_string(), "track": track, "format": fmt.name(), "source_codec": info.codec, "duration_s": buf.duration_s(), "streamed": false}),
    )
}

fn replace_audio(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let video = input_file(ctx, 0)?;
    let audio = input_file(ctx, 1)?;
    ffmpeg_bridge::require("media replace-audio")?;
    let out = ctx.output_path(None, "")?;
    ctx.prepare_output(&out)?;
    ctx.note_input(&video);
    ctx.note_input(&audio);
    let container = out.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mkv".into());
    let src_codec = audio_codec_of(&audio, 0).unwrap_or_default();
    let choice = a.choice("codec");
    let codec = if choice == "auto" {
        match container.as_str() {
            "mp4" | "m4v" | "mov" => {
                if matches!(src_codec.as_str(), "aac" | "alac" | "mp3") {
                    "copy"
                } else {
                    "aac"
                }
            }
            "webm" => {
                if matches!(src_codec.as_str(), "opus" | "vorbis") {
                    "copy"
                } else {
                    "opus"
                }
            }
            _ => {
                if src_codec.is_empty() || src_codec.starts_with("pcm") {
                    "flac"
                } else {
                    "copy"
                }
            }
        }
        .to_string()
    } else {
        choice
    };
    let bitrate = ferraus_codec::encode::normalize_bitrate(ctx.globals.bitrate.unwrap_or(192_000));
    let mut args: Vec<String> = vec![
        "-i".into(),
        video.display().to_string(),
        "-i".into(),
        audio.display().to_string(),
        "-map".into(),
        "0:v?".into(),
        "-map".into(),
        "1:a:0".into(),
    ];
    if a.flag("keep-original") {
        args.extend(["-map".into(), "0:a?".into()]);
    }
    args.extend(["-c:v".into(), "copy".into()]);
    match codec.as_str() {
        "copy" => args.extend(["-c:a:0".into(), "copy".into()]),
        "aac" => args.extend(["-c:a:0".into(), "aac".into(), "-b:a:0".into(), bitrate.to_string()]),
        "opus" => args.extend(["-c:a:0".into(), "libopus".into(), "-b:a:0".into(), bitrate.to_string()]),
        "mp3" => args.extend(["-c:a:0".into(), "libmp3lame".into(), "-b:a:0".into(), bitrate.to_string()]),
        "flac" => args.extend(["-c:a:0".into(), "flac".into()]),
        "pcm" => args.extend(["-c:a:0".into(), "pcm_s24le".into()]),
        _ => {}
    }
    if a.flag("keep-original") {
        args.extend(["-c:a:1".into(), "copy".into()]);
    }
    if a.flag("shortest") {
        args.push("-shortest".into());
    }
    if matches!(container.as_str(), "mp4" | "m4v" | "mov") {
        args.extend(["-movflags".into(), "+faststart".into()]);
    }
    if ctx.globals.dry_run {
        record(ctx, &out);
        return Ok(json!({"output": out.display().to_string(), "audio_codec": codec, "args": args}));
    }
    let cmd = ffmpeg_to(ctx, "media replace-audio", args, &out)?;
    Ok(
        json!({"output": out.display().to_string(), "audio_codec": codec, "source_audio_codec": src_codec, "command_line": cmd}),
    )
}

fn remux(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = input_file(ctx, 0)?;
    ffmpeg_bridge::require("media remux")?;
    let ext = out_ext_or(ctx, "mkv");
    let out = ctx.output_path(None, &ext)?;
    ctx.prepare_output(&out)?;
    ctx.note_input(&p);
    let mut args: Vec<String> =
        vec!["-i".into(), p.display().to_string(), "-map".into(), "0".into(), "-c".into(), "copy".into()];
    if matches!(ext.as_str(), "mp4" | "m4v" | "mov" | "m4a") {
        args.extend(["-movflags".into(), "+faststart".into()]);
    }
    if let Some(f) = &ctx.globals.format {
        let muxer = match f.as_str() {
            "mkv" => "matroska",
            "m4a" | "m4v" => "mp4",
            "ts" => "mpegts",
            other => other,
        };
        args.extend(["-f".into(), muxer.to_string()]);
    }
    if ctx.globals.dry_run {
        record(ctx, &out);
        return Ok(json!({"output": out.display().to_string(), "args": args}));
    }
    let cmd = ffmpeg_to(ctx, "media remux", args, &out).map_err(|e| {
        if e.code == "ffmpeg_failed" {
            e.with_hint("the target container may not support a stream's codec; transcode with `ferraus ffmpeg -- ...`")
        } else {
            e
        }
    })?;
    Ok(json!({"output": out.display().to_string(), "container": ext, "command_line": cmd}))
}
