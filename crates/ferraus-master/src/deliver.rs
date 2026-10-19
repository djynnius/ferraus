//! `master deliver`: the deliverable set (wav24, wav16, mp3, flac, aac) with tags and a
//! loudness sheet.

use crate::util::{jnum, round_to};
use ferraus_analysis::loudness;
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, Quality};
use ferraus_core::EncodeOptions;
use std::collections::BTreeMap;
use std::path::Path;

/// One deliverable format.
#[derive(Clone, Copy, Debug)]
pub struct DeliveryFormat {
    pub key: &'static str,
    pub ext: &'static str,
    pub codec: &'static str,
    pub bit_depth: Option<u16>,
    /// Resample to 44.1 kHz when the master is at another rate.
    pub cd_rate: bool,
    pub bitrate: Option<u32>,
}

pub const FORMATS: &[DeliveryFormat] = &[
    DeliveryFormat { key: "wav24", ext: "wav", codec: "wav", bit_depth: Some(24), cd_rate: false, bitrate: None },
    DeliveryFormat { key: "wav16", ext: "wav", codec: "wav", bit_depth: Some(16), cd_rate: true, bitrate: None },
    DeliveryFormat { key: "mp3", ext: "mp3", codec: "mp3", bit_depth: None, cd_rate: true, bitrate: Some(320_000) },
    DeliveryFormat { key: "flac", ext: "flac", codec: "flac", bit_depth: Some(24), cd_rate: false, bitrate: None },
    DeliveryFormat { key: "aac", ext: "m4a", codec: "m4a", bit_depth: None, cd_rate: true, bitrate: Some(256_000) },
];

pub fn format(key: &str) -> Option<&'static DeliveryFormat> {
    FORMATS.iter().find(|f| f.key == key)
}

/// Parse repeatable `--tags` values (`title=Song,artist=Me`; a comma only starts a new tag
/// when it is followed by `key=`, so values may contain commas).
pub fn parse_tags(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for v in values {
        let mut current: Option<(String, String)> = None;
        for part in v.split(',') {
            let starts_key = part
                .split_once('=')
                .map(|(k, _)| {
                    let k = k.trim();
                    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                })
                .unwrap_or(false);
            if starts_key {
                if let Some((k, val)) = current.take() {
                    out.insert(k, val);
                }
                let (k, val) = part.split_once('=').unwrap();
                current = Some((k.trim().to_ascii_lowercase().replace('-', "_"), val.trim().to_string()));
            } else if let Some((_, val)) = current.as_mut() {
                val.push(',');
                val.push_str(part);
            } else if !part.trim().is_empty() {
                return Err(Error::usage(format!("--tags: '{part}' is not key=value (e.g. title=My Song)")));
            }
        }
        if let Some((k, val)) = current.take() {
            out.insert(k, val);
        }
    }
    Ok(out)
}

