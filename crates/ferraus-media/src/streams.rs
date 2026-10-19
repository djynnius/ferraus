//! Stream inventory for any container: every video, audio, subtitle, and data stream. v1
//! commands only process audio, but nothing assumes a file holds only audio; this type is
//! the inventory the v2 `ferraus video` namespace builds on.

use ferraus_codec::ffmpeg_bridge;
use ferraus_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Kind of an elementary stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    /// Data, attachments (fonts, cover art as attachment), timecode and unknown streams.
    Data,
}

impl StreamKind {
    pub fn from_ffprobe(codec_type: &str) -> StreamKind {
        match codec_type {
            "video" => StreamKind::Video,
            "audio" => StreamKind::Audio,
            "subtitle" => StreamKind::Subtitle,
            _ => StreamKind::Data,
        }
    }
}

/// One stream in a container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaStream {
    /// Index in the container (ffmpeg's `0:<index>`).
    pub index: usize,
    pub kind: StreamKind,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    /// Index among streams of the same kind (`0:a:<n>` for audio).
    pub kind_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub default: bool,
}

fn num<T: std::str::FromStr>(v: &Value) -> Option<T> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

fn rate(v: &Value) -> Option<f64> {
    let s = v.as_str()?;
    let (a, b) = s.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    (b != 0.0 && a != 0.0).then(|| (a / b * 1000.0).round() / 1000.0)
}

/// Build the inventory from `ffprobe -show_streams -show_format -print_format json` output.
pub fn parse_ffprobe(json: &Value) -> Vec<MediaStream> {
    let fmt_dur: Option<f64> = json.get("format").and_then(|f| f.get("duration")).and_then(num);
    let mut counts = [0usize; 4];
    let mut out = Vec::new();
    for s in json.get("streams").and_then(|v| v.as_array()).into_iter().flatten() {
        let kind = StreamKind::from_ffprobe(s.get("codec_type").and_then(|v| v.as_str()).unwrap_or(""));
        let k = kind as usize;
        let tags = s.get("tags");
        out.push(MediaStream {
            index: s.get("index").and_then(num).unwrap_or(out.len()),
            kind,
            codec: s.get("codec_name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
            language: tags
                .and_then(|t| t.get("language"))
                .and_then(|v| v.as_str())
                .filter(|l| *l != "und")
                .map(String::from),
            duration_s: s.get("duration").and_then(num).or(fmt_dur),
            kind_index: counts[k],
            sample_rate: s.get("sample_rate").and_then(num),
            channels: s.get("channels").and_then(num),
            width: s.get("width").and_then(num),
            height: s.get("height").and_then(num),
            frame_rate: if kind == StreamKind::Video {
                s.get("avg_frame_rate").and_then(rate).or_else(|| s.get("r_frame_rate").and_then(rate))
            } else {
                None
            },
            bitrate: s.get("bit_rate").and_then(num),
            title: tags.and_then(|t| t.get("title")).and_then(|v| v.as_str()).map(String::from),
            default: s.get("disposition").and_then(|d| d.get("default")).and_then(num::<i64>).unwrap_or(0) == 1,
        });
        counts[k] += 1;
    }
    out
}

/// Run ffprobe on a file and return its JSON (`-show_format -show_streams -show_chapters`).
pub fn ffprobe_json(path: &Path) -> Result<(Value, String)> {
    let bin = ffmpeg_bridge::require_ffprobe("media info")?;
    let args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-print_format".into(),
        "json".into(),
        "-show_format".into(),
        "-show_streams".into(),
        "-show_chapters".into(),
        path.display().to_string(),
    ];
    let cmd = crate::bridge::command_line(&bin, &args);
    let out = ffmpeg_bridge::run(&bin, &args)?;
    if !out.status.success() {
        let tail = ffmpeg_bridge::tail(&out.stderr, 4);
        return Err(Error::unsupported(format!("ffprobe cannot read {}: {tail}", path.display())));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::processing(format!("ffprobe returned invalid JSON: {e}")))?;
    Ok((v, cmd))
}

/// Inventory from Symphonia (audio-capable containers only; used when ffprobe is absent).
pub fn native_streams(path: &Path) -> Result<Vec<MediaStream>> {
    let ds = ferraus_codec::DecodeStream::open(path, None)?;
    let mut audio_n = 0;
    let mut data_n = 0;
    Ok(ds
        .tracks
        .iter()
        .map(|t| {
            let audio = t.kind == "audio";
            let kind_index = if audio { audio_n } else { data_n };
            if audio {
                audio_n += 1;
            } else {
                data_n += 1;
            }
            MediaStream {
                index: t.index,
                kind: if audio { StreamKind::Audio } else { StreamKind::Data },
                codec: t.codec.clone(),
                language: t.language.clone().filter(|l| l != "und"),
                duration_s: t.duration_s,
                kind_index,
                sample_rate: t.sample_rate,
                channels: t.channels.map(|c| c as u32),
                width: None,
                height: None,
                frame_rate: None,
                bitrate: None,
                title: None,
                default: t.default,
            }
        })
        .collect())
}

/// Every stream in a file: from ffprobe when available, else from Symphonia. Returns the
/// streams and which backend answered (`ffprobe` or `symphonia`).
pub fn streams(path: &Path) -> Result<(Vec<MediaStream>, &'static str)> {
    if ffmpeg_bridge::locate_ffprobe().is_some() {
        let (v, _) = ffprobe_json(path)?;
        return Ok((parse_ffprobe(&v), "ffprobe"));
    }
    Ok((native_streams(path)?, "symphonia"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_ffprobe_inventory() {
        let v = json!({
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080, "avg_frame_rate": "30000/1001", "disposition": {"default": 1}},
                {"index": 1, "codec_type": "audio", "codec_name": "aac", "sample_rate": "48000", "channels": 2, "bit_rate": "128000", "tags": {"language": "eng"}, "disposition": {"default": 1}},
                {"index": 2, "codec_type": "audio", "codec_name": "ac3", "sample_rate": "48000", "channels": 6, "tags": {"language": "fra", "title": "Director"}},
                {"index": 3, "codec_type": "subtitle", "codec_name": "subrip", "tags": {"language": "und"}},
                {"index": 4, "codec_type": "attachment", "codec_name": "ttf"}
            ],
            "format": {"duration": "12.500000"}
        });
        let s = parse_ffprobe(&v);
        assert_eq!(s.len(), 5);
        assert_eq!(s[0].kind, StreamKind::Video);
        assert_eq!(s[0].frame_rate, Some(29.97));
        assert_eq!((s[0].width, s[0].height), (Some(1920), Some(1080)));
        assert_eq!(s[1].kind, StreamKind::Audio);
        assert_eq!((s[1].kind_index, s[2].kind_index), (0, 1));
        assert_eq!(s[1].language.as_deref(), Some("eng"));
        assert_eq!(s[1].sample_rate, Some(48000));
        assert_eq!(s[1].bitrate, Some(128000));
        assert_eq!(s[2].title.as_deref(), Some("Director"));
        assert_eq!(s[3].kind, StreamKind::Subtitle);
        assert_eq!(s[3].language, None);
        assert_eq!(s[4].kind, StreamKind::Data);
        assert_eq!(s[4].duration_s, Some(12.5));
        assert!(s[0].default && s[1].default && !s[2].default);
        let j = serde_json::to_value(&s[3]).unwrap();
        assert_eq!(j["kind"], "subtitle");
    }
}
