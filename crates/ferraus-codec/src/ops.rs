//! Ops contributed by this crate: `info`, `convert`, `tags`.

use crate::analysis::{Loudness, Spectrum};
use crate::decode::DecodeStream;
use crate::encode::Format;
use crate::{can_encode, can_stream, convert_streaming, tags};
use ferraus_core::ctx::{InputRecord, OutputRecord};
use ferraus_core::exec::conform_output;
use ferraus_core::prelude::*;
use serde_json::Map;
use std::path::{Path, PathBuf};

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("info", Category::Inspect, "Probe a file: container, codec, duration, rate, channels, bit depth, bitrate, tags, art", info)
            .about("Reads the container header (and, for video files, lists every stream) without decoding the audio. \
                    Add --loudness for an EBU R128 measurement (integrated loudness, loudness range, true peak) or \
                    --spectrum for 1/3-octave band averages; both decode the file in a streaming pass.")
            .param(param("loudness", ParamKind::Flag).help("Measure EBU R128 integrated loudness (LUFS), loudness range (LU), and true peak (dBTP) [default: off]"))
            .param(param("spectrum", ParamKind::Flag).help("Average level per 1/3-octave band, 25 Hz to 20 kHz, in dB relative to a full-scale sine [default: off]"))
            .example("ferraus info song.flac", "Container, codec, duration, sample rate, channels, bit depth, bitrate, tags")
            .example("ferraus info mix.wav --loudness --json", "Integrated loudness, LRA, and true peak as JSON")
            .example("ferraus info interview.mp4 --spectrum", "Audio track of a video file plus a 1/3-octave spectrum"),
        OpSpec::custom("convert", Category::Edit, "Decode and re-encode between formats", convert)
            .about("Converts between WAV, AIFF, FLAC, Ogg Vorbis, Opus, MP3, and (through the FFmpeg bridge) M4A/AAC/ALAC. \
                    The output format follows the -o extension or --format. Lossless WAV/FLAC round trips are bit-exact at \
                    the same bit depth. When no resampling is needed the conversion streams packet by packet, so multi-hour \
                    files convert in constant memory. Source tags are copied to the output.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("vbr", ParamKind::Switch).default("on").help("Variable bitrate for Opus, Vorbis, and MP3; off uses constant/average bitrate at --bitrate"))
            .param(
                param("dither", ParamKind::Choice)
                    .choices(&["triangular", "shaped", "none"])
                    .default("triangular")
                    .help("Dither when reducing bit depth: triangular (TPDF), shaped (noise-shaped TPDF), or none; seeded by --seed. Never applied when samples are already exact at the target depth"),
            )
            .example("ferraus convert song.wav -o song.flac", "Lossless WAV to FLAC (bit-exact)")
            .example("ferraus convert take.flac -o take.mp3 --bitrate 256k", "FLAC to MP3 at about 256 kbps VBR")
            .example("ferraus convert master.wav -o master-16.wav --bit-depth 16 --sample-rate 44100", "Downsample and dither a 24-bit master to CD format")
            .example("ferraus convert podcast.wav -o podcast.opus --bitrate 64k --channels 1", "Mono Opus for speech"),
        OpSpec::custom("tags", Category::Inspect, "Read and write metadata tags and cover art", tags_op)
            .about("With no options, prints the file's tags and embedded pictures. --set key=value (repeatable), --remove key, \
                    --art cover.jpg, and --clear modify tags; writing needs --in-place (keeps <file>.bak) or -o for a tagged copy. \
                    Keys: title, artist, album, album_artist, date, year, genre, track, track_total, disc, disc_total, comment, \
                    composer, bpm, copyright, encoder, lyrics, label, publisher, isrc, conductor, lyricist, language, key, mood.")
            .param(param("set", ParamKind::Str).repeatable().help("Set a tag: key=value (repeatable); an empty value removes the key"))
            .param(param("remove", ParamKind::Str).repeatable().help("Remove a tag key (repeatable)"))
            .param(param("art", ParamKind::Path).help("Embed a JPEG/PNG image as the front cover"))
            .param(param("clear", ParamKind::Flag).help("Remove all existing tags first [default: off]"))
            .example("ferraus tags song.mp3", "Show title, artist, album and embedded art")
            .example("ferraus tags song.flac --set title=\"Blue Hour\" --set artist=Nadia --in-place", "Edit tags in place (song.flac.bak is kept)")
            .example("ferraus tags song.m4a --art cover.jpg -o tagged.m4a", "Write a copy with cover art"),
    ]
}

