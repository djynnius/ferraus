//! Format detection, loading, and saving by file extension.

use crate::model::Score;
use crate::notate::{self, NotateOptions};
use crate::perform::{self, PlayOptions};
use crate::solfa::{self, SolfaKind};
use crate::{abc, lily, mei, midi, musicxml};
use ferraus_core::{Error, Result};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    MusicXml,
    Mxl,
    Mei,
    Abc,
    Lily,
    Midi,
    Json,
    SolfaText,
    SolfaMarkdown,
    Svg,
    Pdf,
    Png,
    Image,
    Audio,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::MusicXml => "musicxml",
            Format::Mxl => "mxl",
            Format::Mei => "mei",
            Format::Abc => "abc",
            Format::Lily => "lilypond",
            Format::Midi => "midi",
            Format::Json => "json",
            Format::SolfaText => "solfa-text",
            Format::SolfaMarkdown => "solfa-markdown",
            Format::Svg => "svg",
            Format::Pdf => "pdf",
            Format::Png => "png",
            Format::Image => "image",
            Format::Audio => "audio",
        }
    }
    /// Notation formats this crate can read.
    pub fn readable(self) -> bool {
        matches!(
            self,
            Format::MusicXml | Format::Mxl | Format::Mei | Format::Abc | Format::Lily | Format::Midi | Format::Json
        )
    }
}

pub fn ext_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Format from a path's extension.
pub fn format_of(path: &Path) -> Option<Format> {
    Some(match ext_of(path).as_str() {
        "musicxml" | "xml" => Format::MusicXml,
        "mxl" => Format::Mxl,
        "mei" => Format::Mei,
        "abc" => Format::Abc,
        "ly" => Format::Lily,
        "mid" | "midi" | "smf" | "kar" => Format::Midi,
        "json" => Format::Json,
        "txt" | "solfa" => Format::SolfaText,
        "md" | "markdown" => Format::SolfaMarkdown,
        "svg" => Format::Svg,
        "pdf" => Format::Pdf,
        "png" => Format::Png,
        "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp" => Format::Image,
        "wav" | "flac" | "mp3" | "ogg" | "opus" | "m4a" | "aac" | "aiff" | "aif" | "wv" | "caf" | "mp4" | "mov"
        | "mkv" | "webm" => Format::Audio,
        _ => return None,
    })
}

/// Sniff content when the extension is missing or ambiguous.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"MThd") {
        return Some(Format::Midi);
    }
    if bytes.starts_with(b"%PDF") {
        return Some(Format::Pdf);
    }
    if bytes.starts_with(b"PK") {
        return Some(Format::Mxl);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
    if head.contains("<score-partwise") || head.contains("<score-timewise") {
        return Some(Format::MusicXml);
    }
    if head.contains("<mei") {
        return Some(Format::Mei);
    }
    if head.trim_start().starts_with('{') && head.contains("\"parts\"") {
        return Some(Format::Json);
    }
    if head.lines().any(|l| l.starts_with("X:")) && head.lines().any(|l| l.starts_with("K:")) {
        return Some(Format::Abc);
    }
    if head.contains("\\version") || head.contains("\\score") || head.contains("\\new Staff") {
        return Some(Format::Lily);
    }
    None
}

/// A loaded score plus constructs the reader skipped.
pub struct Loaded {
    pub score: Score,
    pub unsupported: Vec<String>,
    pub format: Format,
}

/// Load a notation file (MIDI is notated with default options).
pub fn load(path: &Path, bytes: &[u8]) -> Result<Loaded> {
    let fmt = match format_of(path) {
        Some(f) if f.readable() => f,
        Some(Format::MusicXml) | None => sniff(bytes)
            .ok_or_else(|| Error::unsupported(format!("cannot tell the notation format of {}", path.display())))?,
        Some(other) => {
            return Err(Error::unsupported(format!("{} is {} and not a notation file", path.display(), other.name()))
                .with_hint("notation inputs: .musicxml .mxl .mei .abc .ly .mid .json"))
        }
    };
    load_as(fmt, bytes)
}

pub fn load_as(fmt: Format, bytes: &[u8]) -> Result<Loaded> {
    let text = || String::from_utf8_lossy(bytes).to_string();
    let (score, unsupported) = match fmt {
        Format::MusicXml | Format::Mxl => musicxml::read_bytes(bytes)?,
        Format::Mei => (mei::read(&text())?, vec![]),
        Format::Abc => (abc::read(&text())?, vec![]),
        Format::Lily => (lily::read(&text())?, vec![]),
        Format::Json => {
            let s: Score =
                serde_json::from_slice(bytes).map_err(|e| Error::input(format!("not a ferraus score JSON: {e}")))?;
            (s, vec![])
        }
        Format::Midi => {
            let m = midi::read_midi(bytes)?;
            (notate::notate(&m, &NotateOptions::default()).0, vec![])
        }
        other => return Err(Error::unsupported(format!("cannot read {} as notation", other.name()))),
    };
    Ok(Loaded { score, unsupported, format: fmt })
}

/// Serialize to a text/binary format (not the engraved ones).
pub fn save(score: &Score, fmt: Format, pretty: bool, solfa_kind: SolfaKind) -> Result<Vec<u8>> {
    Ok(match fmt {
        Format::MusicXml => musicxml::write(score).into_bytes(),
        Format::Mxl => musicxml::write_mxl(score)?,
        Format::Mei => mei::write(score).into_bytes(),
        Format::Abc => abc::write(score).into_bytes(),
        Format::Lily => lily::write(score).into_bytes(),
        Format::Json => {
            if pretty {
                serde_json::to_vec_pretty(score)?
            } else {
                serde_json::to_vec(score)?
            }
        }
        Format::SolfaText => solfa::write_text(score, solfa_kind).into_bytes(),
        Format::SolfaMarkdown => solfa::write_markdown(score, solfa_kind).into_bytes(),
        Format::Midi => {
            let (m, _) = perform::perform(score, &PlayOptions::default());
            midi::write_midi(&m)?
        }
        other => return Err(Error::unsupported(format!("{} output goes through the engraver", other.name()))),
    })
}
