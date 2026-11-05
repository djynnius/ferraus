//! Spectral masking between two stems (`analyze masking`): where the priority stem `a` and
//! the yielding stem `b` crowd the same third-octave bands while both are playing.
//!
//! * Spectral overlap per band: `s = clamp((min(a_rel, b_rel) + 18) / 18, 0, 1)`, where
//!   `rel` is the band's long-term level minus the stem's own loudest band. Both stems
//!   within 18 dB of their loudest band means they share that territory; the measure
//!   ignores the gains the stems happen to have now (they are usually not balanced yet).
//! * Co-activity: the stems are cut into `window` frames; a frame is active when its RMS
//!   is within `floor` dB of that stem's loudest frame (and above -80 dBFS).
//!   `co_activity = both_active / a_active`: the share of the time the priority stem plays
//!   during which `b` plays too. (Not `both / either`: a kick is active in only about 20%
//!   of 100 ms frames, so against a sustained bass that ratio stays near 0.2 and the most
//!   common masking pair in a mix would never be reported.)
//! * Band score `= s * (0.25 + 0.75 * co_activity) * level`: two stems that never play
//!   together score at most 0.25, however much their spectra overlap.
//! * `level` (with `levels: as-is`, the default): how loud `b` is against `a` in the band
//!   while each plays (long-term band level corrected by the stem's active share). 1 when
//!   `b` is as loud as `a` or louder, falling linearly to 0 when `b` sits
//!   [`CLEAR_MARGIN_DB`] below `a`. A part well under the priority part in a band does not
//!   mask it, so a cut or a fader move on `b` lowers the score, and the analysis can gate a
//!   mix. With `levels: ignore` the factor is 1 and only the spectral shapes count: use that
//!   on raw stems whose gains mean nothing yet.
//! * Runs of consecutive bands scoring at or above the threshold become regions with a
//!   peak-EQ cut for `b`. Level-aware: the cut that brings the band under the threshold
//!   (rounded up to 0.5 dB, at most `max_cut`). Shape-only: 2 dB at the threshold growing
//!   to 4 dB at a score of 1.

use crate::spectrum::{self, BandLevel};
use crate::util::pow_db;
use ferraus_core::resample::{resample, Quality};
use ferraus_core::AudioBuffer;
use serde::Serialize;
use std::borrow::Cow;

/// Margin (dB) by which `b` must sit under `a` in a band, while both play, for the band to
/// stop counting as masked (the level factor reaches 0).
pub const CLEAR_MARGIN_DB: f64 = 10.0;

/// Lowest and highest band centres analysed (Hz).
pub const LOW_BAND_HZ: f64 = 31.5;
pub const HIGH_BAND_HZ: f64 = 16000.0;
/// Two stems share a band when both are within this many dB of their own loudest band.
const SHARED_RANGE_DB: f64 = 18.0;
/// Frames quieter than this never count as playing (dBFS RMS).
const ABSOLUTE_GATE_DB: f64 = -80.0;

/// Options for [`masking`].
#[derive(Clone, Debug)]
pub struct MaskingOptions {
    /// Band score (0..1) at or above which a band belongs to a reported region.
    pub threshold: f64,
    /// Co-activity frame length (seconds).
    pub window_s: f64,
    /// A frame is active when its RMS is within this many dB (negative) of the stem's
    /// loudest frame.
    pub floor_db: f64,
    /// Deepest suggested cut (negative dB).
    pub max_cut_db: f64,
    /// Weigh each band by how loud `b` is against `a` (true), or compare spectral shapes
    /// only (false, for unbalanced raw stems).
    pub levels: bool,
    /// Names of the stems in the summary sentence.
    pub a_label: String,
    pub b_label: String,
}

impl Default for MaskingOptions {
    fn default() -> Self {
        MaskingOptions {
            threshold: 0.5,
            window_s: 0.1,
            floor_db: -30.0,
            max_cut_db: -4.0,
            levels: true,
            a_label: "a".into(),
            b_label: "b".into(),
        }
    }
}