fn fmt_duration(s: f64) -> String {
    let total_ms = (s * 1000.0).round() as u64;
    let h = total_ms / 3_600_000;
    let m = (total_ms / 60_000) % 60;
    let sec = (total_ms % 60_000) as f64 / 1000.0;
    if h > 0 {
        format!("{h}:{m:02}:{sec:06.3}")
    } else {
        format!("{m}:{sec:06.3}")
    }
}

fn layout(ch: usize) -> &'static str {
    match ch {
        1 => "mono",
        2 => "stereo",
        3 => "3.0",
        4 => "quad",
        6 => "5.1",
        8 => "7.1",
        _ => "multichannel",
    }
}

fn open_input(ctx: &mut OpCtx) -> Result<(DecodeStream, PathBuf)> {
    let path = ctx.input(0)?.to_path_buf();
    if path.as_os_str() == "-" {
        let bytes = ctx.stdin.take().ok_or_else(|| Error::input("stdin is empty"))?;
        let ds = DecodeStream::from_bytes(bytes.clone(), ctx.globals.input_format.as_deref(), None);
        ctx.stdin = Some(bytes);
        return Ok((ds?, path));
    }
    ctx.check_allowed(&path)?;
    if !path.exists() {
        return Err(Error::new(ErrorKind::InputMissing, "input_not_found", format!("input not found: {}", path.display())));
    }
    Ok((DecodeStream::open(&path, None)?, path))
}

fn info(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let (mut ds, path) = open_input(ctx)?;
    let want_loud = a.flag("loudness");
    let want_spec = a.flag("spectrum");
    let mut loud = None;
    let mut spec = None;
    let needs_decode = want_loud || want_spec || ds.info.duration_s <= 0.0;
    if needs_decode && !ctx.globals.dry_run {
        let sr = ds.info.sample_rate.max(1);
        if want_loud {
            loud = Some(Loudness::new(ds.info.channels, sr)?);
        }
        if want_spec {
            spec = Some(Spectrum::new(sr));
        }
        let total = ds.n_frames.unwrap_or(0);
        let mut frames = 0u64;
        let mut n = 0u64;
        while let Some(b) = ds.next_chunk()? {
            frames += b.frames() as u64;
            if let Some(l) = loud.as_mut() {
                l.add(&b)?;
            }
            if let Some(s) = spec.as_mut() {
                s.add(&b);
            }
            n += 1;
            if n % 128 == 0 {
                ctx.check_cancel()?;
                if total > 0 {
                    ctx.set_progress(frames as f64 / total as f64);
                }
            }
        }
        ds.info.duration_s = frames as f64 / sr as f64;
    }
    let mut info = ds.info.clone();
    if path.as_os_str() != "-" && info.bitrate.is_none() && info.duration_s > 0.0 {
        if let Ok(m) = std::fs::metadata(&path) {
            info.bitrate = Some((m.len() as f64 * 8.0 / info.duration_s) as u32);
        }
    }
    let mut tags_map = info.tags.clone();
    tags_map.remove("embedded_art");
    let mut art = ds.art.clone();
    if path.as_os_str() != "-" {
        if let Ok(t) = tags::read(&path) {
            for (k, v) in t.tags {
                tags_map.entry(k).or_insert(v);
            }
            if art.is_empty() {
                art = t.art;
            }
        }
    }
    ctx.input_records.push(InputRecord {
        path: info.path.clone(),
        duration_s: Some(info.duration_s),
        sample_rate: Some(info.sample_rate),
        channels: Some(info.channels),
    });
    let mut m = Map::new();
    m.insert("path".into(), json!(info.path));
    m.insert("container".into(), json!(info.container));
    m.insert("codec".into(), json!(info.codec));
    m.insert("duration_s".into(), json!((info.duration_s * 1000.0).round() / 1000.0));
    m.insert("duration".into(), json!(fmt_duration(info.duration_s)));
    m.insert("sample_rate".into(), json!(info.sample_rate));
    m.insert("channels".into(), json!(info.channels));
    m.insert("channel_layout".into(), json!(layout(info.channels)));
    m.insert("bit_depth".into(), json!(info.bit_depth));
    m.insert("bitrate".into(), json!(info.bitrate));
    m.insert("bitrate_kbps".into(), json!(info.bitrate.map(|b| (b as f64 / 100.0).round() / 10.0)));
    if let Some(c) = &info.codec {
        m.insert("lossless".into(), json!(!crate::decode::is_lossy_codec(c)));
    }
    m.insert("tags".into(), json!(tags_map));
    m.insert("art".into(), json!({"present": !art.is_empty(), "pictures": art}));
    if ds.tracks.len() > 1 {
        m.insert("streams".into(), json!(ds.tracks));
    }
    if ds.skipped_packets > 0 {
        ctx.warn("corrupt_packets", format!("{} packets could not be decoded and were skipped", ds.skipped_packets));
    }
    if let Some(l) = &loud {
        m.insert("loudness".into(), l.result());
    }
    if let Some(s) = spec.as_mut() {
        m.insert("spectrum".into(), s.result());
    }
    Ok(Value::Object(m))
}

