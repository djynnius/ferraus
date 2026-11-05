//! `mix verify`: pass or fail for a mix that is about to be mastered.
//!
//! `master verify` checks a finished master against a delivery spec. This is the gate one
//! step earlier: does the mix leave the mastering stage room to work (true-peak headroom, a
//! sensible level, peaks that were not limited away), is it technically clean (no clipping,
//! DC, broken phase, or stereo bass), and is its tonal balance in the neighbourhood of the
//! genre curve or a reference? Every failed check carries a runnable `fx` stage that fixes
//! it when one exists, and [`MixReport::fixes`] collects them in processing order.
//!
//! The thresholds are meant for real mixes: a decent unmastered pop, rock, or hip-hop mix
//! passes; a mix that is already brickwall-limited, clipped, phase-broken, or tonally far off
//! fails. The tonal checks compare against the hand-designed genre curves (see the
//! provenance note in [`crate::genres`]), so they are deliberately lenient: an octave-wide
//! view and a 6 dB default tolerance.

use crate::analyze::{fix_to_stage, tone_against};
use crate::chain::resolve_genre;
use crate::genres;
use crate::util::{fmt_db, fmt_freq, fmt_pct, jnum, nominal, num, round_to};
use crate::verify::{Check, MAX_HEAD_SILENCE_S};
use ferraus_analysis::levels::{detect_clipping, detect_silence};
use ferraus_analysis::problems::{dc_offsets, Problem, Severity};
use ferraus_analysis::spectrum::ToneBalance;
use ferraus_analysis::{stereo, MasteringAnalysis};
use ferraus_core::prelude::*;
use serde::Serialize;
use std::path::Path;

/// DC offset above which the check warns (linear, about -60 dBFS: `master verify`'s limit).
pub const WARN_DC: f64 = 0.001;
/// DC offset above which the check fails (about -46 dBFS, where `analyze problems` calls it
/// medium). The master chain removes DC as its first stage, so a small offset is a warning.
pub const MAX_DC: f64 = 0.005;
/// Broadband L/R correlation below which the phase check warns (it fails below 0).
pub const WARN_CORRELATION: f64 = 0.2;
/// Correlation below which the phase fix flips the right channel's polarity instead of
/// narrowing (the `analyze problems` threshold for "largely out of phase").
pub const POLARITY_CORRELATION: f64 = -0.3;
/// L/R correlation below `--mono-below` that the mono_bass check needs.
pub const MIN_BASS_CORRELATION: f64 = 0.85;
/// Share of the energy below `--mono-below` under which the mono_bass check is skipped.
pub const MIN_BASS_SHARE: f64 = 0.01;
/// Largest level loss when the mix is folded to mono before mono_loss warns (dB).
pub const MAX_MONO_LOSS_DB: f64 = 3.0;
/// How far the mud (200-400 Hz), harsh (2-5 kHz), and sub_rumble (below 40 Hz) regions may
/// sit above the curve before they warn (dB, mean over the region's 1/3-octave bands).
pub const REGION_EXCESS_DB: f64 = 4.0;
/// The plr check warns this far above `--min-plr` (10 + 2 = 12 dB by default).
///
/// Calibration (a rough mix of a ten-stem pop arrangement, then gain into a -1 dBTP
/// true-peak limiter): unlimited 14.6 dB, about 3 dB of gain reduction 12.2 dB, about 5 dB
/// 10.6 dB, about 8 dB 8.6 dB, about 11 dB 7.7 dB. So `--min-plr 10dB` fails a mix that was
/// limited to master level (a limited mix that is merely turned down keeps its PLR, so the
/// loudness check alone cannot catch it), and 12 dB flags moderate bus limiting.
pub const PLR_WARN_MARGIN_DB: f64 = 2.0;
/// A tonal deviation this large (dB) is a missing or mis-set part, not an EQ problem.
pub const MISSING_PART_DB: f64 = 12.0;
/// Below 100 Hz a deficit only fails where the target is within this many dB of its
/// strongest band (see `smoothed_deltas`).
pub const LOW_END_TARGET_DB: f64 = 6.0;
/// The tonal-balance fix moves this share of the way back to the curve, at most
/// [`MAX_EQ_FIX_DB`] per band.
const EQ_FIX_SHARE: f64 = 0.5;
const MAX_EQ_FIX_DB: f64 = 6.0;
/// Side-to-mid energy ratio that gives a broadband correlation of 0.3 (what `master` keeps):
/// the narrowing fix scales the side signal down to it.
const TARGET_SIDE_RATIO: f64 = 0.7 / 1.3;
/// Problem codes that a dedicated check already covers (so one fault is not failed twice).
const COVERED_PROBLEMS: &[&str] =
    &["clipping", "dc_offset", "inter_sample_peaks", "phase", "out_of_phase_bass", "silence_head"];
/// Problem codes that are not mix faults: sections far apart in loudness are arrangement,
/// and levelling them is a mastering or broadcast decision.
const IGNORED_PROBLEMS: &[&str] = &["loudness_outliers"];

/// The limits a mix is checked against (the command's parameters).
#[derive(Clone, Debug, Serialize)]
pub struct MixLimits {
    /// Highest acceptable true peak (dBTP, 4x oversampled).
    pub headroom_dbtp: f64,
    pub min_lufs: f64,
    /// Louder than this, the mix has been limited already.
    pub max_lufs: f64,
    /// Lowest acceptable peak-to-loudness ratio (dB).
    pub min_plr_db: f64,
    /// Largest allowed octave-smoothed 1/3-octave deviation from the curve (dB).
    pub tolerance_db: f64,
    /// Frequency below which the mix should be mono; 0 turns the mono_bass check off.
    pub mono_below_hz: f64,
}

impl Default for MixLimits {
    fn default() -> Self {
        MixLimits {
            headroom_dbtp: -3.0,
            min_lufs: -26.0,
            max_lufs: -12.0,
            min_plr_db: 10.0,
            tolerance_db: 6.0,
            mono_below_hz: 120.0,
        }
    }
}

/// What to verify.
pub struct MixInput<'a> {
    pub buf: &'a AudioBuffer,
    /// [`crate::analyze::measure`] of `buf` (with the reference, when one is given).
    pub analysis: &'a MasteringAnalysis,
    /// `auto` or a genre name.
    pub genre: &'a str,
    /// Path of the reference track, when the analysis was made with one.
    pub reference: Option<&'a str>,
    /// Path of the mix (used in the fix command).
    pub path: &'a str,
}