/// One third-octave band of a masking report.
#[derive(Clone, Debug, Serialize)]
pub struct MaskingBand {
    pub center_hz: f64,
    /// Long-term level of each stem in the band (dB re a full-scale sine).
    pub a_db: f64,
    pub b_db: f64,
    /// Level relative to the stem's own loudest band (0 = its loudest band).
    pub a_rel_db: f64,
    pub b_rel_db: f64,
    /// How far `a` sits above `b` in the band while each plays (dB; negative = `b` is
    /// louder). Present when levels are taken into account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_db: Option<f64>,
    /// 0..1: spectral overlap weighted by co-activity (and by the level margin).
    pub score: f64,
}

/// A run of adjacent bands at or above the threshold, with an EQ cut for `b`.
#[derive(Clone, Debug, Serialize)]
pub struct MaskingRegion {
    pub low_hz: f64,
    pub high_hz: f64,
    /// Centre of the highest-scoring band of the region (where the cut goes).
    pub center_hz: f64,
    /// Score of that band.
    pub score: f64,
    /// Levels of both stems in that band (dB re a full-scale sine).
    pub a_db: f64,
    pub b_db: f64,
    /// `a` minus `b` in that band while each plays (dB); with levels taken into account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_db: Option<f64>,
    /// Suggested peak-EQ gain for `b` (negative dB).
    pub cut_db: f64,
    pub q: f64,
    /// A runnable fx stage, e.g. `eq:band=peak:250Hz:-3dB:q=1.4`.
    pub suggested_fix: String,
    /// The stem the fix is meant for (always `b`, the stem that yields).
    pub apply_to: String,
    /// Centres of the first and last band of the region (for the summary).
    #[serde(skip)]
    pub first_band_hz: f64,
    #[serde(skip)]
    pub last_band_hz: f64,
}

/// Result of [`masking`].
#[derive(Clone, Debug, Serialize)]
pub struct MaskingReport {
    /// Share of the frames in which `a` plays that `b` plays too (0..1).
    pub co_activity: f64,
    /// Share of all frames in which `a` (and `b`) plays.
    pub a_active: f64,
    pub b_active: f64,
    /// Highest band score (0..1).
    pub overall: f64,
    /// Bands from 31.5 Hz to 16 kHz (limited by the Nyquist frequency).
    pub bands: Vec<MaskingBand>,
    /// Regions at or above the threshold, worst first.
    pub regions: Vec<MaskingRegion>,
    /// Every region's cut as one fx stage (`eq:band=...,band=...`); null without regions.
    pub suggested_fix: Option<String>,
    pub summary: String,
    /// `a` or `b` never rises above the activity gate.
    #[serde(skip)]
    pub a_silent: bool,
    #[serde(skip)]
    pub b_silent: bool,
    /// Length of `b` minus length of `a` (seconds, after resampling).
    #[serde(skip)]
    pub length_difference_s: f64,
    /// What was done to make the inputs comparable (resampling).
    #[serde(skip)]
    pub notes: Vec<String>,
}

/// Mono signal of a buffer, borrowed when it is already mono.
fn mono(buf: &AudioBuffer) -> Cow<'_, [f32]> {
    if buf.channels() == 1 {
        Cow::Borrowed(buf.data[0].as_slice())
    } else {
        Cow::Owned(buf.mono_mix())
    }
}

/// Per-frame activity: frame RMS within `floor_db` of the loudest frame and above the
/// absolute gate. Frames past the end of the signal are silent.
pub fn activity(x: &[f32], hop: usize, n_frames: usize, floor_db: f64) -> Vec<bool> {
    let hop = hop.max(1);
    let ms: Vec<f64> = (0..n_frames)
        .map(|f| {
            let s = (f * hop).min(x.len());
            let e = (s + hop).min(x.len());
            x[s..e].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / hop as f64
        })
        .collect();
    let max = ms.iter().copied().fold(0.0f64, f64::max);
    let gate = (pow_db(max) + floor_db).max(ABSOLUTE_GATE_DB);
    ms.iter().map(|m| *m > 0.0 && pow_db(*m) >= gate).collect()
}

/// A frequency as the unit parser reads it: `31.5Hz`, `250Hz`, `2.5kHz`, `16kHz`.
pub fn fmt_hz(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{}kHz", fmt_num(hz / 1000.0))
    } else {
        format!("{}Hz", fmt_num(hz))
    }
}

