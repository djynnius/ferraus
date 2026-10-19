//! Instrument sources, sound-folder scanning, and instrument-name resolution.
//!
//! Resolution order (PRD "Sound folders and instrument switching"):
//!
//! 1. an alias from `<config>/sounds/aliases.toml` (written by `sounds alias`);
//! 2. an installed or scanned SF2/SFZ whose name, folder, or inferred category matches;
//! 3. a General MIDI program name or number, rendered with the default GM soundfont
//!    (`gm-basic` in the sounds directory, else a scanned General MIDI soundfont) or, when
//!    none is installed, the built-in preview synth;
//! 4. an explicit source: `sf2:<path>[@[bank:]program]`, `sfz:<path>`, `builtin:<timbre>`,
//!    `plugin:"Name"`, or a path ending in `.sf2`/`.sfz`.
//!
//! Explicit sources are recognized first because they cannot be confused with names.
//!
//! `aliases.toml` format:
//!
//! ```toml
//! [aliases]
//! piano = "sf2:/path/to/Salamander.sf2"   # any source string
//! strings = "vsco2-strings"                # an installed sound id: <data>/sounds/<id>/
//! lead = "Lead 2 (sawtooth)"               # or another name, resolved again (no aliases)
//! ```

use ferraus_core::{Error, Result};
use ferraus_midi::gm;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A SoundFont preset: bank 128 is the GM percussion bank.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Sf2Preset {
    pub bank: u16,
    pub program: u8,
}

/// What renders a part.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstrumentSource {
    /// The built-in preview synth. `name` is `gm` (follow the file's program changes), a
    /// timbre (`piano`, `strings`, `organ`, `guitar`, `bass`, `lead`, `pad`, `brass`,
    /// `bell`, `drums`), or `gm:<program>`.
    BuiltIn { name: String },
    /// A SoundFont. `preset: None` follows the file's program changes (General MIDI).
    Sf2 {
        path: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        preset: Option<Sf2Preset>,
    },
    /// An SFZ instrument (every channel plays it).
    Sfz { path: PathBuf },
    /// A hosted instrument plugin (rendered by `ferraus-plugins` once registered).
    Plugin {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<PathBuf>,
    },
}

impl InstrumentSource {
    pub fn builtin(name: &str) -> InstrumentSource {
        InstrumentSource::BuiltIn { name: name.to_string() }
    }

    /// Short human-readable form (`sf2:/x.sf2@0:48`, `builtin:piano`).
    pub fn display(&self) -> String {
        match self {
            InstrumentSource::BuiltIn { name } => format!("builtin:{name}"),
            InstrumentSource::Sf2 { path, preset } => match preset {
                Some(p) => format!("sf2:{}@{}:{}", path.display(), p.bank, p.program),
                None => format!("sf2:{}", path.display()),
            },
            InstrumentSource::Sfz { path } => format!("sfz:{}", path.display()),
            InstrumentSource::Plugin { name, .. } => format!("plugin:{name}"),
        }
    }
}

/// Built-in timbres and the GM program each stands for.
pub const BUILTIN_TIMBRES: &[(&str, u8)] = &[
    ("piano", 0),
    ("bell", 14),
    ("organ", 16),
    ("guitar", 24),
    ("pluck", 24),
    ("bass", 33),
    ("strings", 48),
    ("brass", 61),
    ("lead", 81),
    ("pad", 89),
];

/// Words that mean the GM drum kit.
const DRUM_WORDS: &[&str] = &["drums", "drum", "drum kit", "kit", "percussion", "gm drums"];

