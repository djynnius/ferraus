//! Metadata read/write across formats via `lofty` (ID3v2, Vorbis comments, MP4 atoms,
//! RIFF INFO, AIFF text chunks, APE). Keys use friendly lowercase names (`title`, `artist`,
//! `album`, `album_artist`, `date`, `genre`, `track`, ...).

use crate::decode::ArtInfo;
use ferraus_core::{Error, Result};
use lofty::config::WriteOptions;
use lofty::picture::{Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use std::collections::BTreeMap;
use std::path::Path;

const KEYS: &[(&str, ItemKey)] = &[
    ("title", ItemKey::TrackTitle),
    ("artist", ItemKey::TrackArtist),
    ("album", ItemKey::AlbumTitle),
    ("album_artist", ItemKey::AlbumArtist),
    ("date", ItemKey::RecordingDate),
    ("year", ItemKey::Year),
    ("genre", ItemKey::Genre),
    ("track", ItemKey::TrackNumber),
    ("track_total", ItemKey::TrackTotal),
    ("disc", ItemKey::DiscNumber),
    ("disc_total", ItemKey::DiscTotal),
    ("comment", ItemKey::Comment),
    ("composer", ItemKey::Composer),
    ("bpm", ItemKey::IntegerBpm),
    ("copyright", ItemKey::CopyrightMessage),
    ("encoder", ItemKey::EncoderSoftware),
    ("encoded_by", ItemKey::EncodedBy),
    ("lyrics", ItemKey::Lyrics),
    ("label", ItemKey::Label),
    ("publisher", ItemKey::Publisher),
    ("isrc", ItemKey::Isrc),
    ("conductor", ItemKey::Conductor),
    ("lyricist", ItemKey::Lyricist),
    ("language", ItemKey::Language),
    ("key", ItemKey::InitialKey),
    ("mood", ItemKey::Mood),
    ("description", ItemKey::Description),
];

/// Friendly key names accepted by `tags --set`.
pub fn known_keys() -> Vec<&'static str> {
    KEYS.iter().map(|(k, _)| *k).collect()
}

fn normalize(k: &str) -> String {
    let k = k.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match k.as_str() {
        "albumartist" => "album_artist".into(),
        "tracknumber" => "track".into(),
        "discnumber" => "disc".into(),
        "totaltracks" | "tracktotal" => "track_total".into(),
        _ => k,
    }
}

fn item_key(k: &str, tag_type: TagType) -> Option<ItemKey> {
    let n = normalize(k);
    KEYS.iter()
        .find(|(name, _)| *name == n)
        .map(|(_, key)| *key)
        .or_else(|| ItemKey::from_key(tag_type, k))
        .or_else(|| ItemKey::from_key(tag_type, &k.to_ascii_uppercase()))
}

fn friendly(key: ItemKey, tag_type: TagType) -> String {
    KEYS.iter()
        .find(|(_, k)| *k == key)
        .map(|(n, _)| n.to_string())
        .or_else(|| key.map_key(tag_type).map(|s| s.to_ascii_lowercase()))
        .unwrap_or_else(|| format!("{key:?}").to_ascii_lowercase())
}

fn lerr(e: impl std::fmt::Display, path: &Path) -> Error {
    let msg = e.to_string();
    if msg.contains("No format could be determined") || msg.contains("Unsupported") || msg.contains("unknown") {
        Error::unsupported(format!("{}: tags are not supported for this file ({msg})", path.display()))
    } else {
        Error::input(format!("{}: cannot read or write tags ({msg})", path.display()))
    }
}

/// Tags and embedded pictures of a file.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct TagSet {
    pub tag_type: Option<String>,
    pub tags: BTreeMap<String, String>,
    pub art: Vec<ArtInfo>,
}