/// A number with at most two decimals and no trailing zeros (`-3`, `-2.5`, `1.4`).
fn fmt_num(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// A frequency for prose: `250 Hz`, `2.5 kHz`.
fn say_hz(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{} kHz", fmt_num(hz / 1000.0))
    } else {
        format!("{} Hz", fmt_num(hz))
    }
}

/// The peak-EQ fx stage for one cut.
pub fn eq_stage(center_hz: f64, cut_db: f64, q: f64) -> String {
    format!("eq:band={}", eq_band(center_hz, cut_db, q))
}

fn eq_band(center_hz: f64, cut_db: f64, q: f64) -> String {
    format!("peak:{}:{}dB:q={}", fmt_hz(center_hz), fmt_num(cut_db), fmt_num(q))
}

/// Level factor of a band from the margin of `a` over `b` (dB): 1 at 0 dB or less (`b` as
/// loud as `a`), 0 at [`CLEAR_MARGIN_DB`] or more.
pub fn level_factor(margin_db: f64) -> f64 {
    (1.0 - margin_db / CLEAR_MARGIN_DB).clamp(0.0, 1.0)
}

/// Level-aware cut: how far `b` must come down in the band so that
/// `shape * level_factor(margin + cut)` falls under `threshold` (`shape` is the band score
/// without the level factor). Rounded up to 0.5 dB, at least 1 dB, at most `max_cut_db`.
pub fn cut_to_clear(margin_db: f64, threshold: f64, shape: f64, max_cut_db: f64) -> f64 {
    let target_factor = (threshold / shape.max(1e-9)).min(1.0);
    let needed_margin = CLEAR_MARGIN_DB * (1.0 - target_factor) + 0.25;
    let cut = ((needed_margin - margin_db).max(1.0) * 2.0).ceil() / 2.0;
    -cut.min(max_cut_db.abs())
}