/// Parse an explicit source string. Returns `None` when `s` is a name, not a source.
pub fn parse_source(s: &str) -> Result<Option<InstrumentSource>> {
    let t = s.trim();
    let unq = |v: &str| v.trim().trim_matches('"').trim_matches('\'').to_string();
    if let Some(rest) = t.strip_prefix("sf2:") {
        let rest = unq(rest);
        let (path, preset) = match rest.rsplit_once('@') {
            Some((p, spec)) if !spec.contains('/') && !spec.contains('\\') => {
                let (bank, prog) = match spec.split_once(':') {
                    Some((b, p)) => (b.trim().parse::<u16>().ok(), p.trim().parse::<u8>().ok()),
                    None => (Some(0), spec.trim().parse::<u8>().ok()),
                };
                match (bank, prog) {
                    (Some(bank), Some(program)) if program < 128 => {
                        (PathBuf::from(unq(p)), Some(Sf2Preset { bank, program }))
                    }
                    _ => return Err(Error::usage(format!("'{s}': preset must be @<program> or @<bank>:<program>"))),
                }
            }
            _ => (PathBuf::from(&rest), None),
        };
        return Ok(Some(InstrumentSource::Sf2 { path, preset }));
    }
    if let Some(rest) = t.strip_prefix("sfz:") {
        return Ok(Some(InstrumentSource::Sfz { path: PathBuf::from(unq(rest)) }));
    }
    if let Some(rest) = t.strip_prefix("plugin:") {
        let name = unq(rest);
        if name.is_empty() {
            return Err(Error::usage("plugin: needs a plugin name or path").with_hint("ferraus plugin scan"));
        }
        return Ok(Some(InstrumentSource::Plugin { name, preset: None, state: None }));
    }
    if let Some(rest) = t.strip_prefix("builtin:") {
        let n = gm::normalize(rest);
        if n == "gm" || BUILTIN_TIMBRES.iter().any(|(b, _)| *b == n) || DRUM_WORDS.contains(&n.as_str()) {
            let name = if DRUM_WORDS.contains(&n.as_str()) { "drums".to_string() } else { n };
            return Ok(Some(InstrumentSource::builtin(&name)));
        }
        return match gm::lookup_program(rest) {
            Ok(p) => Ok(Some(InstrumentSource::builtin(&format!("gm:{p}")))),
            Err(_) => Err(Error::usage(format!(
                "unknown built-in timbre '{rest}'; choose one of: gm, drums, {}",
                BUILTIN_TIMBRES.iter().map(|t| t.0).collect::<Vec<_>>().join(", ")
            ))),
        };
    }
    let lower = t.to_ascii_lowercase();
    if lower.ends_with(".sf2") {
        return Ok(Some(InstrumentSource::Sf2 { path: PathBuf::from(unq(t)), preset: None }));
    }
    if lower.ends_with(".sfz") {
        return Ok(Some(InstrumentSource::Sfz { path: PathBuf::from(unq(t)) }));
    }
    Ok(None)
}

/// Kind of a scanned sound file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SoundKind {
    Sf2,
    Sfz,
}

/// One instrument found in a sound folder.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SoundEntry {
    /// File stem (`SalamanderGrandPiano`).
    pub name: String,
    pub path: PathBuf,
    pub kind: SoundKind,
    /// Inferred category: keys, strings, guitar, bass, drums, synth, pad, brass, winds,
    /// vocal, fx — or `gm` for General MIDI soundfonts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Lowercase words from the file name and the folders below the scanned root.
    pub tags: Vec<String>,
}

impl SoundEntry {
    pub fn source(&self) -> InstrumentSource {
        match self.kind {
            SoundKind::Sf2 => InstrumentSource::Sf2 { path: self.path.clone(), preset: None },
            SoundKind::Sfz => InstrumentSource::Sfz { path: self.path.clone() },
        }
    }
}

/// Category keywords, most specific first.
const CATEGORY_WORDS: &[(&str, &[&str])] = &[
    ("gm", &["gm", "general", "generaluser", "fluidr3", "fluid", "gs", "timgm", "musescore"]),
    (
        "drums",
        &["drum", "drums", "kit", "kits", "percussion", "perc", "kick", "snare", "hihat", "cymbal", "cymbals", "toms"],
    ),
    ("bass", &["bass", "basses", "contrabass", "upright", "808"]),
    ("guitar", &["guitar", "guitars", "nylon", "steel", "strat", "gtr", "ukulele", "banjo", "mandolin"]),
    (
        "keys",
        &[
            "piano",
            "pianos",
            "grand",
            "upright piano",
            "keys",
            "rhodes",
            "epiano",
            "wurlitzer",
            "wurli",
            "clav",
            "clavinet",
            "harpsichord",
            "organ",
            "celesta",
        ],
    ),
    (
        "strings",
        &[
            "string",
            "strings",
            "violin",
            "violins",
            "viola",
            "violas",
            "cello",
            "cellos",
            "ensemble",
            "orchestra",
            "orchestral",
            "harp",
            "pizzicato",
        ],
    ),
    ("brass", &["brass", "trumpet", "trumpets", "trombone", "trombones", "horn", "horns", "tuba", "flugelhorn"]),
    (
        "winds",
        &[
            "winds",
            "woodwind",
            "woodwinds",
            "flute",
            "flutes",
            "clarinet",
            "clarinets",
            "oboe",
            "bassoon",
            "sax",
            "saxophone",
            "recorder",
            "piccolo",
        ],
    ),
    ("vocal", &["vocal", "vocals", "voice", "voices", "choir", "vox", "aah", "ooh", "soprano", "alto", "tenor"]),
    ("pad", &["pad", "pads", "ambient", "atmosphere", "atmos", "drone", "texture", "warm"]),
    ("synth", &["synth", "synths", "lead", "leads", "saw", "square", "analog", "chip", "8bit", "pluck"]),
    ("fx", &["fx", "sfx", "effect", "effects", "noise", "foley", "impact", "riser"]),
];