/// The verification result.
#[derive(Clone, Debug, Serialize)]
pub struct MixReport {
    pub pass: bool,
    pub genre: String,
    /// `explicit` or `auto`.
    pub genre_source: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub genre_reasons: Vec<String>,
    /// What the tonal checks compare with: `genre curve: pop` or `reference: ref.wav`.
    pub tonal_reference: String,
    pub limits: MixLimits,
    pub checks: Vec<Check>,
    /// Names of the failed checks.
    pub failed: Vec<String>,
    /// `check: details` for every check that warns.
    pub warnings: Vec<String>,
    /// The `fx` stages that fix the failed checks, de-duplicated, in processing order.
    pub fixes: Vec<String>,
    /// `ferraus fx <mix> -o <mix>.fixed.wav --fx ...` with [`MixReport::fixes`], when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_command: Option<String>,
    pub summary: String,
}

impl MixReport {
    /// One line per failed check, for the error message.
    pub fn failure_lines(&self) -> Vec<String> {
        self.checks.iter().filter(|c| c.status == "fail").map(|c| format!("{}: {}", c.check, c.details)).collect()
    }
}

/// Checks and the fx stages behind each one's `fix`.
struct Checks<'p> {
    path: &'p str,
    list: Vec<Check>,
    stages: Vec<Vec<String>>,
}

impl Checks<'_> {
    fn push(&mut self, check: &'static str, status: &'static str, value: Value, limit: String, details: String) {
        self.push_fix(check, status, value, limit, details, Vec::new());
    }

    /// Add a check; `stages` (fx stage strings) become its `fix` when it fails or warns.
    fn push_fix(
        &mut self,
        check: &'static str,
        status: &'static str,
        value: Value,
        limit: String,
        details: String,
        mut stages: Vec<String>,
    ) {
        stages.retain(|s| !s.is_empty());
        if status != "fail" && status != "warn" {
            stages.clear();
        }
        let fix = match stages.len() {
            0 => String::new(),
            1 => stages[0].clone(),
            _ => fx_command(self.path, &stages),
        };
        self.list.push(Check { check, status, value, limit, details, fix });
        self.stages.push(stages);
    }

    fn skip(&mut self, check: &'static str, details: &str) {
        self.push(check, "skip", Value::Null, String::new(), details.into());
    }
}

/// `pass`, `warn`, or `fail`.
fn grade(fail: bool, warn: bool) -> &'static str {
    if fail {
        "fail"
    } else if warn {
        "warn"
    } else {
        "pass"
    }
}

/// Quote a path for a shell command line when it needs it.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-/+=:,@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `<dir>/<stem>.fixed.wav` next to the mix. Only the file name is replaced, so the directory
/// keeps the separators the user typed (a hint must be runnable as written on every OS).
fn fixed_path(path: &str) -> String {
    let p = Path::new(path);
    match (p.file_name(), p.file_stem()) {
        (Some(name), Some(stem)) if path != "-" => {
            let dir = path.strip_suffix(&*name.to_string_lossy()).unwrap_or("");
            format!("{dir}{}.fixed.wav", stem.to_string_lossy())
        }
        _ => "mix.fixed.wav".into(),
    }
}

/// `ferraus fx <mix> -o <mix>.fixed.wav --fx "<stage>" ...`
pub fn fx_command(path: &str, stages: &[String]) -> String {
    let fx: Vec<String> = stages.iter().map(|s| format!("--fx \"{s}\"")).collect();
    format!("ferraus fx {} -o {} {}", shell_quote(path), shell_quote(&fixed_path(path)), fx.join(" "))
}

/// Processing order of a fix stage: repairs first, then filters and EQ, stereo, and the
/// level last (so the gain sees the final peaks).
fn stage_rank(stage: &str) -> u8 {
    match stage.split(':').next().unwrap_or("").trim() {
        "dc-remove" => 0,
        "declip" => 1,
        "declick" | "dehum" | "dehiss" | "denoise" | "channels mix" | "phase-align" => 2,
        "highpass" => 3,
        "eq" => 4,
        "widen" => 5,
        "gain" => 9,
        _ => 6,
    }
}

/// Round to 0.5 dB steps.
fn half_db(x: f64) -> f64 {
    round_to((x * 2.0).round() / 2.0, 1)
}

/// One gain that puts the true peak at or under the headroom and the loudness inside the
/// window, aiming 0.5 dB under the ceiling and 1 LU inside the window when there is room
/// (0.5 dB steps when they fit). `None` when the level cannot be measured or no gain fits
/// both (the mix is too peaky for its loudness).
pub fn level_gain(lufs: f64, true_peak: f64, lim: &MixLimits) -> Option<f64> {
    if !lufs.is_finite() || !true_peak.is_finite() {
        return None;
    }
    let lo = lim.min_lufs - lufs;
    let hi = (lim.max_lufs - lufs).min(lim.headroom_dbtp - true_peak);
    if lo > hi + 1e-9 {
        return None;
    }
    let (tlo, thi) = (lo + 1.0, (lim.max_lufs - lufs - 1.0).min(lim.headroom_dbtp - true_peak - 0.5));
    let g = if tlo <= thi { 0.0f64.clamp(tlo, thi) } else { (lo + hi) / 2.0 };
    let stepped = if g < 0.0 { (g * 2.0).floor() / 2.0 } else { (g * 2.0).ceil() / 2.0 };
    let g = if stepped >= lo - 1e-9 && stepped <= hi + 1e-9 { stepped } else { g };
    Some(round_to(g, 2))
}

/// Narrowing fix: scale the side signal so the broadband correlation comes back to about 0.3.
fn narrow_stage(side_to_mid_db: f64) -> String {
    let amount = if side_to_mid_db.is_finite() {
        let r = 10f64.powf(side_to_mid_db / 10.0);
        ((TARGET_SIDE_RATIO / r).sqrt() * 20.0).floor() / 20.0
    } else if side_to_mid_db > 0.0 {
        0.0
    } else {
        1.0
    };
    format!("widen:amount={}", fmt_pct(amount.clamp(0.0, 0.95)))
}