/// Suggested cut for a band score: 2 dB at the threshold, 4 dB at a score of 1, rounded
/// to 0.5 dB and never deeper than `max_cut_db`.
pub fn cut_depth(score: f64, threshold: f64, max_cut_db: f64) -> f64 {
    let t = if threshold < 1.0 { ((score - threshold) / (1.0 - threshold)).clamp(0.0, 1.0) } else { 0.0 };
    let cut = (-(2.0 + 2.0 * t) * 2.0).round() / 2.0;
    cut.max(-max_cut_db.abs())
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// ISO 266 preferred name of a third-octave band from its exact (base-2) centre:
/// 99.2 Hz → 100, 396.9 Hz → 400, 10079 Hz → 10000. These are the frequencies an engineer
/// types into an EQ, so regions and fixes use them.
pub fn iso_third_octave(center_hz: f64) -> f64 {
    const R10: [f64; 10] = [1.0, 1.25, 1.6, 2.0, 2.5, 3.15, 4.0, 5.0, 6.3, 8.0];
    let n = (10.0 * center_hz.max(1e-3).log10()).round() as i32;
    R10[n.rem_euclid(10) as usize] * 10f64.powi(n.div_euclid(10))
}

/// Exact centre of a third-octave band (its upper edge may be clipped at Nyquist).
fn exact_center(band: &BandLevel) -> f64 {
    band.low_hz * 2f64.powf(1.0 / 6.0)
}

/// Where `b` competes with the priority stem `a`: per-band scores, regions above the
/// threshold, and an EQ cut for `b` per region. Both stems are mixed to mono, `b` is
/// resampled to `a`'s rate, and both are read from their first sample (stems of one song
/// are expected to start together).
pub fn masking(a: &AudioBuffer, b: &AudioBuffer, opts: &MaskingOptions) -> MaskingReport {
    let sr = a.sample_rate;
    let mut notes = Vec::new();
    let ma = mono(a);
    let mut mb = mono(b);
    if b.sample_rate != sr && !mb.is_empty() {
        notes.push(format!("resampled b from {} Hz to {} Hz", b.sample_rate, sr));
        let r = resample(&AudioBuffer::from_mono(b.sample_rate, mb.into_owned()), sr, Quality::Medium);
        mb = Cow::Owned(r.data.into_iter().next().unwrap_or_default());
    }
    let length_difference_s = (mb.len() as f64 - ma.len() as f64) / sr.max(1) as f64;

    // co-activity
    let hop = ((opts.window_s * sr as f64).round() as usize).max(1);
    let n_frames = ma.len().max(mb.len()).div_ceil(hop);
    let act_a = activity(&ma, hop, n_frames, opts.floor_db);
    let act_b = activity(&mb, hop, n_frames, opts.floor_db);
    let (mut na, mut nb, mut both) = (0usize, 0usize, 0usize);
    for (&x, &y) in act_a.iter().zip(&act_b) {
        na += x as usize;
        nb += y as usize;
        both += (x && y) as usize;
    }
    let co_activity = if na > 0 { both as f64 / na as f64 } else { 0.0 };
    let frac = |n: usize| if n_frames > 0 { n as f64 / n_frames as f64 } else { 0.0 };

    // long-term third-octave spectra
    let levels = |m: Cow<'_, [f32]>| -> Vec<BandLevel> {
        spectrum::third_octave_levels(&AudioBuffer::from_mono(sr, m.into_owned()))
            .into_iter()
            .map(|band| BandLevel { center_hz: iso_third_octave(exact_center(&band)), ..band })
            .filter(|band| band.center_hz >= LOW_BAND_HZ && band.center_hz <= HIGH_BAND_HZ)
            .collect()
    };
    let la = levels(ma);
    let lb = levels(mb);
    let loudest = |l: &[BandLevel]| l.iter().map(|x| x.db).fold(f64::NEG_INFINITY, f64::max);
    let (max_a, max_b) = (loudest(&la), loudest(&lb));
    let a_silent = na == 0 || max_a <= -140.0;
    let b_silent = nb == 0 || max_b <= -140.0;
    // band level while playing: the long-term level is averaged over silence too, so a
    // kick that plays 20% of the time reads about 7 dB under its level while it hits
    let playing = |n: usize| if n > 0 && n_frames > 0 { -pow_db(n as f64 / n_frames as f64) } else { 0.0 };
    let (lift_a, lift_b) = (playing(na), playing(nb));
    let activity_weight = 0.25 + 0.75 * co_activity;
    let bands: Vec<MaskingBand> = la
        .iter()
        .zip(&lb)
        .map(|(x, y)| {
            let (a_rel, b_rel) = (x.db - max_a, y.db - max_b);
            let s = if a_silent || b_silent {
                0.0
            } else {
                ((a_rel.min(b_rel) + SHARED_RANGE_DB) / SHARED_RANGE_DB).clamp(0.0, 1.0)
            };
            let margin = (x.db + lift_a) - (y.db + lift_b);
            let level = if opts.levels { level_factor(margin) } else { 1.0 };
            MaskingBand {
                center_hz: x.center_hz,
                a_db: round1(x.db),
                b_db: round1(y.db),
                a_rel_db: round1(a_rel),
                b_rel_db: round1(b_rel),
                margin_db: opts.levels.then(|| round1(margin)),
                score: s * activity_weight * level,
            }
        })
        .collect();
    let overall = bands.iter().map(|x| x.score).fold(0.0, f64::max);

    // regions: runs of consecutive bands at or above the threshold
    let thr = opts.threshold;
    let mut regions = Vec::new();
    let mut i = 0;
    while i < bands.len() {
        if bands[i].score < thr {
            i += 1;
            continue;
        }
        let start = i;
        while i < bands.len() && bands[i].score >= thr {
            i += 1;
        }
        let run = start..i;
        let k = run.clone().fold(start, |m, j| if bands[j].score > bands[m].score { j } else { m });
        let peak = &bands[k];
        let cut = match peak.margin_db {
            // the cut that takes the band's level factor under the threshold
            Some(margin) if peak.score > 0.0 => {
                let shape = peak.score / level_factor(margin).max(1e-9);
                cut_to_clear(margin, thr, shape, opts.max_cut_db)
            }
            _ => cut_depth(peak.score, thr, opts.max_cut_db),
        };
        let q = if run.len() <= 2 { 1.4 } else { 1.0 };
        regions.push(MaskingRegion {
            low_hz: round1(la[start].low_hz),
            high_hz: round1(la[i - 1].high_hz),
            center_hz: peak.center_hz,
            score: peak.score,
            a_db: peak.a_db,
            b_db: peak.b_db,
            margin_db: peak.margin_db,
            cut_db: cut,
            q,
            suggested_fix: eq_stage(peak.center_hz, cut, q),
            apply_to: "b".into(),
            first_band_hz: la[start].center_hz,
            last_band_hz: la[i - 1].center_hz,
        });
    }
    regions.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
    let suggested_fix = if regions.is_empty() {
        None
    } else {
        let parts: Vec<String> =
            regions.iter().map(|r| format!("band={}", eq_band(r.center_hz, r.cut_db, r.q))).collect();
        Some(format!("eq:{}", parts.join(",")))
    };

    let mut report = MaskingReport {
        co_activity,
        a_active: frac(na),
        b_active: frac(nb),
        overall,
        bands,
        regions,
        suggested_fix,
        summary: String::new(),
        a_silent,
        b_silent,
        length_difference_s,
        notes,
    };
    report.summary = summary(&report, opts);
    report
}