/// Split a name into lowercase words (camelCase and digits split too).
pub fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in s.chars() {
        let boundary = match prev {
            Some(p) => p.is_lowercase() && c.is_uppercase(),
            None => false,
        };
        if !c.is_alphanumeric() || boundary {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        }
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Infer a category from words of a file name and its folders.
pub fn infer_category(tags: &[String]) -> Option<&'static str> {
    for (cat, kws) in CATEGORY_WORDS {
        if tags.iter().any(|t| kws.contains(&t.as_str())) {
            return Some(cat);
        }
    }
    None
}

/// Find every `.sf2` and `.sfz` under `dir` (recursively, symlinks not followed), with an
/// inferred category for each. Results are sorted by path. Missing folders give nothing.
pub fn scan_sound_folder(dir: &Path) -> Vec<SoundEntry> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            visited += 1;
            if visited > 50_000 {
                break;
            }
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < 8 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            let kind = match ext.as_str() {
                "sf2" => SoundKind::Sf2,
                "sfz" => SoundKind::Sfz,
                _ => continue,
            };
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let mut tags = words(&stem);
            if let Ok(rel) = p.parent().unwrap_or(&p).strip_prefix(dir) {
                for c in rel.components() {
                    tags.extend(words(&c.as_os_str().to_string_lossy()));
                }
            }
            tags.dedup();
            let category = infer_category(&tags).map(String::from);
            out.push(SoundEntry { name: stem, path: p, kind, category, tags });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The result of resolving an instrument name.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Resolved {
    /// What the user typed.
    pub given: String,
    pub source: InstrumentSource,
    /// `alias`, `sound`, `gm`, or `explicit`.
    pub via: String,
    /// GM program the name stands for, when it is a GM name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl Resolved {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Resolves instrument names against aliases, scanned sounds, and General MIDI.
#[derive(Clone, Debug, Default)]
pub struct Resolver {
    pub aliases: BTreeMap<String, String>,
    pub sounds: Vec<SoundEntry>,
    /// Default GM soundfont (`gm-basic`), when installed.
    pub gm_soundfont: Option<PathBuf>,
    /// Directory of installed sounds (for alias targets that are sound ids).
    pub sounds_dir: Option<PathBuf>,
}

/// Path of the alias file written by `sounds alias`.
pub fn aliases_path() -> PathBuf {
    ferraus_core::paths::config_dir().join("sounds").join("aliases.toml")
}

/// Read `[aliases]` from an aliases.toml (missing or malformed files give no aliases).
pub fn read_aliases(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    let Ok(v) = text.parse::<toml::Table>() else { return out };
    if let Some(toml::Value::Table(t)) = v.get("aliases") {
        for (k, v) in t {
            if let toml::Value::String(s) = v {
                out.insert(gm::normalize(k), s.clone());
            }
        }
    }
    out
}

impl Resolver {
    /// Aliases from the config dir, sounds from `<data>/sounds` plus `sound_paths`.
    pub fn discover(sound_paths: &[PathBuf]) -> Resolver {
        let sounds_dir = ferraus_core::paths::sounds_dir();
        let mut sounds = scan_sound_folder(&sounds_dir);
        for p in sound_paths {
            for e in scan_sound_folder(p) {
                if !sounds.iter().any(|s| s.path == e.path) {
                    sounds.push(e);
                }
            }
        }
        let gm_soundfont = find_gm_basic(&sounds_dir);
        Resolver { aliases: read_aliases(&aliases_path()), sounds, gm_soundfont, sounds_dir: Some(sounds_dir) }
    }

    /// The soundfont used for General MIDI names: `gm-basic`, else a scanned GM soundfont.
    pub fn default_gm(&self) -> Option<PathBuf> {
        self.gm_soundfont.clone().or_else(|| {
            self.sounds
                .iter()
                .find(|s| s.kind == SoundKind::Sf2 && s.category.as_deref() == Some("gm"))
                .map(|s| s.path.clone())
        })
    }

    /// Resolve a name or source string.
    pub fn resolve(&self, name: &str) -> Result<Resolved> {
        self.resolve_depth(name, 0, true)
    }

    fn resolve_depth(&self, name: &str, depth: usize, use_aliases: bool) -> Result<Resolved> {
        let given = name.trim().to_string();
        if given.is_empty() {
            return Err(Error::usage("--instrument is empty"));
        }
        // explicit sources
        if let Some(src) = parse_source(&given)? {
            return Ok(Resolved {
                given,
                source: src,
                via: "explicit".into(),
                program: None,
                program_name: None,
                alias: None,
            });
        }
        let n = gm::normalize(&given);
        // 1. alias
        if use_aliases && depth < 4 {
            if let Some(target) = self.aliases.get(&n) {
                let mut r = self.resolve_alias_target(target, depth)?;
                r.given = given;
                r.alias = Some(target.clone());
                r.via = "alias".into();
                return Ok(r);
            }
        }
        // `gm` = follow the file's programs
        if n == "gm" || n == "default" || n == "general midi" {
            return Ok(Resolved {
                given,
                source: match self.default_gm() {
                    Some(p) => InstrumentSource::Sf2 { path: p, preset: None },
                    None => InstrumentSource::builtin("gm"),
                },
                via: "gm".into(),
                program: None,
                program_name: None,
                alias: None,
            });
        }
        // 2. scanned sounds
        let matches = self.match_sounds(&n);
        match matches.len() {
            0 => {}
            1 => {
                return Ok(Resolved {
                    given,
                    source: matches[0].source(),
                    via: "sound".into(),
                    program: None,
                    program_name: None,
                    alias: None,
                })
            }
            _ => {
                let list: Vec<String> = matches
                    .iter()
                    .map(|m| {
                        format!(
                            "{} ({}{}) — hint: --instrument {}:\"{}\"  or  ferraus sounds alias {}={}:\"{}\"",
                            m.name,
                            if m.kind == SoundKind::Sf2 { "sf2" } else { "sfz" },
                            m.category.as_deref().map(|c| format!(", {c}")).unwrap_or_default(),
                            if m.kind == SoundKind::Sf2 { "sf2" } else { "sfz" },
                            m.path.display(),
                            n.replace(' ', "-"),
                            if m.kind == SoundKind::Sf2 { "sf2" } else { "sfz" },
                            m.path.display(),
                        )
                    })
                    .collect();
                let first = &matches[0];
                return Err(Error::new(
                    ferraus_core::ErrorKind::Usage,
                    "ambiguous_instrument",
                    format!("'{given}' matches {} installed sounds:\n  {}", matches.len(), list.join("\n  ")),
                )
                .with_hint(format!(
                    "ferraus sounds alias {}={}:\"{}\"",
                    n.replace(' ', "-"),
                    if first.kind == SoundKind::Sf2 { "sf2" } else { "sfz" },
                    first.path.display()
                )));
            }
        }
        // 3. General MIDI names and numbers, and drums
        if DRUM_WORDS.contains(&n.as_str()) {
            return Ok(Resolved {
                given,
                source: match self.default_gm() {
                    Some(p) => InstrumentSource::Sf2 { path: p, preset: Some(Sf2Preset { bank: 128, program: 0 }) },
                    None => InstrumentSource::builtin("drums"),
                },
                via: "gm".into(),
                program: None,
                program_name: Some("Drum kit".into()),
                alias: None,
            });
        }
        match gm::lookup_program(&given) {
            Ok(p) => {
                let timbre = BUILTIN_TIMBRES.iter().find(|(t, _)| *t == n).map(|t| t.0);
                Ok(Resolved {
                    given,
                    source: match self.default_gm() {
                        Some(path) => InstrumentSource::Sf2 {
                            path,
                            preset: Some(Sf2Preset { bank: 0, program: p }),
                        },
                        None => InstrumentSource::builtin(&timbre.map(String::from).unwrap_or(format!("gm:{p}"))),
                    },
                    via: "gm".into(),
                    program: Some(p),
                    program_name: Some(gm::program_name(p).to_string()),
                    alias: None,
                })
            }
            Err(cands) if !cands.is_empty() => {
                let list: Vec<String> = cands
                    .iter()
                    .take(12)
                    .map(|p| format!("{} — hint: --instrument gm:{p}", gm::program_name(*p)))
                    .collect();
                Err(Error::new(
                    ferraus_core::ErrorKind::Usage,
                    "ambiguous_instrument",
                    format!("'{given}' matches several General MIDI programs:\n  {}", list.join("\n  ")),
                )
                .with_hint(format!("ferraus render <part.mid> --instrument gm:{}", cands[0])))
            }
            Err(_) => Err(Error::usage(format!(
                "unknown instrument '{given}': not an alias, installed sound, General MIDI name, or source (sf2:<path>, sfz:<path>, plugin:<name>, builtin:<timbre>)"
            ))
            .with_hint("ferraus sounds list")),
        }
    }

    fn resolve_alias_target(&self, target: &str, depth: usize) -> Result<Resolved> {
        if let Some(src) = parse_source(target)? {
            return Ok(Resolved {
                given: target.to_string(),
                source: src,
                via: "alias".into(),
                program: None,
                program_name: None,
                alias: None,
            });
        }
        if let Some(dir) = &self.sounds_dir {
            let p = dir.join(target);
            if p.exists() {
                let found = if p.is_dir() { scan_sound_folder(&p) } else { vec![] };
                if let Some(e) = found.iter().find(|e| e.kind == SoundKind::Sfz).or_else(|| found.first()) {
                    return Ok(Resolved {
                        given: target.to_string(),
                        source: e.source(),
                        via: "alias".into(),
                        program: None,
                        program_name: None,
                        alias: None,
                    });
                }
            }
        }
        self.resolve_depth(target, depth + 1, false)
    }

    /// Scanned sounds matching a normalized query: exact names first, then entries whose
    /// words contain every query word or whose category equals the query.
    pub fn match_sounds(&self, q: &str) -> Vec<&SoundEntry> {
        let exact: Vec<&SoundEntry> =
            self.sounds.iter().filter(|s| gm::normalize(&s.name) == q || words(&s.name).join(" ") == q).collect();
        if !exact.is_empty() {
            return exact;
        }
        let qw: Vec<&str> = q.split(' ').collect();
        let cat_alias = match q {
            "piano" | "keys" | "keyboard" => Some("keys"),
            "string" | "strings" | "ensemble" => Some("strings"),
            "drums" | "drum" | "kit" => Some("drums"),
            "choir" | "voice" | "vocals" => Some("vocal"),
            "synth" | "lead" => Some("synth"),
            other => CATEGORY_WORDS.iter().find(|(c, _)| *c == other).map(|(c, _)| *c),
        };
        self.sounds
            .iter()
            .filter(|s| s.category.as_deref() != Some("gm") || qw.iter().all(|w| s.tags.iter().any(|t| t == w)))
            .filter(|s| {
                qw.iter().all(|w| s.tags.iter().any(|t| t == w || (w.len() >= 4 && t.starts_with(w))))
                    || (cat_alias.is_some()
                        && s.category.as_deref() == cat_alias
                        && qw.len() == 1
                        && cat_alias != Some("gm"))
            })
            .collect()
    }
}

/// `<sounds>/gm-basic.sf2` or the first `.sf2` under `<sounds>/gm-basic*/`.
pub fn find_gm_basic(sounds_dir: &Path) -> Option<PathBuf> {
    let f = sounds_dir.join("gm-basic.sf2");
    if f.is_file() {
        return Some(f);
    }
    let rd = std::fs::read_dir(sounds_dir).ok()?;
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("gm-basic"))
                    .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    for d in dirs {
        if let Some(e) = scan_sound_folder(&d).into_iter().find(|e| e.kind == SoundKind::Sf2) {
            return Some(e.path);
        }
    }
    None
}