/// Per-band deltas over 60 Hz-12.5 kHz (the range `tone_balance` removes the offset over),
/// each averaged with its in-range neighbours: an octave-wide view, so one 1/3-octave dip
/// or a peak on the song's bass notes does not count as a tonal-balance problem. Deltas are
/// capped at ±30 dB (beyond that the band is simply missing).
///
/// A deficit below 100 Hz only counts where the target has real low end (within
/// [`LOW_END_TARGET_DB`] of its strongest band): a voice high-passed at 80 Hz is not "too
/// weak at 63 Hz" against the podcast curve, which is already 15 dB down there.
fn smoothed_deltas(t: &ToneBalance) -> Vec<(f64, f64)> {
    let pts: Vec<(f64, f64, f64)> = t
        .bands
        .iter()
        .filter(|b| b.center_hz >= 60.0 && b.center_hz <= 12500.0 && b.reference_db > -140.0)
        .map(|b| (b.center_hz, b.delta_db.clamp(-30.0, 30.0), b.reference_db))
        .collect();
    let ref_max = pts.iter().map(|p| p.2).fold(f64::NEG_INFINITY, f64::max);
    (0..pts.len())
        .map(|i| {
            let lo = i.saturating_sub(1);
            let hi = (i + 1).min(pts.len() - 1);
            let s = pts[lo..=hi].iter().map(|p| p.1).sum::<f64>() / (hi - lo + 1) as f64;
            let (c, _, r) = pts[i];
            let sparse_low = c < 100.0 && r < ref_max - LOW_END_TARGET_DB;
            (c, if sparse_low { s.max(0.0) } else { s })
        })
        .collect()
}