fn summary(r: &MaskingReport, opts: &MaskingOptions) -> String {
    let (a, b) = (&opts.a_label, &opts.b_label);
    let pct = |v: f64| format!("{:.0}%", v * 100.0);
    if r.a_silent || r.b_silent {
        let which = match (r.a_silent, r.b_silent) {
            (true, true) => format!("{a} and {b} are"),
            (true, false) => format!("{a} is"),
            _ => format!("{b} is"),
        };
        return format!("{which} silent, so nothing can be masked.");
    }
    let along = pct(r.co_activity);
    match r.regions.first() {
        None => {
            let worst =
                r.bands.iter().max_by(|x, y| x.score.partial_cmp(&y.score).unwrap_or(std::cmp::Ordering::Equal));
            match worst {
                Some(w) => format!(
                    "No masking at or above {}: {b} and {a} overlap most at {} (score {:.2}); {b} plays along {along} of the time {a} plays.",
                    pct(opts.threshold),
                    say_hz(w.center_hz),
                    w.score
                ),
                None => "No bands to compare.".into(),
            }
        }
        Some(w) => {
            let more = match r.regions.len() {
                1 => String::new(),
                n => format!(" ({} more region{} listed)", n - 1, if n > 2 { "s" } else { "" }),
            };
            let span = if w.first_band_hz == w.last_band_hz {
                say_hz(w.center_hz)
            } else {
                format!("{}–{}", say_hz(w.first_band_hz), say_hz(w.last_band_hz))
            };
            format!(
                "{b} masks {a} most at {span} (score {:.2}, playing along {along} of the time {a} plays): cut {b} by {} dB at {} ({}){more}.",
                w.score,
                fmt_num(-w.cut_db),
                say_hz(w.center_hz),
                w.suggested_fix
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::op::parse_stage_string;
    use ferraus_core::testing::{add, noise, sine, stereo};
    use ferraus_core::units;

    const SR: u32 = 22050;

    /// `freq` sine that is on during the given half-second slots of a `secs` long signal.
    fn bursts(freq: f64, secs: f64, odd: bool) -> AudioBuffer {
        let mut x = sine(SR, freq, secs, 0.5);
        let slot = SR as usize / 2;
        for (i, v) in x.data[0].iter_mut().enumerate() {
            if ((i / slot) % 2 == 1) != odd {
                *v = 0.0;
            }
        }
        x
    }

    /// Parse a suggested fix the way `fx` and `eq` do.
    fn parse_fix(fix: &str) -> Vec<(f64, f64, f64)> {
        let (name, pairs) = parse_stage_string(fix).unwrap();
        assert_eq!(name, "eq");
        pairs
            .iter()
            .map(|(k, v)| {
                assert_eq!(k, "band");
                let p: Vec<&str> = v.split(':').collect();
                assert_eq!(p.len(), 4, "{v}");
                assert_eq!(p[0], "peak");
                let q = p[3].strip_prefix("q=").unwrap().parse::<f64>().unwrap();
                (units::parse_freq(p[1]).unwrap(), units::parse_db(p[2]).unwrap(), q)
            })
            .collect()
    }

    #[test]
    fn sines_two_octaves_apart_do_not_mask() {
        let a = sine(SR, 250.0, 2.0, 0.5);
        let b = sine(SR, 1000.0, 2.0, 0.5);
        let r = masking(&a, &b, &MaskingOptions::default());
        assert!((r.co_activity - 1.0).abs() < 1e-9);
        assert!(r.overall < 0.5, "{}", r.overall);
        assert!(r.regions.is_empty());
        assert!(r.suggested_fix.is_none());
        assert!(r.summary.starts_with("No masking"), "{}", r.summary);
        assert_eq!(r.bands.first().unwrap().center_hz, 31.5);
        assert_eq!(r.bands.last().unwrap().center_hz, 10000.0); // Nyquist 11025 Hz
    }

    #[test]
    fn same_band_sines_give_one_region_with_a_valid_fix() {
        let a = sine(SR, 400.0, 2.0, 0.5);
        let b = sine(SR, 410.0, 2.0, 0.1);
        let shapes = MaskingOptions { levels: false, ..Default::default() };
        let r = masking(&a, &b, &shapes);
        assert_eq!(r.regions.len(), 1, "{:?}", r.regions);
        let g = &r.regions[0];
        assert_eq!(g.center_hz, 400.0);
        assert!(g.low_hz < 400.0 && g.high_hz > 400.0);
        assert!(g.score > 0.99);
        assert_eq!(g.cut_db, -4.0);
        assert_eq!(g.q, 1.4);
        assert_eq!(g.apply_to, "b");
        assert_eq!(g.suggested_fix, "eq:band=peak:400Hz:-4dB:q=1.4");
        assert_eq!(parse_fix(&g.suggested_fix), vec![(400.0, -4.0, 1.4)]);
        assert_eq!(r.suggested_fix.as_deref(), Some(g.suggested_fix.as_str()));
        // shape-only ignores the stems' gains: b is 14 dB quieter and still masks
        assert!((r.overall - 1.0).abs() < 1e-6);
        assert!(r.summary.contains("cut b by 4 dB at 400 Hz"), "{}", r.summary);
        assert!(g.margin_db.is_none());
    }

    #[test]
    fn levels_decide_whether_a_quieter_part_masks() {
        let a = sine(SR, 400.0, 2.0, 0.5);
        // 14 dB under a in the same band: does not mask
        let r = masking(&a, &sine(SR, 410.0, 2.0, 0.1), &MaskingOptions::default());
        assert!(r.regions.is_empty(), "{:?}", r.regions);
        assert!(r.bands.iter().any(|b| b.margin_db.is_some_and(|m| (m - 14.0).abs() < 0.5)));
        // as loud as a: masks, and the suggested cut is the one that clears the threshold
        let r = masking(&a, &sine(SR, 410.0, 2.0, 0.5), &MaskingOptions::default());
        assert_eq!(r.regions.len(), 1);
        let g = &r.regions[0];
        assert!(g.margin_db.is_some_and(|m| m.abs() < 0.5), "{:?}", g.margin_db);
        // 5.5 dB would clear it; capped at max-cut
        assert_eq!(g.cut_db, -4.0);
        // 3 dB under: a smaller cut clears it
        let r = masking(&a, &sine(SR, 410.0, 2.0, 0.354), &MaskingOptions::default());
        let g = &r.regions[0];
        assert_eq!(g.cut_db, -2.5, "{g:?}");
        // applying that cut (as a level drop of b) brings the band under the threshold
        let r = masking(&a, &sine(SR, 410.0, 2.0, 0.354 * 10f32.powf(-2.5 / 20.0)), &MaskingOptions::default());
        assert!(r.regions.is_empty(), "{:?}", r.regions);
    }

    #[test]
    fn level_aware_cut_depth() {
        // shape 1, threshold 0.5: needs a 5.25 dB margin
        assert_eq!(cut_to_clear(0.0, 0.5, 1.0, -8.0), -5.5);
        assert_eq!(cut_to_clear(3.0, 0.5, 1.0, -8.0), -2.5);
        assert_eq!(cut_to_clear(5.0, 0.5, 1.0, -8.0), -1.0); // at least 1 dB
        assert_eq!(cut_to_clear(-6.0, 0.5, 1.0, -4.0), -4.0); // capped
        assert_eq!(level_factor(0.0), 1.0);
        assert_eq!(level_factor(-3.0), 1.0);
        assert_eq!(level_factor(CLEAR_MARGIN_DB), 0.0);
    }

    #[test]
    fn never_simultaneous_bursts_score_low() {
        let a = bursts(400.0, 4.0, false);
        let b = bursts(400.0, 4.0, true);
        let r = masking(&a, &b, &MaskingOptions::default());
        assert!(r.co_activity < 0.05, "{}", r.co_activity);
        assert!((r.a_active - 0.5).abs() < 0.05 && (r.b_active - 0.5).abs() < 0.05);
        assert!(r.overall <= 0.25 + 1e-9, "{}", r.overall);
        assert!(r.regions.is_empty());
    }

    #[test]
    fn sparse_priority_stem_against_a_sustained_part_is_reported() {
        // a kick-like part playing 20% of the time over a sustained bass in the same band:
        // whenever a plays, b plays, so the band scores 1 (both / either would give 0.2)
        let mut kick = sine(SR, 63.0, 4.0, 0.8);
        for (i, v) in kick.data[0].iter_mut().enumerate() {
            if (i % (SR as usize / 2)) >= SR as usize / 10 {
                *v = 0.0;
            }
        }
        let bass = sine(SR, 60.0, 4.0, 0.3);
        let shapes = MaskingOptions { levels: false, ..Default::default() };
        let r = masking(&kick, &bass, &shapes);
        assert!((r.a_active - 0.2).abs() < 0.03, "{}", r.a_active);
        assert!(r.co_activity > 0.99, "{}", r.co_activity);
        assert_eq!(r.regions.len(), 1);
        assert_eq!(r.regions[0].center_hz, 63.0);
        // level-aware, the kick is compared while it plays: its long-term level reads 7 dB
        // low (20% duty), yet a bass as loud as the hits still masks it
        let loud_bass = sine(SR, 60.0, 4.0, 0.8);
        let r = masking(&kick, &loud_bass, &MaskingOptions::default());
        assert_eq!(r.regions.len(), 1, "{:?}", r.bands.iter().map(|b| (b.center_hz, b.margin_db)).collect::<Vec<_>>());
        assert!(r.regions[0].margin_db.is_some_and(|m| m.abs() < 1.5), "{:?}", r.regions[0].margin_db);
        // the other way round the kick covers only a fifth of the bass's time
        let r = masking(&bass, &kick, &shapes);
        assert!((r.co_activity - 0.2).abs() < 0.03, "{}", r.co_activity);
        assert!(r.regions.is_empty());
    }

    #[test]
    fn half_overlap_scales_the_score_and_the_cut() {
        // a plays throughout, b half of the time: co-activity 0.5, score 0.625
        let a = sine(SR, 1000.0, 4.0, 0.5);
        let b = bursts(1000.0, 4.0, true);
        let r = masking(&a, &b, &MaskingOptions::default());
        assert!((r.co_activity - 0.5).abs() < 0.03, "{}", r.co_activity);
        let g = &r.regions[0];
        assert_eq!(g.center_hz, 1000.0);
        assert!((g.score - 0.625).abs() < 0.03, "{}", g.score);
        assert_eq!(g.cut_db, -2.5);
        assert_eq!(g.suggested_fix, "eq:band=peak:1kHz:-2.5dB:q=1.4");
        let o = MaskingOptions { threshold: 0.7, ..Default::default() };
        assert!(masking(&a, &b, &o).regions.is_empty());
    }

    #[test]
    fn broadband_overlap_is_one_wide_region() {
        // two white noises (+3 dB per octave in third-octave bands) share the top octaves:
        // one region of many bands (q 1.0) reaching the highest band
        let a = noise(SR, 2.0, 0.3, 1);
        let b = noise(SR, 2.0, 0.3, 2);
        let o = MaskingOptions { max_cut_db: -3.0, ..Default::default() };
        let r = masking(&a, &b, &o);
        assert_eq!(r.regions.len(), 1, "{:?}", r.regions);
        let g = &r.regions[0];
        assert_eq!(g.q, 1.0);
        assert!(g.low_hz > 500.0 && g.low_hz < 2000.0 && g.high_hz > 11000.0, "{g:?}");
        assert!(g.center_hz >= 5000.0, "{}", g.center_hz);
        assert_eq!(g.cut_db, -3.0); // clamped by max-cut
        assert_eq!(parse_fix(&g.suggested_fix).len(), 1);
        assert!(r.bands[0].score < 0.1); // 31.5 Hz is 25 dB below the top band
    }

    #[test]
    fn two_regions_combine_into_one_stage() {
        let a = add(&sine(SR, 100.0, 2.0, 0.5), &sine(SR, 2000.0, 2.0, 0.5));
        let b = add(&sine(SR, 100.0, 2.0, 0.5), &sine(SR, 2000.0, 2.0, 0.4));
        let r = masking(&a, &b, &MaskingOptions::default());
        assert_eq!(r.regions.len(), 2, "{:?}", r.regions);
        assert!(r.regions[0].score >= r.regions[1].score);
        let all = parse_fix(r.suggested_fix.as_deref().unwrap());
        assert_eq!(all.len(), 2);
        let mut hz: Vec<f64> = all.iter().map(|x| x.0).collect();
        hz.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert_eq!(hz, vec![100.0, 2000.0]);
        assert!(r.summary.contains("1 more region listed"), "{}", r.summary);
    }

    #[test]
    fn silence_masks_nothing() {
        let a = AudioBuffer::new(SR, 1, SR as usize);
        let b = sine(SR, 400.0, 1.0, 0.5);
        let r = masking(&a, &b, &MaskingOptions::default());
        assert!(r.a_silent && !r.b_silent);
        assert_eq!(r.overall, 0.0);
        assert!(r.regions.is_empty());
        assert_eq!(r.co_activity, 0.0);
        assert!(r.summary.contains("a is silent"), "{}", r.summary);
        let r = masking(&AudioBuffer::new(SR, 2, 0), &AudioBuffer::new(SR, 2, 0), &MaskingOptions::default());
        assert!(r.a_silent && r.b_silent && r.regions.is_empty());
    }

    #[test]
    fn stereo_and_other_rates_are_conformed() {
        let a = stereo(sine(48000, 1000.0, 2.0, 0.5));
        let b = sine(44100, 1000.0, 2.5, 0.5);
        let r = masking(&a, &b, &MaskingOptions::default());
        assert_eq!(r.notes, vec!["resampled b from 44100 Hz to 48000 Hz".to_string()]);
        assert!((r.length_difference_s - 0.5).abs() < 0.01);
        assert_eq!(r.regions.len(), 1);
        assert_eq!(r.regions[0].center_hz, 1000.0);
        assert_eq!(r.bands.last().unwrap().center_hz, 16000.0);
        // b plays whenever a does (it only runs on longer)
        assert!((r.co_activity - 1.0).abs() < 1e-9, "{}", r.co_activity);
        assert!((r.b_active - 1.0).abs() < 1e-9 && (r.a_active - 0.8).abs() < 0.02);
        let r = masking(&b, &a, &MaskingOptions::default());
        assert!((r.co_activity - 0.8).abs() < 0.02, "{}", r.co_activity);
    }

    #[test]
    fn bands_carry_iso_names() {
        let r = masking(&noise(48000, 1.0, 0.3, 1), &noise(48000, 1.0, 0.3, 2), &MaskingOptions::default());
        let hz: Vec<f64> = r.bands.iter().map(|b| b.center_hz).collect();
        let iso = [
            31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0,
            1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10000.0, 12500.0, 16000.0,
        ];
        assert_eq!(hz, iso);
        assert_eq!(iso_third_octave(19.69), 20.0);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_hz(31.5), "31.5Hz");
        assert_eq!(fmt_hz(250.0), "250Hz");
        assert_eq!(fmt_hz(1250.0), "1.25kHz");
        assert_eq!(fmt_hz(3150.0), "3.15kHz");
        assert_eq!(fmt_hz(16000.0), "16kHz");
        assert_eq!(cut_depth(0.5, 0.5, -4.0), -2.0);
        assert_eq!(cut_depth(1.0, 0.5, -4.0), -4.0);
        assert_eq!(cut_depth(0.8, 0.5, -4.0), -3.0);
        assert_eq!(cut_depth(1.0, 0.5, 3.0), -3.0);
        assert_eq!(cut_depth(1.0, 1.0, -4.0), -2.0);
    }
}