/// GM program a built-in name stands for (`None` = follow the file; `Some(None)` = drums).
pub fn builtin_program(name: &str) -> Option<Option<u8>> {
    let n = gm::normalize(name);
    if n == "gm" {
        return None;
    }
    if n == "drums" {
        return Some(None);
    }
    if let Some(p) = n.strip_prefix("gm ") {
        return p.trim().parse::<u8>().ok().filter(|p| *p < 128).map(Some);
    }
    BUILTIN_TIMBRES
        .iter()
        .find(|(t, _)| *t == n)
        .map(|(_, p)| Some(*p))
        .or_else(|| gm::lookup_program(name).ok().map(Some))
}

/// JSON summary of a resolver's view of the sound folders (for dry runs).
pub fn sounds_summary(r: &Resolver) -> Value {
    json!({
        "aliases": r.aliases.len(),
        "sounds": r.sounds.len(),
        "gm_soundfont": r.default_gm().map(|p| p.display().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_sources() {
        assert_eq!(
            parse_source("sf2:/a/b.sf2@48").unwrap(),
            Some(InstrumentSource::Sf2 { path: "/a/b.sf2".into(), preset: Some(Sf2Preset { bank: 0, program: 48 }) })
        );
        assert_eq!(
            parse_source("sf2:\"/a/My Font.sf2\"@128:0").unwrap(),
            Some(InstrumentSource::Sf2 {
                path: "/a/My Font.sf2".into(),
                preset: Some(Sf2Preset { bank: 128, program: 0 })
            })
        );
        assert_eq!(parse_source("sfz:x/y.sfz").unwrap(), Some(InstrumentSource::Sfz { path: "x/y.sfz".into() }));
        assert!(
            matches!(parse_source("plugin:\"Surge XT\"").unwrap(), Some(InstrumentSource::Plugin { name, .. }) if name == "Surge XT")
        );
        assert_eq!(parse_source("builtin:strings").unwrap(), Some(InstrumentSource::builtin("strings")));
        assert_eq!(parse_source("builtin:Trumpet").unwrap(), Some(InstrumentSource::builtin("gm:56")));
        assert_eq!(parse_source("piano").unwrap(), None);
        assert!(parse_source("sf2:/a.sf2@nope").is_err());
    }

    #[test]
    fn categories_and_words() {
        assert_eq!(words("SalamanderGrandPianoV3"), vec!["salamander", "grand", "piano", "v3"]);
        let t = words("VSCO2 Violin Section");
        assert_eq!(infer_category(&t), Some("strings"));
        assert_eq!(infer_category(&words("FluidR3_GM")), Some("gm"));
        assert_eq!(infer_category(&words("Fingered Bass Guitar")), Some("bass"));
        assert_eq!(infer_category(&words("Warm Pad")), Some("pad"));
        assert_eq!(infer_category(&words("random")), None);
    }

    #[test]
    fn gm_names_without_sounds() {
        let r = Resolver::default();
        let x = r.resolve("piano").unwrap();
        assert_eq!(x.source, InstrumentSource::builtin("piano"));
        assert_eq!(x.program, Some(0));
        let x = r.resolve("String Ensemble 1").unwrap();
        assert_eq!(x.source, InstrumentSource::builtin("gm:48"));
        assert_eq!(r.resolve("gm:48").unwrap().program, Some(48));
        assert_eq!(r.resolve("drums").unwrap().source, InstrumentSource::builtin("drums"));
        assert_eq!(r.resolve("gm").unwrap().source, InstrumentSource::builtin("gm"));
        let e = r.resolve("electric").unwrap_err();
        assert_eq!(e.code, "ambiguous_instrument");
        assert!(e.message.contains("gm:"));
        assert!(r.resolve("zzzz").is_err());
        assert!(matches!(r.resolve("plugin:Surge XT").unwrap().source, InstrumentSource::Plugin { .. }));
    }

    #[test]
    fn builtin_programs() {
        assert_eq!(builtin_program("gm"), None);
        assert_eq!(builtin_program("drums"), Some(None));
        assert_eq!(builtin_program("strings"), Some(Some(48)));
        assert_eq!(builtin_program("gm:5"), Some(Some(5)));
    }
}