/// Validate an ISRC (`CC-XXX-YY-NNNNN`, hyphens optional) and return it without hyphens.
pub fn parse_isrc(s: &str) -> Result<String> {
    let c: String = s.chars().filter(|c| *c != '-' && !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
    let ok = c.len() == 12
        && c[..2].chars().all(|x| x.is_ascii_alphabetic())
        && c[2..5].chars().all(|x| x.is_ascii_alphanumeric())
        && c[5..].chars().all(|x| x.is_ascii_digit());
    if ok {
        Ok(c)
    } else {
        Err(Error::usage(format!("--isrc '{s}' is not a valid ISRC (CC-XXX-YY-NNNNN, e.g. US-RC1-76-07839)")))
    }
}

/// Loudness measurements of one deliverable.
pub fn measure_row(
    key: &str,
    path: &Path,
    buf: &AudioBuffer,
    bit_depth: Option<u16>,
    bitrate: Option<u32>,
    measured: &str,
) -> Value {
    let l = loudness::loudness(buf);
    json!({
        "format": key,
        "path": path.display().to_string(),
        "sample_rate": buf.sample_rate,
        "channels": buf.channels(),
        "bit_depth": bit_depth,
        "bitrate": bitrate,
        "duration_s": round_to(buf.duration_s(), 3),
        "integrated_lufs": jnum(l.integrated_lufs, 2),
        "loudness_range_lu": jnum(l.loudness_range_lu, 2),
        "short_term_max_lufs": jnum(l.short_term_max_lufs, 2),
        "true_peak_dbtp": jnum(l.true_peak_dbtp, 2),
        "sample_peak_dbfs": jnum(l.sample_peak_dbfs, 2),
        "plr_db": jnum(l.plr_db, 2),
        "measured": measured,
    })
}

/// CSV rendering of the loudness sheet.
pub fn sheet_csv(rows: &[Value]) -> String {
    let cols = [
        "format",
        "path",
        "sample_rate",
        "channels",
        "bit_depth",
        "bitrate",
        "duration_s",
        "integrated_lufs",
        "loudness_range_lu",
        "short_term_max_lufs",
        "true_peak_dbtp",
        "sample_peak_dbfs",
        "plr_db",
        "measured",
    ];
    let mut s = cols.join(",");
    s.push('\n');
    for r in rows {
        let cells: Vec<String> = cols
            .iter()
            .map(|c| match &r[*c] {
                Value::Null => String::new(),
                Value::String(v) => {
                    if v.contains([',', '"', '\n']) {
                        format!("\"{}\"", v.replace('"', "\"\""))
                    } else {
                        v.clone()
                    }
                }
                other => other.to_string(),
            })
            .collect();
        s.push_str(&cells.join(","));
        s.push('\n');
    }
    s
}

/// Resample to 44.1 kHz when needed, re-limiting any new inter-sample overs to the
/// master's own true peak.
pub fn to_cd_rate(buf: &AudioBuffer) -> (AudioBuffer, Option<String>) {
    if buf.sample_rate == 44100 {
        return (buf.clone(), None);
    }
    let tp_in = loudness::true_peak_db(buf);
    let mut out = resample(buf, 44100, Quality::High);
    let tp_out = loudness::true_peak_db(&out);
    if tp_in.is_finite() && tp_out > tp_in + 0.05 {
        let p = ferraus_dsp::dynamics::LimiterParams { ceiling_db: tp_in as f32, ..Default::default() };
        ferraus_dsp::dynamics::limit(&mut out.data, 44100.0, &p);
        return (
            out,
            Some(format!(
                "resampled {} Hz → 44100 Hz; new inter-sample peaks ({tp_out:.2} dBTP) limited back to {tp_in:.2} dBTP",
                buf.sample_rate
            )),
        );
    }
    (out, Some(format!("resampled {} Hz → 44100 Hz", buf.sample_rate)))
}

/// Encoding options for a deliverable.
pub fn encode_options(f: &DeliveryFormat, ctx: &OpCtx, tags: &BTreeMap<String, String>) -> EncodeOptions {
    EncodeOptions {
        format: Some(f.codec.to_string()),
        bit_depth: f.bit_depth,
        float: false,
        bitrate: ctx.globals.bitrate.or(f.bitrate),
        vbr: false,
        dither: f.bit_depth.map(|_| "triangular".to_string()),
        seed: ctx.globals.seed,
        tags: tags.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_isrc() {
        let t = parse_tags(&["title=Hello, World,artist=Me".into(), "album=X".into()]).unwrap();
        assert_eq!(t["title"], "Hello, World");
        assert_eq!(t["artist"], "Me");
        assert_eq!(t["album"], "X");
        assert!(parse_tags(&["nonsense".into()]).is_err());
        assert_eq!(parse_isrc("US-RC1-76-07839").unwrap(), "USRC17607839");
        assert!(parse_isrc("12345").is_err());
    }

    #[test]
    fn csv_escapes() {
        let rows = vec![json!({"format": "wav24", "path": "a,b.wav", "integrated_lufs": -14.0})];
        let s = sheet_csv(&rows);
        assert!(s.contains("\"a,b.wav\""));
        assert!(s.lines().count() == 2);
    }
}