/// Out-of-tolerance regions: runs of same-sign smoothed deltas beyond `tol`, each reduced to
/// its worst band, largest first.
fn tonal_regions(sm: &[(f64, f64)], tol: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut current: Option<(f64, f64)> = None;
    for &(f, d) in sm {
        if d.abs() > tol {
            current = match current {
                Some((cf, cd)) if cd.signum() == d.signum() => Some(if d.abs() > cd.abs() { (f, d) } else { (cf, cd) }),
                Some(c) => {
                    out.push(c);
                    Some((f, d))
                }
                None => Some((f, d)),
            };
        } else if let Some(c) = current.take() {
            out.push(c);
        }
    }
    out.extend(current);
    out.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// `band=peak:<f>:<gain>:q=1.0` moving a band part of the way back to the curve.
fn eq_band(center: f64, delta: f64) -> String {
    let g = half_db(-delta * EQ_FIX_SHARE).clamp(-MAX_EQ_FIX_DB, MAX_EQ_FIX_DB);
    format!("band=peak:{}:{}:q=1.0", fmt_freq(nominal(center)), fmt_db(g))
}

/// Mean delta of the measurable bands with centres in `lo..=hi`, and the band that sits
/// highest.
fn region_excess(t: &ToneBalance, lo: f64, hi: f64) -> Option<(f64, f64, f64)> {
    let bands: Vec<_> = t
        .bands
        .iter()
        .filter(|b| b.center_hz >= lo && b.center_hz <= hi && b.level_db > -140.0 && b.reference_db > -140.0)
        .collect();
    if bands.is_empty() {
        return None;
    }
    let mean = bands.iter().map(|b| b.delta_db).sum::<f64>() / bands.len() as f64;
    let top = bands.iter().max_by(|a, b| a.delta_db.partial_cmp(&b.delta_db).unwrap_or(std::cmp::Ordering::Equal))?;
    Some((mean, top.center_hz, top.delta_db))
}

fn problem_json(p: &Problem) -> Value {
    json!({
        "problem": p.problem,
        "severity": p.severity,
        "details": p.details,
        "suggested_fix": fix_to_stage(None, &p.suggested_fix).0,
    })
}

/// Run every check.
pub fn verify_mix(inp: &MixInput, lim: &MixLimits) -> MixReport {
    let (buf, m) = (inp.buf, inp.analysis);
    let l = &m.loudness;
    let mut cs = Checks { path: inp.path, list: Vec::new(), stages: Vec::new() };

    let genre = resolve_genre(inp.genre, m);
    let (label, tone) = tone_against(m, &genre.name, inp.reference.is_some());
    let label = match inp.reference {
        Some(p) if label == "reference" => format!("reference: {p}"),
        _ => label,
    };

    // ---- level: one gain serves both the true peak and the loudness window
    let (li, tp) = (l.integrated_lufs, l.true_peak_dbtp);
    let gain = level_gain(li, tp, lim);
    let gain_stage = |g: f64| if g.abs() >= 0.05 { format!("gain:by={}", fmt_db(g)) } else { String::new() };
    let tp_fail = tp > lim.headroom_dbtp + 0.005;
    let lufs_fail = !li.is_finite() || li > lim.max_lufs + 0.005 || li < lim.min_lufs - 0.005;
    let both = tp_fail && lufs_fail && gain.is_some();
    let once = if both { "; one gain fixes both true_peak and integrated_lufs: apply it once" } else { "" };

    let details = if !tp.is_finite() {
        "no peaks to measure (silent)".to_string()
    } else if tp_fail {
        format!(
            "true peak {tp:.2} dBTP is above the {} dBTP mastering headroom{}",
            num(lim.headroom_dbtp),
            if gain.is_none() && li.is_finite() {
                format!(
                    "; no single gain also keeps the loudness at or above {} LUFS (the mix is too peaky for its level: tame the transients with compress before raising it)",
                    num(lim.min_lufs)
                )
            } else {
                once.to_string()
            }
        )
    } else {
        format!("true peak {tp:.2} dBTP leaves {:.1} dB of headroom", -tp)
    };
    let tp_gain = gain.or_else(|| tp.is_finite().then(|| ((lim.headroom_dbtp - tp - 0.1) * 2.0).floor() / 2.0));
    cs.push_fix(
        "true_peak",
        grade(tp_fail, false),
        jnum(tp, 2),
        format!("<= {} dBTP (4x oversampled)", num(lim.headroom_dbtp)),
        details,
        tp_gain.map(gain_stage).into_iter().collect(),
    );

    let window = format!("{} to {} LUFS", num(lim.min_lufs), num(lim.max_lufs));
    let details = if !li.is_finite() {
        "integrated loudness cannot be measured (silent or shorter than 400 ms)".to_string()
    } else if li > lim.max_lufs + 0.005 {
        format!(
            "{li:.1} LUFS is louder than {} LUFS: that is master level, so the mix has probably been limited or pushed into a bus clipper (turning it down fixes this check; plr says whether the peaks survived){once}",
            num(lim.max_lufs)
        )
    } else if li < lim.min_lufs - 0.005 {
        if gain.is_some() {
            format!("{li:.1} LUFS is quieter than {} LUFS: the mix was printed too low{once}", num(lim.min_lufs))
        } else {
            format!(
                "{li:.1} LUFS is quieter than {} LUFS, and raising it would push the true peak over {} dBTP: tame the transients (compress) first",
                num(lim.min_lufs),
                num(lim.headroom_dbtp)
            )
        }
    } else {
        format!("{li:.1} LUFS is inside {window}")
    };
    cs.push_fix(
        "integrated_lufs",
        grade(lufs_fail, false),
        jnum(li, 2),
        window,
        details,
        gain.map(gain_stage).into_iter().collect(),
    );

    // ---- peak-to-loudness ratio: has the mix been limited already?
    let plr = l.plr_db;
    if plr.is_finite() {
        let warn_below = lim.min_plr_db + PLR_WARN_MARGIN_DB;
        let status = grade(plr < lim.min_plr_db - 1e-9, plr < warn_below - 1e-9);
        let details = match status {
            "fail" => format!(
                "peak-to-loudness ratio {plr:.1} dB is below {} dB: the mix is already limited or squashed; re-bounce it without the bus limiter or clipper (no fx stage restores crushed peaks)",
                num(lim.min_plr_db)
            ),
            "warn" => format!(
                "peak-to-loudness ratio {plr:.1} dB is dense for an unmastered mix (below {} dB): check the mix bus for a limiter or heavy compression",
                num(warn_below)
            ),
            _ => format!("peak-to-loudness ratio {plr:.1} dB: the peaks are intact"),
        };
        cs.push(
            "plr",
            status,
            jnum(plr, 2),
            format!(">= {} dB (true peak minus integrated loudness)", num(lim.min_plr_db)),
            details,
        );
    } else {
        cs.skip("plr", "cannot be measured (silent or shorter than 400 ms)");
    }

    // ---- clipping (runs above full scale in a float file are not clipped yet: gain fixes them)
    let clip = detect_clipping(buf, 0.0, 3);
    let listed_overs = clip.runs.iter().filter(|r| r.kind == "threshold" && r.level_db > 0.01).count();
    let overs = if clip.runs.is_empty() {
        0
    } else {
        ((listed_overs as f64) * clip.count as f64 / clip.runs.len() as f64).round() as usize
    };
    let clipped = clip.count.saturating_sub(overs);
    let mut details = if clipped == 0 {
        "no clipped runs".to_string()
    } else {
        let first = clip.runs.iter().find(|r| !(r.kind == "threshold" && r.level_db > 0.01)).map(|r| r.start_s);
        format!("{clipped} clipped run(s) (flat tops at or below full scale), first at {:.2}s", first.unwrap_or(0.0))
    };
    if overs > 0 {
        details.push_str(&format!(
            "; {overs} run(s) above 0 dBFS in floating point are not clipped yet (the true_peak gain brings them under)"
        ));
    }
    cs.push_fix(
        "clipping",
        grade(clipped > 0, false),
        json!(clipped),
        "no clipped runs".into(),
        details,
        vec!["declip".into()],
    );

    // ---- DC offset
    let dc = dc_offsets(buf).into_iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    let status = grade(dc > MAX_DC, dc > WARN_DC);
    cs.push_fix(
        "dc_offset",
        status,
        jnum(dc, 5),
        format!("<= {MAX_DC} (warns above {WARN_DC})"),
        match status {
            "fail" => format!("largest channel mean {dc:.5}: the offset eats headroom and skews the limiter"),
            "warn" => format!("largest channel mean {dc:.5}: small (master removes DC first), but dc-remove is free"),
            _ => format!("largest channel mean {dc:.5}"),
        },
        vec!["dc-remove".into()],
    );

    // ---- stereo
    let st = &m.stereo;
    if buf.channels() >= 2 {
        let corr = st.correlation;
        let fix = if corr < POLARITY_CORRELATION {
            "channels mix:invert=R".to_string()
        } else {
            narrow_stage(st.side_to_mid_db)
        };
        cs.push_fix(
            "phase",
            grade(corr < 0.0, corr < WARN_CORRELATION),
            jnum(corr, 3),
            format!(">= 0 broadband L/R correlation (warns below {WARN_CORRELATION})"),
            if corr < POLARITY_CORRELATION {
                format!("L/R correlation {corr:.2}: left and right are largely out of phase (one channel's polarity is probably flipped); the mono sum cancels")
            } else if corr < 0.0 {
                format!("L/R correlation {corr:.2}: more out of phase than in phase; the mono sum thins out")
            } else {
                format!("L/R correlation {corr:.2}")
            },
            vec![fix],
        );

        let mb = lim.mono_below_hz;
        if mb <= 0.0 {
            cs.skip("mono_bass", "turned off (--mono-below 0Hz)");
        } else {
            let share = genres::share(&m.third_octave, 20.0, mb);
            if share < MIN_BASS_SHARE {
                cs.skip(
                    "mono_bass",
                    &format!("only {:.1}% of the energy is below {}; nothing to check", share * 100.0, fmt_freq(mb)),
                );
            } else {
                let lc = stereo::low_correlation(buf, mb);
                cs.push_fix(
                    "mono_bass",
                    grade(lc < MIN_BASS_CORRELATION, false),
                    jnum(lc, 3),
                    format!("L/R correlation below {} >= {MIN_BASS_CORRELATION}", fmt_freq(mb)),
                    format!(
                        "L/R correlation below {} is {lc:.2} ({:.0}% of the energy is down there){}",
                        fmt_freq(mb),
                        share * 100.0,
                        if lc < MIN_BASS_CORRELATION {
                            ": stereo or out-of-phase bass wastes headroom and thins out in mono"
                        } else {
                            ""
                        }
                    ),
                    vec![format!("widen:mono-below={},amount=100%", fmt_freq(mb))],
                );
            }
        }

        let loss = (-st.mono_loss_db).max(0.0);
        cs.push_fix(
            "mono_loss",
            grade(false, loss > MAX_MONO_LOSS_DB),
            jnum(loss, 2),
            format!("<= {MAX_MONO_LOSS_DB} dB level drop when folded to mono (warns)"),
            format!("folding to mono loses {loss:.1} dB"),
            vec![narrow_stage(st.side_to_mid_db)],
        );
    } else {
        for name in ["phase", "mono_bass", "mono_loss"] {
            cs.skip(name, "mono file");
        }
    }

    // ---- tonal balance against the genre curve or the reference
    let measurable = tone.bands.iter().any(|b| b.level_db > -140.0);
    let sm = if measurable { smoothed_deltas(&tone) } else { Vec::new() };
    if sm.len() >= 3 {
        let worst = sm
            .iter()
            .copied()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((1000.0, 0.0));
        let regions = tonal_regions(&sm, lim.tolerance_db);
        let fail = !regions.is_empty();
        let dir = |d: f64| if d > 0.0 { "too strong" } else { "too weak" };
        let mut details = format!(
            "{} is {:.1} dB {} vs {label} (octave-smoothed, overall level removed; RMS deviation {:.1} dB)",
            fmt_freq(nominal(worst.0)),
            worst.1.abs(),
            dir(worst.1),
            tone.rms_deviation_db
        );
        if regions.len() > 1 {
            let others: Vec<String> =
                regions[1..].iter().map(|(f, d)| format!("{} {:+.1} dB", fmt_freq(nominal(*f)), d)).collect();
            details.push_str(&format!("; also out of tolerance: {}", others.join(", ")));
        }
        if fail && worst.1.abs() > MISSING_PART_DB {
            details.push_str(
                "; a gap this large usually means a missing, muted, or badly balanced part rather than an EQ problem",
            );
        }
        let stage = if fail {
            let bands: Vec<String> = regions.iter().take(3).map(|(f, d)| eq_band(*f, *d)).collect();
            format!("eq:{}", bands.join(","))
        } else {
            String::new()
        };
        cs.push_fix(
            "tonal_balance",
            grade(fail, false),
            jnum(worst.1, 1),
            format!(
                "|deviation| <= {} dB (1/3-octave bands averaged over an octave, 60 Hz-12.5 kHz, vs {label})",
                num(lim.tolerance_db)
            ),
            details,
            vec![stage],
        );
    } else {
        cs.skip("tonal_balance", "no measurable spectrum (silent)");
    }

    // ---- regions that commonly go wrong in a mix (warnings)
    let regions: [(&'static str, f64, f64, &str); 3] = [
        ("mud", 200.0, 400.0, "200-400 Hz"),
        ("harsh", 2000.0, 5000.0, "2-5 kHz"),
        ("sub_rumble", 0.0, 35.0, "below 40 Hz"),
    ];
    for (name, lo, hi, range) in regions {
        match region_excess(&tone, lo, hi) {
            Some((mean, top, _)) if measurable => {
                let warn = mean > REGION_EXCESS_DB;
                let stage = if name == "sub_rumble" {
                    "highpass:freq=30Hz,order=4".to_string()
                } else {
                    let cut = half_db(-mean * EQ_FIX_SHARE).clamp(-MAX_EQ_FIX_DB, -1.0);
                    format!("eq:band=peak:{}:{}:q=1.0", fmt_freq(nominal(top)), fmt_db(cut))
                };
                cs.push_fix(
                    name,
                    grade(false, warn),
                    jnum(mean, 1),
                    format!("<= +{REGION_EXCESS_DB} dB above {label} ({range}, warns)"),
                    format!("{range} sits {mean:+.1} dB relative to {label} (overall level removed)"),
                    vec![stage],
                );
            }
            _ => cs.skip(name, &format!("no measurable energy {range}")),
        }
    }

    // ---- head silence
    let sil = detect_silence(buf, -60.0, 0.1);
    cs.push_fix(
        "head_silence",
        grade(false, sil.head_silence_s > MAX_HEAD_SILENCE_S),
        jnum(sil.head_silence_s, 3),
        format!("<= {MAX_HEAD_SILENCE_S} s below -60 dBFS (warns)"),
        format!("{:.2} s of silence before the first sound", sil.head_silence_s),
        vec!["silence trim:edges,threshold=-60dB".into()],
    );

    // ---- everything else analyze problems finds (high severity fails, medium warns)
    let rest: Vec<&Problem> = m
        .problems
        .problems
        .iter()
        .filter(|p| !COVERED_PROBLEMS.contains(&p.problem.as_str()) && !IGNORED_PROBLEMS.contains(&p.problem.as_str()))
        .collect();
    let high: Vec<&&Problem> = rest.iter().filter(|p| p.severity == Severity::High).collect();
    let medium: Vec<&&Problem> = rest.iter().filter(|p| p.severity == Severity::Medium).collect();
    let shown = if high.is_empty() { &medium } else { &high };
    let mut stages: Vec<String> = Vec::new();
    for p in shown {
        let s = fix_to_stage(None, &p.suggested_fix).0;
        if !s.is_empty() && !stages.contains(&s) {
            stages.push(s);
        }
    }
    let details = if shown.is_empty() {
        format!("no high or medium severity problems ({} low)", rest.len())
    } else {
        let what: Vec<String> = shown.iter().map(|p| format!("{} ({})", p.problem, p.details)).collect();
        format!("{}: {}", if high.is_empty() { "medium severity" } else { "high severity" }, what.join("; "))
    };
    cs.push_fix(
        "problems",
        grade(!high.is_empty(), !medium.is_empty()),
        Value::Array(shown.iter().map(|p| problem_json(p)).collect()),
        "no high-severity problem from analyze problems (medium warns)".into(),
        details,
        stages,
    );

    // ---- verdict
    let failed: Vec<String> = cs.list.iter().filter(|c| c.status == "fail").map(|c| c.check.to_string()).collect();
    let warnings: Vec<String> =
        cs.list.iter().filter(|c| c.status == "warn").map(|c| format!("{}: {}", c.check, c.details)).collect();
    let mut fixes: Vec<String> = Vec::new();
    for (c, stages) in cs.list.iter().zip(&cs.stages) {
        if c.status == "fail" {
            for s in stages {
                if !fixes.contains(s) {
                    fixes.push(s.clone());
                }
            }
        }
    }
    fixes.sort_by_key(|s| stage_rank(s));
    let pass = failed.is_empty();
    let passed = cs.list.iter().filter(|c| c.status == "pass").count();
    let warned: Vec<&str> = cs.list.iter().filter(|c| c.status == "warn").map(|c| c.check).collect();
    let summary = if pass {
        let w =
            if warned.is_empty() { String::new() } else { format!(", {} warn ({})", warned.len(), warned.join(", ")) };
        format!("ready for mastering: {passed} checks pass{w}")
    } else {
        format!(
            "not ready for mastering: {} of {} checks fail ({}){}",
            failed.len(),
            cs.list.iter().filter(|c| c.status != "skip").count(),
            failed.join(", "),
            if fixes.is_empty() { "" } else { "; apply the fixes and verify again" }
        )
    };
    MixReport {
        pass,
        genre: genre.name,
        genre_source: genre.source,
        genre_reasons: genre.reasons,
        tonal_reference: label,
        limits: lim.clone(),
        fix_command: (!fixes.is_empty()).then(|| fx_command(inp.path, &fixes)),
        checks: cs.list,
        failed,
        warnings,
        fixes,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genres::genre;
    use ferraus_analysis::loudness;
    use ferraus_analysis::spectrum::BandDelta;
    use ferraus_analysis::util::{highpass4, lowpass4};
    use ferraus_core::op::parse_stage_string;
    use ferraus_core::testing::{add, leak_registry, noise, sine};
    use ferraus_core::{Args, Registry};

    use std::sync::OnceLock;

    fn registry() -> &'static Registry {
        static REG: OnceLock<&'static Registry> = OnceLock::new();
        REG.get_or_init(|| {
            let mut ops = ferraus_dsp::ops();
            ops.extend(ferraus_edit::ops());
            ops.extend(ferraus_restore::ops());
            leak_registry(ops)
        })
    }

    /// Every fix is an fx stage this build can run with these exact arguments (or a full
    /// `ferraus fx` command of such stages).
    fn assert_fixes_parse(r: &MixReport) {
        let mut stages: Vec<String> = r.fixes.clone();
        for c in &r.checks {
            if let Some(cmd) = c.fix.strip_prefix("ferraus fx ") {
                stages.extend(cmd.split("--fx \"").skip(1).map(|s| s.trim().trim_end_matches('"').to_string()));
            } else if !c.fix.is_empty() {
                stages.push(c.fix.clone());
            }
        }
        for s in stages {
            let (name, pairs) = parse_stage_string(&s).unwrap_or_else(|e| panic!("{s}: {}", e.message));
            let spec = registry().get(&name).unwrap_or_else(|| panic!("no op '{name}' for fix {s}"));
            assert!(spec.is_transform(), "{s} is not an fx stage");
            Args::build(spec, pairs, &|_, _| None).unwrap_or_else(|e| panic!("{s}: {}", e.message));
        }
    }

    fn scale_to_lufs(mut b: AudioBuffer, lufs: f64) -> AudioBuffer {
        let now = loudness::integrated_lufs(&b);
        b.scale(10f64.powf((lufs - now) / 20.0) as f32);
        b
    }

    /// A stand-in for a decent rough mix: four sines per 1/3-octave band from 31.5 Hz to
    /// 16 kHz on the pop curve (kept 5 Hz off the 50/60 Hz harmonics, so analyze problems
    /// does not hear mains hum), random phases, mono below 150 Hz and a moderately wide
    /// image above, pulsing on a 120 BPM beat, at -18 LUFS.
    fn clean_mix() -> AudioBuffer {
        static MIX: OnceLock<AudioBuffer> = OnceLock::new();
        MIX.get_or_init(|| {
            let (sr, secs) = (48000u32, 3.0);
            let n = (sr as f64 * secs) as usize;
            let (mut l, mut r) = (vec![0.0f64; n], vec![0.0f64; n]);
            let mut x = 0x2545F4914F6CDD1Du64;
            let mut rand = move || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 11) as f64 / (1u64 << 53) as f64
            };
            let off_hum_grid = |f: f64| {
                let near = |base: f64| f < 500.0 && (f - (f / base).round() * base).abs() < 5.0;
                if near(50.0) || near(60.0) {
                    f + 7.0
                } else {
                    f
                }
            };
            let curve = genre("pop").unwrap().curve;
            for k in -15..=12 {
                let fc = 1000.0 * 2f64.powf(k as f64 / 3.0);
                let a = (2.0 * 10f64.powf(genres::curve_at(curve, fc) / 10.0) / 4.0).sqrt();
                for j in 0..4 {
                    let f = off_hum_grid(fc * 2f64.powf(((j as f64 + 0.5) / 4.0 - 0.5) / 3.0));
                    let pl = rand() * std::f64::consts::TAU;
                    let pr = if f < 150.0 { pl } else { pl + (rand() - 0.5) * 2.0 };
                    let w = std::f64::consts::TAU * f / sr as f64;
                    for i in 0..n {
                        l[i] += a * (w * i as f64 + pl).sin();
                        r[i] += a * (w * i as f64 + pr).sin();
                    }
                }
            }
            let env = |i: usize| 0.5 + 0.5 * (-((i as f64 / sr as f64) % 0.5) * 8.0).exp();
            let ch = |v: Vec<f64>| v.iter().enumerate().map(|(i, s)| (s * env(i)) as f32).collect();
            scale_to_lufs(AudioBuffer::from_planar(sr, vec![ch(l), ch(r)]), -18.0)
        })
        .clone()
    }

    fn run(buf: &AudioBuffer, g: &str, lim: &MixLimits) -> MixReport {
        let m = crate::analyze::measure(buf, None);
        let r = verify_mix(&MixInput { buf, analysis: &m, genre: g, reference: None, path: "mix.wav" }, lim);
        assert_fixes_parse(&r);
        r
    }

    fn check<'a>(r: &'a MixReport, name: &str) -> &'a Check {
        r.checks.iter().find(|c| c.check == name).unwrap_or_else(|| panic!("no check {name}"))
    }

    fn status(r: &MixReport, name: &str) -> &'static str {
        check(r, name).status
    }

    #[test]
    fn clean_mix_passes() {
        let r = run(&clean_mix(), "pop", &MixLimits::default());
        let v = serde_json::to_value(&r).unwrap();
        assert!(r.pass, "{}", serde_json::to_string_pretty(&v).unwrap());
        for c in ["true_peak", "integrated_lufs", "plr", "clipping", "dc_offset", "phase", "mono_bass", "tonal_balance"]
        {
            assert_eq!(status(&r, c), "pass", "{c}: {}", check(&r, c).details);
        }
        assert!(r.fixes.is_empty() && r.fix_command.is_none());
        assert!(r.summary.starts_with("ready for mastering"), "{}", r.summary);
        assert_eq!((r.genre.as_str(), r.genre_source.as_str()), ("pop", "explicit"));
        assert_eq!(r.tonal_reference, "genre curve: pop");
        assert_eq!(r.checks.len(), 14);
        assert!(check(&r, "tonal_balance").value.as_f64().unwrap().abs() < 3.0);
    }

    #[test]
    fn limited_mix_fails_loudness_and_plr_with_one_gain_fix() {
        // push the clean mix 14 dB into a saturating clipper: master level, crushed peaks
        let mut b = clean_mix();
        b.scale(10f32.powf(14.0 / 20.0));
        b.data.iter_mut().flatten().for_each(|v| *v = 0.95 * v.tanh());
        let r = run(&b, "pop", &MixLimits::default());
        assert!(!r.pass);
        for c in ["integrated_lufs", "plr", "true_peak"] {
            assert_eq!(status(&r, c), "fail", "{c}: {}", check(&r, c).details);
        }
        assert!(check(&r, "plr").details.contains("already limited"), "{}", check(&r, "plr").details);
        assert!(check(&r, "plr").fix.is_empty(), "no fx stage restores crushed peaks");
        let gain = &check(&r, "integrated_lufs").fix;
        assert!(gain.starts_with("gain:by=-"), "{gain}");
        assert_eq!(gain, &check(&r, "true_peak").fix, "one gain serves both level checks");
        assert_eq!(r.fixes.iter().filter(|f| f.starts_with("gain:")).count(), 1, "{:?}", r.fixes);
        // that gain puts both inside their limits
        let g = units::parse_db(gain.trim_start_matches("gain:by=")).unwrap();
        let l = loudness::loudness(&b);
        assert!(l.integrated_lufs + g <= -12.0 && l.integrated_lufs + g >= -26.0);
        assert!(l.true_peak_dbtp + g <= -3.0);
        assert!(r.summary.starts_with("not ready"), "{}", r.summary);
    }

    #[test]
    fn plr_warns_up_to_two_db_above_the_limit() {
        let b = clean_mix();
        let plr = loudness::loudness(&b).plr_db;
        let lim = MixLimits { min_plr_db: plr - 1.0, ..MixLimits::default() };
        assert_eq!(status(&run(&b, "pop", &lim), "plr"), "warn");
        let lim = MixLimits { min_plr_db: plr + 0.5, ..MixLimits::default() };
        assert_eq!(status(&run(&b, "pop", &lim), "plr"), "fail");
    }

    #[test]
    fn out_of_phase_bass_fails_mono_bass() {
        let mut b = clean_mix();
        let low = lowpass4(&b.data[1], b.sample_rate as f64, 150.0);
        for (v, lo) in b.data[1].iter_mut().zip(&low) {
            *v -= 2.0 * lo;
        }
        let r = run(&b, "pop", &MixLimits::default());
        let c = check(&r, "mono_bass");
        assert_eq!(c.status, "fail", "{}", c.details);
        assert!(c.value.as_f64().unwrap() < 0.5, "{}", c.value);
        assert_eq!(c.fix, "widen:mono-below=120Hz,amount=100%");
        assert!(r.fixes.contains(&c.fix));
        // --mono-below follows into the fix; 0Hz turns the check off
        let r = run(&b, "pop", &MixLimits { mono_below_hz: 150.0, ..MixLimits::default() });
        assert_eq!(check(&r, "mono_bass").fix, "widen:mono-below=150Hz,amount=100%");
        let r = run(&b, "pop", &MixLimits { mono_below_hz: 0.0, ..MixLimits::default() });
        assert_eq!(status(&r, "mono_bass"), "skip");
    }

    #[test]
    fn clipped_mix_fails_clipping_and_declip_runs_before_the_gain() {
        let mut b = clean_mix();
        let peak = b.peak();
        b.scale(2.0 / peak);
        b.data.iter_mut().flatten().for_each(|v| *v = v.clamp(-1.0, 1.0));
        let r = run(&b, "pop", &MixLimits::default());
        let c = check(&r, "clipping");
        assert_eq!(c.status, "fail", "{}", c.details);
        assert_eq!(c.fix, "declip");
        let pos = |op: &str| r.fixes.iter().position(|f| f.starts_with(op)).unwrap();
        assert!(pos("declip") < pos("gain"), "{:?}", r.fixes);
        let cmd = r.fix_command.as_deref().unwrap();
        assert!(cmd.starts_with("ferraus fx mix.wav -o mix.fixed.wav --fx \"declip\""), "{cmd}");
    }

    #[test]
    fn float_overs_are_not_clipping() {
        // a 32-bit float mix peaking at +3.5 dBFS is not clipped yet: the gain fixes it
        let mut b = clean_mix();
        let peak = b.peak();
        b.scale(1.5 / peak);
        let r = run(&b, "pop", &MixLimits::default());
        let c = check(&r, "clipping");
        assert_eq!(c.status, "pass", "{}", c.details);
        assert!(c.details.contains("not clipped yet"), "{}", c.details);
        assert_eq!(status(&r, "true_peak"), "fail");
        assert!(r.fixes.iter().all(|f| f != "declip"), "{:?}", r.fixes);
    }

    #[test]
    fn tonal_excess_fails_with_an_eq_cut_and_harsh_warns() {
        // noise over 2.2-4.2 kHz (about three 1/3-octave bands) adding 12 dB to each band
        let b = clean_mix();
        let n = noise(48000, 3.0, 1.0, 9);
        let band = lowpass4(&highpass4(&n.data[0], 48000.0, 2200.0), 48000.0, 4200.0);
        let rms = (band.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / band.len() as f64).sqrt();
        let bands = ferraus_analysis::spectrum::third_octave_levels(&b);
        let at3k = bands.iter().find(|x| x.center_hz == 3150.0).unwrap().db;
        // band level is sine-referenced (+3 dB over the mean square)
        let want_rms = (3.0 * 10f64.powf((at3k - 3.0103 + 12.0) / 10.0)).sqrt();
        let hot = AudioBuffer::from_mono(48000, band.iter().map(|v| v * (want_rms / rms) as f32).collect());
        let mixed = scale_to_lufs(add(&b, &hot.with_channels(2)), -18.0);
        let r = run(&mixed, "pop", &MixLimits::default());
        let c = check(&r, "tonal_balance");
        assert_eq!(c.status, "fail", "{}", c.details);
        assert!(c.value.as_f64().unwrap() > 6.0, "{}", c.value);
        assert!(c.details.contains("too strong"), "{}", c.details);
        assert!(c.fix.starts_with("eq:band=peak:") && c.fix.contains("kHz:-"), "{}", c.fix);
        assert_eq!(status(&r, "harsh"), "warn", "{}", check(&r, "harsh").details);
        // a wider tolerance lets it through
        let r = run(&mixed, "pop", &MixLimits { tolerance_db: 24.0, ..MixLimits::default() });
        assert_eq!(status(&r, "tonal_balance"), "pass");
    }

    #[test]
    fn polarity_flip_fails_phase() {
        let mut b = clean_mix();
        b.data[1] = b.data[0].iter().map(|v| -v).collect();
        let r = run(&b, "pop", &MixLimits::default());
        let c = check(&r, "phase");
        assert_eq!(c.status, "fail", "{}", c.details);
        assert_eq!(c.fix, "channels mix:invert=R");
        assert_eq!(status(&r, "mono_loss"), "warn");
    }

    #[test]
    fn dc_warns_then_fails() {
        let mut b = clean_mix();
        b.data.iter_mut().flatten().for_each(|v| *v += 0.002);
        let r = run(&b, "pop", &MixLimits::default());
        assert_eq!(status(&r, "dc_offset"), "warn");
        assert!(r.pass, "a small offset only warns: {:?}", r.failed);
        b.data.iter_mut().flatten().for_each(|v| *v += 0.01);
        let r = run(&b, "pop", &MixLimits::default());
        assert_eq!(status(&r, "dc_offset"), "fail");
        assert_eq!(r.fixes.first().map(String::as_str), Some("dc-remove"), "{:?}", r.fixes);
    }

    #[test]
    fn mono_silent_and_short_inputs() {
        // mono: the stereo checks skip, the rest still runs
        let mono = AudioBuffer::from_mono(48000, clean_mix().mono_mix());
        let r = run(&mono, "pop", &MixLimits::default());
        for c in ["phase", "mono_bass", "mono_loss"] {
            assert_eq!(status(&r, c), "skip", "{c}");
        }
        assert_eq!(status(&r, "integrated_lufs"), "pass");
        // silence: fails without panicking, and no gain is offered
        let r = run(&AudioBuffer::new(48000, 2, 48000), "auto", &MixLimits::default());
        assert!(!r.pass);
        assert_eq!(status(&r, "integrated_lufs"), "fail");
        assert_eq!(status(&r, "problems"), "fail", "{}", check(&r, "problems").details);
        assert_eq!(status(&r, "tonal_balance"), "skip");
        assert_eq!(status(&r, "plr"), "skip");
        assert!(r.fixes.iter().all(|f| !f.starts_with("gain")), "{:?}", r.fixes);
        // 200 ms: loudness cannot be measured
        let short = sine(48000, 220.0, 0.2, 0.3).with_channels(2);
        let r = run(&short, "auto", &MixLimits::default());
        assert_eq!(status(&r, "integrated_lufs"), "fail");
        assert!(check(&r, "integrated_lufs").details.contains("cannot be measured"));
    }

    #[test]
    fn level_gain_aims_inside_the_window() {
        let lim = MixLimits::default();
        // the rough pop mix of the calibration: 0.5 dB over the ceiling
        assert_eq!(level_gain(-17.06, -2.51, &lim), Some(-1.0));
        // master level: under the ceiling and inside the window at once
        let g = level_gain(-9.23, -0.21, &lim).unwrap();
        assert!(-9.23 + g <= -13.0 && -0.21 + g <= -3.5, "{g}");
        // too quiet with room: raised 1 LU into the window
        assert_eq!(level_gain(-30.0, -14.0, &lim), Some(5.0));
        // too quiet and too peaky: no single gain
        assert_eq!(level_gain(-30.0, -1.0, &lim), None);
        assert_eq!(level_gain(f64::NEG_INFINITY, f64::NEG_INFINITY, &lim), None);
        // already fine: no change
        assert_eq!(level_gain(-18.0, -6.0, &lim), Some(0.0));
    }

    #[test]
    fn low_end_deficits_only_count_where_the_target_has_low_end() {
        let band = |c: f64, delta: f64, reference: f64| BandDelta {
            center_hz: c,
            level_db: -30.0,
            reference_db: reference,
            delta_db: delta,
        };
        let mut bands = vec![band(63.0, -15.0, -15.0), band(80.0, -12.0, -10.0)];
        bands.extend([100.0, 125.0, 160.0, 200.0, 250.0].map(|c| band(c, 0.0, 0.0)));
        let tb = |bands: Vec<BandDelta>| ToneBalance {
            offset_db: 0.0,
            bands,
            largest_deviations: vec![],
            rms_deviation_db: 0.0,
        };
        // a target that is itself 10-15 dB down there (the podcast curve): no failure
        let sm = smoothed_deltas(&tb(bands.clone()));
        assert!(sm.iter().all(|(_, d)| *d > -6.0), "{sm:?}");
        // a target with full low end flags the same deficit
        for b in bands.iter_mut().take(2) {
            b.reference_db = 0.0;
        }
        let sm = smoothed_deltas(&tb(bands));
        assert!(sm[0].1 < -6.0, "{sm:?}");
        assert_eq!(tonal_regions(&sm, 6.0).len(), 1);
    }

    #[test]
    fn fix_command_quotes_paths_and_orders_stages() {
        let cmd = fx_command("takes/my mix.wav", &["dc-remove".into(), "gain:by=-2dB".into()]);
        assert_eq!(
            cmd,
            "ferraus fx 'takes/my mix.wav' -o 'takes/my mix.fixed.wav' --fx \"dc-remove\" --fx \"gain:by=-2dB\""
        );
        // the directory keeps the separators as typed, on every OS
        assert_eq!(fixed_path("takes\\my mix.wav"), "takes\\my mix.fixed.wav");
        assert_eq!(fixed_path("mix.wav"), "mix.fixed.wav");
        assert_eq!(fixed_path("-"), "mix.fixed.wav");
        let mut s = vec!["gain:by=-1dB", "widen:amount=80%", "eq:band=peak:1kHz:-3dB:q=1.0", "declip", "dc-remove"];
        s.sort_by_key(|x| stage_rank(x));
        assert_eq!(s, ["dc-remove", "declip", "eq:band=peak:1kHz:-3dB:q=1.0", "widen:amount=80%", "gain:by=-1dB"]);
        assert_eq!(narrow_stage(f64::NEG_INFINITY), "widen:amount=95%");
        assert_eq!(narrow_stage(0.0), "widen:amount=70%");
    }
}