/// Read every tag of the primary (or first) tag block.
pub fn read(path: &Path) -> Result<TagSet> {
    let tf = lofty::read_from_path(path).map_err(|e| lerr(e, path))?;
    let mut out = TagSet::default();
    let tag = tf.primary_tag().or_else(|| tf.first_tag());
    if let Some(tag) = tag {
        let tt = tag.tag_type();
        out.tag_type = Some(format!("{tt:?}"));
        for item in tag.items() {
            if let Some(t) = item.value().text() {
                if !t.is_empty() {
                    out.tags.entry(friendly(item.key(), tt)).or_insert_with(|| t.to_string());
                }
            }
        }
        for p in tag.pictures() {
            out.art.push(ArtInfo {
                mime: p.mime_type().map(|m| m.as_str().to_string()).unwrap_or_else(|| "application/octet-stream".into()),
                bytes: p.data().len(),
                usage: Some(format!("{:?}", p.pic_type())),
            });
        }
    }
    Ok(out)
}

/// Requested changes for [`write`].
#[derive(Clone, Debug, Default)]
pub struct TagEdit {
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
    pub clear: bool,
    pub art: Option<std::path::PathBuf>,
}

impl TagEdit {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty() && !self.clear && self.art.is_none()
    }
}

/// Apply tag edits to a file in place.
pub fn write(path: &Path, edit: &TagEdit) -> Result<()> {
    let opts = WriteOptions::default();
    if edit.clear {
        let tf = lofty::read_from_path(path).map_err(|e| lerr(e, path))?;
        for t in tf.tags() {
            t.tag_type().remove_from_path(path, opts).map_err(|e| lerr(e, path))?;
        }
    }
    if edit.set.is_empty() && edit.remove.is_empty() && edit.art.is_none() {
        return Ok(());
    }
    let mut tf = lofty::read_from_path(path).map_err(|e| lerr(e, path))?;
    let tt = tf.primary_tag_type();
    if tf.primary_tag().is_none() {
        tf.insert_tag(Tag::new(tt));
    }
    let tag = tf.primary_tag_mut().expect("tag inserted above");
    for k in &edit.remove {
        let key = item_key(k, tt)
            .ok_or_else(|| Error::usage(format!("unknown tag key '{k}'; known: {}", known_keys().join(", "))))?;
        tag.remove_key(key);
    }
    for (k, v) in &edit.set {
        let key = item_key(k, tt)
            .ok_or_else(|| Error::usage(format!("unknown tag key '{k}'; known: {}", known_keys().join(", "))))?;
        if v.is_empty() {
            tag.remove_key(key);
        } else if !tag.insert_text(key, v.clone()) {
            return Err(Error::unsupported(format!(
                "tag '{k}' cannot be stored in a {tt:?} tag"
            )));
        }
    }
    if let Some(art) = &edit.art {
        let mut f = std::fs::File::open(art)?;
        let mut pic = Picture::from_reader(&mut f)
            .map_err(|e| Error::input(format!("{}: not a supported image ({e})", art.display())))?;
        pic.set_pic_type(PictureType::CoverFront);
        tag.remove_picture_type(PictureType::CoverFront);
        tag.push_picture(pic);
    }
    tf.save_to_path(path, opts).map_err(|e| lerr(e, path))?;
    Ok(())
}

/// Write tags into a freshly encoded file (best effort; formats without tag support are skipped).
pub fn apply(path: &Path, tags: &BTreeMap<String, String>) -> Result<()> {
    let set: Vec<(String, String)> = tags
        .iter()
        .filter(|(k, v)| k.as_str() != "embedded_art" && !v.is_empty())
        .filter(|(k, _)| item_key(k, TagType::VorbisComments).is_some())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if set.is_empty() {
        return Ok(());
    }
    write(path, &TagEdit { set, ..Default::default() })
}

/// Parse `key=value`.
pub fn parse_assignment(s: &str) -> Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| Error::usage(format!("--set expects key=value, got '{s}'")))?;
    if k.trim().is_empty() {
        return Err(Error::usage(format!("--set expects key=value, got '{s}'")));
    }
    Ok((normalize(k), v.to_string()))
}
