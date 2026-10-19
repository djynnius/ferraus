//! `master verify`: pass or fail against a delivery spec.

use crate::targets::Target;
use crate::util::{fmt_db, jnum, round_to};
use ferraus_analysis::levels::{detect_clipping, detect_silence};
use ferraus_analysis::spectrum::{band_power_fraction, ltas};
use ferraus_analysis::{loudness, stereo};
use ferraus_core::prelude::*;
use ferraus_core::SourceInfo;
use serde::Serialize;

/// Largest acceptable DC offset (linear, about -60 dBFS).
pub const MAX_DC: f64 = 0.001;
/// Longest acceptable silence before the first sound (seconds).
pub const MAX_HEAD_SILENCE_S: f64 = 1.0;

/// One check.
#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub check: &'static str,
    /// `pass`, `fail`, `warn`, or `skip`.
    pub status: &'static str,
    pub value: Value,
    pub limit: String,
    pub details: String,
    /// A runnable `fx` stage (or command) that fixes a failure.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
}

/// The verification result.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyReport {
    pub pass: bool,
    pub target: Target,
    pub tolerance_lu: f64,
    pub checks: Vec<Check>,
    /// Names of the failed checks.
    pub failed: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl VerifyReport {
    /// One line per failed check, for the error message.
    pub fn failure_lines(&self) -> Vec<String> {
        self.checks.iter().filter(|c| c.status == "fail").map(|c| format!("{}: {}", c.check, c.details)).collect()
    }
}

fn check(check: &'static str, ok: bool, value: Value, limit: String, details: String, fix: &str) -> Check {
    Check { check, status: if ok { "pass" } else { "fail" }, value, limit, details, fix: if ok { String::new() } else { fix.into() } }
}

