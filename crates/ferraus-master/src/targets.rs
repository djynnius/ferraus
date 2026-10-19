//! Delivery targets: the PRD table, as data in the binary. `--target custom` (or explicit
//! `--lufs` / `--true-peak`) overrides it.

use ferraus_core::prelude::*;
use serde::{Deserialize, Serialize};

/// Target names accepted by `--target`.
pub const TARGET_NAMES: &[&str] = &["streaming", "apple", "youtube", "club", "cd", "broadcast", "podcast", "custom"];

/// One row of the delivery-target table.
#[derive(Clone, Copy, Debug)]
pub struct TargetRow {
    pub name: &'static str,
    pub lufs: f64,
    pub true_peak_dbtp: f64,
    /// Mono podcast loudness (only `podcast` differs).
    pub mono_lufs: Option<f64>,
    pub bit_depth: Option<u16>,
    pub sample_rate: Option<u32>,
    pub notes: &'static str,
}

/// The delivery targets (common platform practice).
pub const TARGETS: &[TargetRow] = &[
    TargetRow {
        name: "streaming",
        lufs: -14.0,
        true_peak_dbtp: -1.0,
        mono_lufs: None,
        bit_depth: None,
        sample_rate: None,
        notes: "Spotify, YouTube, Tidal, Amazon normalize around this level",
    },
    TargetRow {
        name: "apple",
        lufs: -16.0,
        true_peak_dbtp: -1.0,
        mono_lufs: None,
        bit_depth: None,
        sample_rate: None,
        notes: "Apple Music and Apple Podcasts guidance",
    },
    TargetRow {
        name: "youtube",
        lufs: -14.0,
        true_peak_dbtp: -1.0,
        mono_lufs: None,
        bit_depth: None,
        sample_rate: None,
        notes: "same as streaming, kept as a named alias",
    },
    TargetRow {
        name: "club",
        lufs: -8.0,
        true_peak_dbtp: -0.3,
        mono_lufs: None,
        bit_depth: None,
        sample_rate: None,
        notes: "DJ and club playback, no normalization",
    },
    TargetRow {
        name: "cd",
        lufs: -10.0,
        true_peak_dbtp: -0.3,
        mono_lufs: None,
        bit_depth: Some(16),
        sample_rate: Some(44100),
        notes: "16-bit 44.1 kHz with dither",
    },
    TargetRow {
        name: "broadcast",
        lufs: -23.0,
        true_peak_dbtp: -1.0,
        mono_lufs: None,
        bit_depth: None,
        sample_rate: None,
        notes: "EBU R128; --standard atsc switches to -24 LKFS and -2 dBTP",
    },
    TargetRow {
        name: "podcast",
        lufs: -16.0,
        true_peak_dbtp: -1.0,
        mono_lufs: Some(-19.0),
        bit_depth: None,
        sample_rate: None,
        notes: "-16 LUFS stereo, -19 LUFS mono; the speech chain adds leveler and deess",
    },
];

/// A resolved delivery spec.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Target {
    pub name: String,
    pub lufs: f64,
    pub true_peak_dbtp: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standard: Option<String>,
    /// Output bit depth (`None` = the global `--bit-depth`, else 24-bit PCM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u16>,
    /// Output sample rate (`None` = keep the input's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// `triangular` (TPDF) when writing 16-bit or less.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dither: Option<String>,
    #[serde(default)]
    pub notes: String,
}

impl Target {
    pub fn is_speech(&self) -> bool {
        self.name == "podcast"
    }
}

pub fn row(name: &str) -> Option<&'static TargetRow> {
    TARGETS.iter().find(|t| t.name == name)
}