fn default_ext(ctx: &OpCtx, input: &Path) -> String {
    if let Some(f) = &ctx.globals.format {
        return f.to_ascii_lowercase();
    }
    if let Some(o) = ctx.globals.output.as_deref().filter(|o| *o != "-") {
        if let Some(e) = Path::new(o).extension() {
            return e.to_string_lossy().to_lowercase();
        }
    }
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "wav".into());
    // video containers keep their audio as wav unless asked otherwise
    if can_encode(&ext) && Format::from_name(&ext).is_some() {
        ext
    } else {
        "wav".into()
    }
}

fn peak(buf: &AudioBuffer) -> f32 {
    buf.data.iter().flatten().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn convert(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let input = ctx.input(0)?.to_path_buf();
    let is_stdin = input.as_os_str() == "-";
    let ext = default_ext(ctx, &input);
    let out = ctx.output_path(None, &ext)?;
    let mut opts = ctx.encode_options(&out);
    if opts.format.is_none() {
        opts.format = Some(ext.clone());
    }
    let fmt = Format::from_name(opts.format.as_deref().unwrap_or("wav")).ok_or_else(|| {
        Error::unsupported(format!("cannot encode '{}'", opts.format.clone().unwrap_or_default()))
            .with_hint("use one of: wav, aiff, flac, ogg, opus, mp3, m4a")
    })?;

    // source description (stdin is decoded below)
    let src = if is_stdin {
        None
    } else {
        ctx.check_allowed(&input)?;
        if !input.exists() {
            return Err(Error::new(ErrorKind::InputMissing, "input_not_found", format!("input not found: {}", input.display())));
        }
        Some(DecodeStream::open(&input, None)?.info)
    };
    let src_lossy = src
        .as_ref()
        .and_then(|s| s.codec.as_deref())
        .map(crate::decode::is_lossy_codec)
        .unwrap_or(false);
    opts.bit_depth = ctx
        .globals
        .bit_depth
        .or(if src_lossy { Some(16) } else { None })
        .or(src.as_ref().and_then(|s| s.bit_depth))
        .or(Some(24));
    opts.vbr = a.switch("vbr", true);
    opts.dither = Some(a.choice("dither"));
    if let Some(s) = &src {
        opts.tags = s.tags.iter().filter(|(k, _)| *k != "embedded_art").map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    let streamed = !is_stdin
        && out.as_os_str() != "-"
        && src.as_ref().map(|s| can_stream(&fmt, s.sample_rate, ctx.globals.sample_rate)).unwrap_or(false);
    ctx.note("bit_depth", opts.bit_depth);
    let plan = json!({
        "input": input.display().to_string(),
        "output": out.display().to_string(),
        "format": fmt.name(),
        "bit_depth": if fmt.is_lossless() { json!(opts.bit_depth) } else { Value::Null },
        "bitrate": if fmt.is_lossless() { Value::Null } else { json!(crate::encode::normalize_bitrate(opts.bitrate.unwrap_or(192_000))) },
        "vbr": opts.vbr,
        "dither": opts.dither,
        "streamed": streamed,
    });
    if ctx.globals.dry_run {
        ctx.prepare_output(&out)?;
        ctx.record_output(&out, None);
        return Ok(plan);
    }

    if streamed {
        ctx.prepare_output(&out)?;
        if ctx.globals.in_place && out.exists() {
            std::fs::copy(&out, PathBuf::from(format!("{}.bak", out.display())))?;
        }
        let tmp = ferraus_core::ctx::tmp_path(&out);
        let cancel = ctx.cancel.clone();
        let progress = ctx.progress;
        let command = ctx.command.clone();
        let mut cb = |f: f64| -> Result<()> {
            cancel.check()?;
            progress.progress(&command, f.clamp(0.0, 1.0));
            Ok(())
        };
        let stats = match convert_streaming(&input, &tmp, &opts, ctx.globals.channels, None, &mut cb) {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        std::fs::rename(&tmp, &out)?;
        let s = &stats.source;
        ctx.input_records.push(InputRecord {
            path: input.display().to_string(),
            duration_s: Some(s.duration_s),
            sample_rate: Some(s.sample_rate),
            channels: Some(s.channels),
        });
        let channels = ctx.globals.channels.unwrap_or(s.channels);
        ctx.outputs.push(OutputRecord {
            path: out.display().to_string(),
            role: None,
            duration_s: Some(s.duration_s),
            sample_rate: Some(s.sample_rate),
            channels: Some(channels),
            extra: Map::new(),
        });
        ctx.progress.output(&out.display().to_string(), None);
        if stats.report.clipped > 0 {
            ctx.warn("clipped", format!("{} samples clipped while quantizing to {} bits", stats.report.clipped, opts.bit_depth.unwrap_or(24)));
        }
        if stats.skipped_packets > 0 {
            ctx.warn("corrupt_packets", format!("{} undecodable packets were skipped", stats.skipped_packets));
        }
        let mut r = plan;
        r["codec"] = json!(stats.report.codec);
        r["sample_rate"] = json!(s.sample_rate);
        r["channels"] = json!(channels);
        r["duration_s"] = json!(s.duration_s);
        r["frames"] = json!(stats.frames);
        if let Some(b) = stats.report.bitrate {
            r["bitrate"] = json!(b);
        }
        return Ok(r);
    }

    let buf = ctx.read_audio(0)?;
    let buf = conform_output(buf, &ctx.globals);
    if fmt.is_lossless() && opts.bit_depth.unwrap_or(24) < 32 && peak(&buf) > 1.0 {
        ctx.warn("clipped", "the signal exceeds full scale and will clip at the integer output depth; use --bit-depth 32 or normalize first");
    }
    ctx.write_audio_with(&out, &buf, None, &opts)?;
    let mut r = plan;
    r["sample_rate"] = json!(fmt.target_rate(buf.sample_rate));
    r["channels"] = json!(buf.channels());
    r["duration_s"] = json!(buf.duration_s());
    r["frames"] = json!(buf.frames());
    Ok(r)
}

fn tags_op(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let input = ctx.input(0)?.to_path_buf();
    if input.as_os_str() == "-" {
        return Err(Error::usage("tags needs a file path, not stdin"));
    }
    ctx.check_allowed(&input)?;
    if !input.exists() {
        return Err(Error::new(ErrorKind::InputMissing, "input_not_found", format!("input not found: {}", input.display())));
    }
    ctx.note_input(&input);
    let mut edit = tags::TagEdit {
        set: a.all("set").iter().map(|s| tags::parse_assignment(s)).collect::<Result<_>>()?,
        remove: a.all("remove"),
        clear: a.flag("clear"),
        art: a.path("art"),
    };
    if let Some(art) = &edit.art {
        ctx.check_allowed(art)?;
        if !art.exists() {
            return Err(Error::new(ErrorKind::InputMissing, "input_not_found", format!("cover image not found: {}", art.display())));
        }
    }
    if edit.is_empty() {
        let t = tags::read(&input)?;
        return Ok(json!({"path": input.display().to_string(), "tag_type": t.tag_type, "tags": t.tags, "art": {"present": !t.art.is_empty(), "pictures": t.art}}));
    }
    let target = if ctx.globals.in_place {
        input.clone()
    } else if ctx.globals.output.is_some() {
        ctx.output_path(None, "")?
    } else {
        return Err(Error::usage("writing tags needs --in-place or -o <copy>")
            .with_hint(format!("ferraus tags {} --set title=... --in-place", input.display())));
    };
    if target.as_os_str() == "-" {
        return Err(Error::usage("tags cannot write to stdout; give -o a file path"));
    }
    ctx.prepare_output(&target)?;
    let plan = json!({"path": target.display().to_string(), "set": edit.set, "remove": edit.remove, "clear": edit.clear, "art": edit.art.as_ref().map(|p| p.display().to_string())});
    if ctx.globals.dry_run {
        ctx.record_output(&target, None);
        return Ok(plan);
    }
    // apply to a temporary copy, then move into place so a failure never damages the file
    let tmp = ferraus_core::ctx::tmp_path(&target);
    std::fs::copy(&input, &tmp)?;
    edit.remove.retain(|k| !k.is_empty());
    if let Err(e) = tags::write(&tmp, &edit) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if target == input {
        std::fs::copy(&input, PathBuf::from(format!("{}.bak", input.display())))?;
    }
    std::fs::rename(&tmp, &target)?;
    ctx.record_output(&target, None);
    let t = tags::read(&target)?;
    Ok(json!({"path": target.display().to_string(), "tag_type": t.tag_type, "tags": t.tags, "art": {"present": !t.art.is_empty(), "pictures": t.art}}))
}