/// Run every check on `buf`. `info` (from the decoder) enables the format checks.
pub fn verify(buf: &AudioBuffer, info: Option<&SourceInfo>, target: &Target, tolerance_lu: f64) -> VerifyReport {
    let mut checks = Vec::new();
    let l = loudness::loudness(buf);

    // integrated loudness
    let li = l.integrated_lufs;
    let ok = li.is_finite() && (li - target.lufs).abs() <= tolerance_lu + 1e-9;
    checks.push(check(
        "loudness",
        ok,
        jnum(li, 2),
        format!("{} LUFS ± {} LU", target.lufs, tolerance_lu),
        if li.is_finite() {
            format!("integrated loudness {li:.2} LUFS is {:+.2} LU from the {} LUFS target", li - target.lufs, target.lufs)
        } else {
            "integrated loudness cannot be measured (silent or shorter than 400 ms)".into()
        },
        &format!("normalize:lufs={}LUFS", target.lufs),
    ));

    // true peak
    let tp = l.true_peak_dbtp;
    let ok = tp <= target.true_peak_dbtp + 0.005;
    checks.push(check(
        "true_peak",
        ok,
        jnum(tp, 2),
        format!("<= {} dBTP (4x oversampled)", target.true_peak_dbtp),
        format!("true peak {tp:.2} dBTP {} the {} dBTP ceiling", if ok { "is within" } else { "exceeds" }, target.true_peak_dbtp),
        &format!("limit:ceiling={}TP,oversample=4x", fmt_db(target.true_peak_dbtp)),
    ));

    // clipping
    let clip = detect_clipping(buf, 0.0, 3);
    checks.push(check(
        "clipping",
        clip.count == 0,
        json!(clip.count),
        "no clipped runs".into(),
        if clip.count == 0 {
            "no clipped runs".into()
        } else {
            format!(
                "{} clipped run(s), {} samples, first at {:.2}s",
                clip.count,
                clip.clipped_samples,
                clip.runs.first().map(|r| r.start_s).unwrap_or(0.0)
            )
        },
        "declip",
    ));

    // DC offset
    let dc = ferraus_analysis::problems::dc_offsets(buf).into_iter().fold(0.0f64, |m, v| m.max(v.abs()));
    checks.push(check(
        "dc_offset",
        dc <= MAX_DC,
        jnum(dc, 5),
        format!("<= {MAX_DC}"),
        format!("largest channel mean {dc:.5}"),
        "dc-remove",
    ));

    // stereo checks
    if buf.channels() >= 2 {
        let lt = ltas(buf, 8192.min(buf.frames().next_power_of_two()).max(1024));
        let low_share = band_power_fraction(&lt, 20.0, 120.0);
        let lc = stereo::low_correlation(buf, 120.0);
        let mut c = check(
            "out_of_phase_bass",
            !(low_share > 0.01 && lc < 0.5),
            jnum(lc, 3),
            "L/R correlation below 120 Hz >= 0.5".into(),
            format!("L/R correlation below 120 Hz is {lc:.2} ({:.0}% of the energy is below 120 Hz)", low_share * 100.0),
            "widen:amount=100%,mono-below=120Hz",
        );
        if c.status == "pass" && low_share > 0.01 && lc < 0.8 {
            c.status = "warn";
        }
        checks.push(c);
        let corr = stereo::stereo_analysis(buf).correlation;
        let mut c = check(
            "correlation",
            corr >= 0.0,
            jnum(corr, 3),
            "broadband L/R correlation >= 0 (0.3 recommended)".into(),
            format!("broadband L/R correlation {corr:.2}"),
            "widen:amount=80%",
        );
        if c.status == "pass" && corr < 0.3 {
            c.status = "warn";
            c.details = format!("{}; below the 0.3 that master keeps", c.details);
        }
        checks.push(c);
    } else {
        for name in ["out_of_phase_bass", "correlation"] {
            checks.push(Check {
                check: name,
                status: "skip",
                value: Value::Null,
                limit: String::new(),
                details: "mono file".into(),
                fix: String::new(),
            });
        }
    }

    // head silence
    let sil = detect_silence(buf, -60.0, 0.1);
    checks.push(check(
        "head_silence",
        sil.head_silence_s <= MAX_HEAD_SILENCE_S,
        jnum(sil.head_silence_s, 3),
        format!("<= {MAX_HEAD_SILENCE_S} s below -60 dBFS"),
        format!("{:.2} s of silence before the first sound", sil.head_silence_s),
        "silence trim:edges,threshold=-60dB",
    ));

    // delivery format
    if target.sample_rate.is_some() || target.bit_depth.is_some() {
        let sr_ok = target.sample_rate.map(|sr| buf.sample_rate == sr).unwrap_or(true);
        let bd = info.and_then(|i| i.bit_depth);
        let bd_ok = match (target.bit_depth, bd) {
            (Some(want), Some(have)) => want == have,
            _ => true,
        };
        checks.push(check(
            "format",
            sr_ok && bd_ok,
            json!({"sample_rate": buf.sample_rate, "bit_depth": bd}),
            format!(
                "{}{}",
                target.sample_rate.map(|s| format!("{s} Hz")).unwrap_or_default(),
                target.bit_depth.map(|b| format!(", {b}-bit")).unwrap_or_default()
            ),
            format!("{} Hz, {}", buf.sample_rate, bd.map(|b| format!("{b}-bit")).unwrap_or_else(|| "unknown bit depth".into())),
            "ferraus master deliver master.wav --formats wav16",
        ));
    }

    let failed: Vec<String> = checks.iter().filter(|c| c.status == "fail").map(|c| c.check.to_string()).collect();
    let warnings: Vec<String> = checks.iter().filter(|c| c.status == "warn").map(|c| format!("{}: {}", c.check, c.details)).collect();
    VerifyReport { pass: failed.is_empty(), target: target.clone(), tolerance_lu: round_to(tolerance_lu, 3), checks, failed, warnings }
}

/// Parse `0.5LU`, `0.5`, `1lu`.
pub fn parse_tolerance(s: &str) -> Result<f64> {
    let v = units::parse_with_unit(s, &["LU", "lu", "dB", "db"])
        .map_err(|e| Error::usage(format!("--tolerance: {}", e.message)))?;
    if !(0.0..=10.0).contains(&v) {
        return Err(Error::usage("--tolerance must be between 0LU and 10LU"));
    }
    Ok(v)
}