/// Resolve `--target`, `--lufs`, `--true-peak`, `--standard` for an input with `channels`.
pub fn resolve(a: &Args, channels: usize) -> Result<Target> {
    let name = a.str_or("target", "streaming");
    let standard = a.str("standard").filter(|s| !s.is_empty());
    let mut t = if name == "custom" {
        let lufs = a
            .opt_db("lufs")?
            .ok_or_else(|| Error::usage("--target custom needs --lufs (and optionally --true-peak, default -1dBTP)"))?;
        let tp = a.opt_db("true-peak")?.unwrap_or(-1.0);
        Target {
            name: "custom".into(),
            lufs,
            true_peak_dbtp: tp,
            standard: None,
            bit_depth: None,
            sample_rate: None,
            dither: None,
            notes: "custom target from --lufs/--true-peak".into(),
        }
    } else {
        let r = row(&name).ok_or_else(|| {
            Error::usage(format!("unknown --target '{name}'; one of: {}", TARGET_NAMES.join(", ")))
        })?;
        let mut lufs = r.lufs;
        let mut tp = r.true_peak_dbtp;
        let mut notes = r.notes.to_string();
        if let (Some(m), true) = (r.mono_lufs, channels == 1) {
            lufs = m;
            notes = format!("{notes} (mono input: {m} LUFS)");
        }
        let mut std_used = None;
        if r.name == "broadcast" {
            match standard.as_deref().unwrap_or("ebu") {
                "atsc" => {
                    lufs = -24.0;
                    tp = -2.0;
                    std_used = Some("atsc".to_string());
                    notes = "ATSC A/85: -24 LKFS, -2 dBTP".into();
                }
                _ => std_used = Some("ebu".to_string()),
            }
        }
        // explicit --lufs / --true-peak refine a named target
        if a.is_explicit("lufs") {
            if let Some(v) = a.opt_db("lufs")? {
                lufs = v;
                notes = format!("{notes}; --lufs override");
            }
        }
        if a.is_explicit("true-peak") {
            if let Some(v) = a.opt_db("true-peak")? {
                tp = v;
                notes = format!("{notes}; --true-peak override");
            }
        }
        Target {
            name: r.name.into(),
            lufs,
            true_peak_dbtp: tp,
            standard: std_used,
            bit_depth: r.bit_depth,
            sample_rate: r.sample_rate,
            dither: None,
            notes,
        }
    };
    if !(-40.0..=0.0).contains(&t.lufs) {
        return Err(Error::usage(format!("target loudness {} LUFS is outside -40..0", t.lufs)));
    }
    if !(-12.0..=0.0).contains(&t.true_peak_dbtp) {
        return Err(Error::usage(format!("true-peak ceiling {} dBTP is outside -12..0", t.true_peak_dbtp)));
    }
    if t.bit_depth.map(|b| b <= 16).unwrap_or(false) {
        t.dither = Some("triangular".into());
    }
    Ok(t)
}

/// The target table as JSON (for docs and `master analyze`).
pub fn table_json() -> Value {
    Value::Array(
        TARGETS
            .iter()
            .map(|t| {
                json!({"target": t.name, "lufs": t.lufs, "true_peak_dbtp": t.true_peak_dbtp,
                       "mono_lufs": t.mono_lufs, "bit_depth": t.bit_depth, "sample_rate": t.sample_rate,
                       "notes": t.notes})
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_prd() {
        let want = [
            ("streaming", -14.0, -1.0),
            ("apple", -16.0, -1.0),
            ("youtube", -14.0, -1.0),
            ("club", -8.0, -0.3),
            ("cd", -10.0, -0.3),
            ("broadcast", -23.0, -1.0),
            ("podcast", -16.0, -1.0),
        ];
        for (n, l, tp) in want {
            let r = row(n).unwrap();
            assert_eq!((r.lufs, r.true_peak_dbtp), (l, tp), "{n}");
        }
        assert_eq!(row("podcast").unwrap().mono_lufs, Some(-19.0));
        assert_eq!(row("cd").unwrap().bit_depth, Some(16));
        assert_eq!(row("cd").unwrap().sample_rate, Some(44100));
    }
}
