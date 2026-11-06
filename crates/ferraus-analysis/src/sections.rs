//! Song structure without a model (best-effort): where the sections are, which ones repeat,
//! how loud each one is, and a guess at its role (intro, verse, pre-chorus, chorus, bridge,
//! instrumental, outro).
//!
//! 1. Grid: beats from [`tempo::detect_tempo`], brought to the given tempo (or to half a
//!    fast reading) by taking every other beat or adding midpoints, gaps filled and the
//!    grid extended over the whole file. Bars start on the tracker's downbeats when it is
//!    confident, else on the beat phase with the strongest accent (low-band onsets plus
//!    chord changes); with no trustworthy beat the grid is a fixed 2 s "bar" of four
//!    0.5 s units.
//! 2. Per beat: chroma (12 pitch classes, L2-normalised), spectral shape (13 cepstral
//!    coefficients of a 40-band log-mel spectrum, level removed), and RMS level.
//! 3. Bar self-similarity: chroma compared beat by beat across a bar (a one-bar time-delay
//!    embedding, so the order of the chords counts) and timbre compared as the distance
//!    between the bars' mean spectral shapes in dB, combined.
//! 4. Boundaries: Foote checkerboard novelty (Gaussian-tapered kernels at two scales) plus
//!    level jumps between neighbouring bar pairs; peaks above a moving median and an
//!    absolute floor (both set by the sensitivity), at least `min_bars` apart.
//! 5. Labels: segments compared bar by bar (chroma, weighted most, and onset patterns,
//!    best alignment within the length difference plus one bar), plus mean spectral shape,
//!    level, and vocal activity; average-linkage grouping at the similarity threshold,
//!    where a section joins a label only if it is nearly as alike to it as the label's
//!    sections are to each other; letters by first appearance.
//! 6. Roles: heuristics on loudness, repetition, position, and vocal activity.

use crate::{chords, loudness, tempo, util};
use ferraus_core::resample::{resample_channel, Quality};
use ferraus_core::AudioBuffer;
use realfft::RealFftPlanner;
use serde::Serialize;
use std::collections::BTreeMap;

/// Analysis sample rate: higher rates are resampled down (chroma and timbre need no more).
const ANALYSIS_RATE: u32 = 22050;
/// Tempo confidence below which the tracked beats are not trusted.
pub const MIN_TEMPO_CONFIDENCE: f64 = 0.3;
/// Detected tempi above this are tracked at half speed when the half tempo scores nearly as well.
const HALF_TIME_ABOVE_BPM: f64 = 160.0;
/// Unit length of the fallback grid (four units make a 2 s bar).
const FALLBACK_UNIT_S: f64 = 0.5;
const N_MELS: usize = 40;
const N_CEPS: usize = 13;
/// Onset-pattern slots per beat (sixteenth notes in 4/4).
const SLOTS: usize = 4;
/// Bounds of the spectral-shape distance (rms dB per mel band) at which timbre similarity
/// falls to 1/e: the song's median distance between bars, clamped to this range.
const TIMBRE_SCALE_MIN_DB: f64 = 1.0;
const TIMBRE_SCALE_MAX_DB: f64 = 3.0;
/// Level difference at which level similarity falls to 1/e.
const LEVEL_SCALE_DB: f64 = 6.0;
/// Weight of harmonic change (chroma before versus after) in the novelty curve.
const HARMONY_NOVELTY_WEIGHT: f64 = 0.8;
/// Novelty added per dB of level jump between neighbouring bar pairs (capped at 10 dB).
const LEVEL_NOVELTY_PER_DB: f64 = 0.03;
/// A section joins a label only when its mean similarity to the label's sections is at
/// most this far below their similarity to each other.
const LABEL_GAP: f64 = 0.1;
/// Similarity removed per unit of vocal-activity difference between two sections.
const VOCAL_LABEL_PENALTY: f64 = 0.2;
/// Beat-by-beat chroma similarity at which the chroma part of a section comparison is 0
/// (it is 1 at identical chroma): parts over the same chords with another melody land
/// near 0.85 raw, true repeats near 0.95 and above.
const CHROMA_FLOOR: f64 = 0.5;
/// Vocal activity at or above which a section counts as sung, below which as unsung.
const SUNG: f64 = 0.3;
const UNSUNG: f64 = 0.15;

/// Options for [`sections`].
#[derive(Clone, Debug)]
pub struct SectionOptions {
    /// Shortest section in bars.
    pub min_bars: usize,
    /// 0..1: higher finds more boundaries.
    pub sensitivity: f64,
    /// 0..1: how alike two sections must be to share a label.
    pub similarity: f64,
    /// Override the detected meter.
    pub beats_per_bar: Option<usize>,
    /// A known tempo (BPM); overrides the detected one (the beat phase is still tracked).
    pub bpm: Option<f64>,
}

impl Default for SectionOptions {
    fn default() -> Self {
        SectionOptions { min_bars: 4, sensitivity: 0.5, similarity: 0.8, beats_per_bar: None, bpm: None }
    }
}

/// One section of the song.
#[derive(Clone, Debug, Serialize)]
pub struct Section {
    pub index: usize,
    /// `A`, `B`, ... by first appearance; sections with the same letter repeat each other.
    pub label: String,
    pub start_s: f64,
    pub end_s: f64,
    /// First bar of the section (1-based; a pickup before bar 1 belongs to the first section).
    pub start_bar: usize,
    /// Whole bars in the section.
    pub bars: usize,
    /// Integrated loudness of the section (`null` when silent).
    pub lufs: f64,
    /// Section loudness minus the song's integrated loudness.
    pub delta_lu: f64,
    /// 0..1 share of the section in which the vocal stem is active (only with a vocal stem).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocal_activity: Option<f64>,
    /// Index of the first section with the same label (`null` for that first one).
    pub repeat_of: Option<usize>,
    /// 0..1 similarity to that first section.
    pub similarity_to_repeat: Option<f64>,
    /// intro, verse, pre-chorus, chorus, bridge, instrumental, outro, or section.
    pub role: String,
    /// 0..1: how clear the evidence for the role was.
    pub role_confidence: f64,
}

/// Summary of one label.
#[derive(Clone, Debug, Serialize)]
pub struct LabelSummary {
    pub count: usize,
    pub mean_lufs: f64,
    /// The most common role among the label's sections.
    pub role: String,
}

/// Two sections with the same label.
#[derive(Clone, Debug, Serialize)]
pub struct Repeat {
    pub a: usize,
    pub b: usize,
    pub label: String,
    pub similarity: f64,
    /// Time from a bar in section `a` to the same bar in section `b` (a bad bar at `t` in
    /// `a` has its twin at `t + offset_s`).
    pub offset_s: f64,
}

/// A condition the caller should report as a warning.
#[derive(Clone, Debug)]
pub struct SectionWarning {
    pub code: &'static str,
    pub message: String,
}

/// Result of [`sections`].
#[derive(Clone, Debug, Serialize)]
pub struct SectionsReport {
    pub tempo_bpm: f64,
    /// `detected` or `given`.
    pub tempo_source: String,
    pub tempo_confidence: f64,
    pub beats_per_bar: usize,
    pub bar_s: f64,
    /// What the bars are built from: `downbeats`, `beats`, or `seconds` (no usable beat).
    pub grid: String,
    pub duration_s: f64,
    pub integrated_lufs: f64,
    pub sections: Vec<Section>,
    pub labels: BTreeMap<String, LabelSummary>,
    pub repeats: Vec<Repeat>,
    /// Start times of every section after the first.
    pub boundaries_s: Vec<f64>,
    /// 0..1 overall confidence in the structure (grid, boundary strength, repeat clarity).
    pub confidence: f64,
    #[serde(skip)]
    pub warnings: Vec<SectionWarning>,
}

/// Find the sections of `buf`. `vocal` is the lead vocal stem of the same song, read from
/// its first sample like the mix.
pub fn sections(buf: &AudioBuffer, vocal: Option<&AudioBuffer>, opts: &SectionOptions) -> SectionsReport {
    let duration_s = buf.duration_s();
    let integrated_lufs = loudness::integrated_lufs(buf);
    let min_bars = opts.min_bars.max(1);
    let mut warnings = Vec::new();
    let mono = analysis_signal(buf);
    let given = opts.bpm.filter(|b| b.is_finite() && *b > 0.0);
    let tg = tempo_grid(&mono, given);
    if tg.low_confidence && !buf.is_empty() {
        let message = if let Some(b) = given {
            format!("tempo confidence is low ({:.2}); bars follow a strict grid at the given {b} BPM", tg.confidence)
        } else {
            format!(
                "tempo confidence is low ({:.2}); bars fall back to a fixed 2 s grid (pass --bpm to use the real bars)",
                tg.confidence
            )
        };
        warnings.push(SectionWarning { code: "low_tempo_confidence", message });
    }
    let feats = unit_features(&mono, &tg.units);
    drop(mono);
    let bars = bar_grid(&tg, &feats, opts.beats_per_bar);
    let m = bars.starts.len();
    let bar_time = |k: usize| -> f64 {
        if k < m {
            tg.units[bars.starts[k]]
        } else {
            duration_s
        }
    };
    let bar_s = if m >= 2 {
        let d: Vec<f64> = (1..m).map(|k| bar_time(k) - bar_time(k - 1)).collect();
        util::median(&d)
    } else if tg.fallback {
        FALLBACK_UNIT_S * 4.0
    } else if tg.bpm > 0.0 {
        60.0 / tg.bpm * bars.bpb as f64
    } else {
        0.0
    };

    // bar features and the self-similarity matrix
    let bf = bar_features(&feats, &bars);
    let ssm = combined_ssm(&bf);

    // boundaries
    let mut strengths = Vec::new();
    let mut cuts: Vec<usize> = Vec::new();
    if m < 2 * min_bars {
        if !buf.is_empty() {
            warnings.push(SectionWarning {
                code: "too_short",
                message: format!(
                    "the file has {m} whole bar(s), fewer than twice --min-bars ({min_bars}); it is returned as one section"
                ),
            });
        }
    } else {
        let nov = novelty_curve(&ssm, &bf, min_bars);
        cuts = pick_boundaries(&nov, min_bars, opts.sensitivity);
        strengths = cuts.iter().map(|&k| nov[k]).collect();
    }
    let mut segs: Vec<(usize, usize)> = Vec::new();
    let mut prev = 0;
    for &c in &cuts {
        segs.push((prev, c));
        prev = c;
    }
    segs.push((prev, m));

    let ns = segs.len();
    let times: Vec<(f64, f64)> = (0..ns)
        .map(|i| {
            let start = if i == 0 { 0.0 } else { bar_time(segs[i].0) };
            let end = if i + 1 < ns { bar_time(segs[i + 1].0) } else { duration_s };
            (start, end)
        })
        .collect();
    let vocal_frames = vocal.map(vocal_activity_frames);
    let activity: Option<Vec<f64>> =
        vocal_frames.as_ref().map(|(act, hop)| times.iter().map(|&(a, b)| activity_between(act, *hop, a, b)).collect());

    // labels: sections that differ in how much the vocal sings are less alike
    let mut seg_sim = vec![vec![1.0; ns]; ns];
    let mut seg_lag = vec![vec![0isize; ns]; ns];
    for i in 0..ns {
        for j in i + 1..ns {
            let (mut s, lag) = segment_similarity(&bf, &ssm, segs[i], segs[j]);
            if let Some(va) = &activity {
                s = (s - VOCAL_LABEL_PENALTY * (va[i] - va[j]).abs()).max(0.0);
            }
            seg_sim[i][j] = s;
            seg_sim[j][i] = s;
            seg_lag[i][j] = lag;
            seg_lag[j][i] = -lag;
        }
    }
    let ids = cluster(&seg_sim, opts.similarity.clamp(0.0, 1.0));

    // sections with times, loudness, vocal activity
    if let Some(v) = vocal {
        let d = v.duration_s() - duration_s;
        if d.abs() > 0.05 {
            let rel = if d > 0.0 { "longer" } else { "shorter" };
            warnings.push(SectionWarning {
                code: "vocal_length_differs",
                message: format!(
                    "the vocal stem is {:.2} s {rel} than the mix; both are read from their first sample, so they must start together",
                    d.abs()
                ),
            });
        }
    }
    let sr = buf.sample_rate as f64;
    let mut secs: Vec<Section> = Vec::with_capacity(ns);
    if !buf.is_empty() {
        for (i, &(b0, b1)) in segs.iter().enumerate() {
            let (start_s, end_s) = times[i];
            let a = ((start_s * sr).round() as usize).min(buf.frames());
            let b = ((end_s * sr).round() as usize).clamp(a, buf.frames());
            let lufs = loudness::integrated_lufs(&buf.slice(a, b));
            let label = label_name(ids[i]);
            let first = (0..i).find(|&j| ids[j] == ids[i]);
            secs.push(Section {
                index: i,
                label,
                start_s,
                end_s,
                start_bar: b0 + 1,
                bars: b1 - b0,
                lufs,
                delta_lu: if lufs.is_finite() && integrated_lufs.is_finite() {
                    lufs - integrated_lufs
                } else {
                    f64::NEG_INFINITY
                },
                vocal_activity: activity.as_ref().map(|va| va[i]),
                repeat_of: first,
                similarity_to_repeat: first.map(|j| seg_sim[j][i]),
                role: String::new(),
                role_confidence: 0.0,
            });
        }
    }
    assign_roles(&mut secs, duration_s);

    // repeats
    let mut repeats = Vec::new();
    for i in 0..secs.len() {
        for j in i + 1..secs.len() {
            if ids[i] != ids[j] {
                continue;
            }
            let lag = seg_lag[i][j];
            let tb = bar_time(((segs[j].0 as isize + lag).max(0) as usize).min(m));
            let ta = bar_time(segs[i].0);
            repeats.push(Repeat {
                a: i,
                b: j,
                label: secs[i].label.clone(),
                similarity: seg_sim[i][j],
                offset_s: tb - ta,
            });
        }
    }
    if repeats.is_empty() && !warnings.iter().any(|w| w.code == "too_short") && !buf.is_empty() {
        let message = if secs.len() <= 1 {
            "no section boundaries were found, so nothing repeats; raise --sensitivity or lower --min-bars to split the song".to_string()
        } else {
            format!(
                "no two of the {} sections are alike enough to share a label; lower --similarity (now {:.0}%) to group looser repeats",
                secs.len(),
                opts.similarity * 100.0
            )
        };
        warnings.push(SectionWarning { code: "no_repeats", message });
    }

    // label summaries
    let mut labels: BTreeMap<String, LabelSummary> = BTreeMap::new();
    for s in &secs {
        if labels.contains_key(&s.label) {
            continue;
        }
        let members: Vec<&Section> = secs.iter().filter(|x| x.label == s.label).collect();
        let l: Vec<f64> = members.iter().map(|x| x.lufs).filter(|v| v.is_finite()).collect();
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for x in &members {
            match counts.iter_mut().find(|c| c.0 == x.role) {
                Some(c) => c.1 += 1,
                None => counts.push((&x.role, 1)),
            }
        }
        // most common role; ties keep the first section's role
        let role = counts.iter().fold(("section", 0usize), |best, c| if c.1 > best.1 { *c } else { best }).0;
        labels.insert(
            s.label.clone(),
            LabelSummary {
                count: members.len(),
                mean_lufs: if l.is_empty() { f64::NEG_INFINITY } else { util::mean(&l) },
                role: role.to_string(),
            },
        );
    }

    // overall confidence: grid quality x (boundary strength and repeat clarity)
    let grid_q = match bars.kind {
        "downbeats" => 1.0,
        "beats" => 0.85,
        _ => 0.5,
    };
    let boundary_q = if strengths.is_empty() {
        0.0
    } else {
        strengths.iter().map(|s| (s / 0.3).clamp(0.0, 1.0)).sum::<f64>() / strengths.len() as f64
    };
    let label_q = if secs.len() < 2 {
        0.0
    } else if repeats.is_empty() {
        0.3
    } else {
        let mut within = Vec::new();
        let mut across = Vec::new();
        for i in 0..ns {
            for j in i + 1..ns {
                if ids[i] == ids[j] {
                    within.push(seg_sim[i][j]);
                } else {
                    across.push(seg_sim[i][j]);
                }
            }
        }
        if across.is_empty() {
            0.2
        } else {
            ((util::mean(&within) - util::mean(&across)) / 0.3).clamp(0.0, 1.0)
        }
    };
    let confidence = grid_q * (0.5 * boundary_q + 0.5 * label_q);

    SectionsReport {
        tempo_bpm: given.unwrap_or(tg.bpm),
        tempo_source: if given.is_some() { "given" } else { "detected" }.to_string(),
        tempo_confidence: tg.confidence,
        beats_per_bar: bars.bpb,
        bar_s,
        grid: bars.kind.to_string(),
        duration_s,
        integrated_lufs,
        boundaries_s: secs.iter().skip(1).map(|s| s.start_s).collect(),
        sections: secs,
        labels,
        repeats,
        confidence,
        warnings,
    }
}

/// `A`..`Z`, then `A1`..`Z1`, ...
fn label_name(n: usize) -> String {
    let c = (b'A' + (n % 26) as u8) as char;
    if n < 26 {
        c.to_string()
    } else {
        format!("{c}{}", n / 26)
    }
}

/// The mono mix at no more than [`ANALYSIS_RATE`].
fn analysis_signal(buf: &AudioBuffer) -> AudioBuffer {
    let x = buf.mono_mix();
    if buf.sample_rate > ANALYSIS_RATE + 2000 {
        AudioBuffer::from_mono(ANALYSIS_RATE, resample_channel(&x, buf.sample_rate, ANALYSIS_RATE, Quality::Fast))
    } else {
        AudioBuffer::from_mono(buf.sample_rate, x)
    }
}

// ---------------------------------------------------------------------------------------
// grid

struct TempoGrid {
    /// Start times of the grid units (beats, or 0.5 s steps on the fallback grid).
    units: Vec<f64>,
    bpm: f64,
    confidence: f64,
    low_confidence: bool,
    /// The fixed 2 s grid is in use.
    fallback: bool,
    downbeats: Vec<f64>,
    meter_bpb: usize,
}

/// The beat grid. The tracker runs over its full range first, because it follows best the
/// pulse the music articulates (eighth-note hats or arpeggios); the target tempo (the given
/// one, or half a fast reading whose half scores nearly as well, since bars shorter than
/// ~1.5 s are rare) is then reached from those beats by taking every other beat or adding
/// midpoints. Only a given tempo that matches no tracked level is tracked again, within 4%.
fn tempo_grid(mono: &AudioBuffer, given: Option<f64>) -> TempoGrid {
    let dur = mono.duration_s();
    let free = tempo::detect_tempo(mono, &tempo::TempoOptions::default());
    let usable = |r: &tempo::TempoResult| r.beats.len() >= 4 && r.confidence >= MIN_TEMPO_CONFIDENCE;
    let meter = |r: &tempo::TempoResult| if r.time_signature == "3/4" { 3 } else { 4 };
    let free_ok = usable(&free);
    let target = given.or_else(|| {
        let half = free.bpm / 2.0;
        let half_scores = free.alternatives.iter().any(|c| (c.bpm / half - 1.0).abs() < 0.03 && c.score >= 0.8);
        (free_ok && free.bpm > HALF_TIME_ABOVE_BPM && meter(&free) == 4 && half_scores).then_some(half)
    });
    let level = target.map(|t| free.bpm / t).unwrap_or(1.0);
    let fast_period = 60.0 / free.bpm.max(1.0);
    // (beats, bpm, confidence, downbeats, beats per bar)
    let (beats, bpm, confidence, downbeats, meter_bpb) = if free_ok && (level - 1.0).abs() < 0.04 {
        let m = meter(&free);
        (free.beats, free.bpm, free.confidence, free.downbeats, m)
    } else if free_ok && (level - 2.0).abs() < 0.08 {
        let all = complete_beats(&free.beats, fast_period, dur);
        (every_other_beat(mono, &all), free.bpm / 2.0, free.confidence, Vec::new(), 4)
    } else if free_ok && (level - 0.5).abs() < 0.02 {
        let all = complete_beats(&free.beats, fast_period, dur);
        let mut b: Vec<f64> = all.windows(2).flat_map(|w| [w[0], 0.5 * (w[0] + w[1])]).collect();
        b.extend(all.last());
        (b, free.bpm * 2.0, free.confidence, Vec::new(), 4)
    } else if let Some(t) = target {
        let r = tempo::detect_tempo(
            mono,
            &tempo::TempoOptions { min_bpm: t * 0.96, max_bpm: t * 1.04, prior_bpm: t, ..Default::default() },
        );
        let m = meter(&r);
        (r.beats, r.bpm, r.confidence, r.downbeats, m)
    } else {
        let m = meter(&free);
        (free.beats, free.bpm, free.confidence, free.downbeats, m)
    };
    let trusted = beats.len() >= 4 && confidence >= MIN_TEMPO_CONFIDENCE;
    let (units, fallback) = match given {
        Some(b) if trusted => (complete_beats(&beats, 60.0 / b, dur), false),
        Some(b) => (strict_grid(&beats, 60.0 / b, dur), false),
        None if trusted => (complete_beats(&beats, 60.0 / bpm.max(1.0), dur), false),
        None => {
            let n = (dur / FALLBACK_UNIT_S).ceil() as usize;
            ((0..n).map(|i| i as f64 * FALLBACK_UNIT_S).collect(), true)
        }
    };
    TempoGrid {
        units,
        bpm,
        confidence,
        low_confidence: !trusted,
        fallback,
        downbeats: if trusted { downbeats } else { Vec::new() },
        meter_bpb,
    }
}

/// Every other beat of a fast grid: the parity whose beats carry the stronger onsets
/// (kick and snare, chord changes) is the slower pulse.
fn every_other_beat(mono: &AudioBuffer, beats: &[f64]) -> Vec<f64> {
    let env = tempo::onset_envelope(mono);
    let at = |t: f64| -> f64 {
        let c = (t * env.frame_rate).round().max(0.0) as usize;
        let end = (c + 3).min(env.values.len());
        let r = c.saturating_sub(2).min(end)..end;
        env.values[r.clone()].iter().fold(0.0f32, |m, v| m.max(*v)) as f64
            + env.low[r].iter().fold(0.0f32, |m, v| m.max(*v)) as f64
    };
    let strength = |p: usize| beats.iter().skip(p).step_by(2).map(|t| at(*t)).sum::<f64>();
    let parity = if strength(1) > strength(0) { 1 } else { 0 };
    beats.iter().skip(parity).step_by(2).copied().collect()
}

/// Tracked beats with gaps filled and the grid extended to the start and end of the file.
fn complete_beats(beats: &[f64], period: f64, dur: f64) -> Vec<f64> {
    if beats.len() < 2 {
        return strict_grid(beats, period, dur);
    }
    let ibis: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let p = util::median(&ibis);
    let p = if p > 0.0 { p } else { period };
    let mut out = Vec::with_capacity(beats.len() + 16);
    // a beat up to a quarter period before the start counts as the first beat (at 0)
    let first = beats[0];
    let mut k = (first / p + 0.25).floor() as usize;
    while k > 0 {
        out.push((first - k as f64 * p).max(0.0));
        k -= 1;
    }
    for w in beats.windows(2) {
        out.push(w[0]);
        let gap = w[1] - w[0];
        let k = (gap / p).round() as usize;
        for j in 1..k {
            out.push(w[0] + gap * j as f64 / k as f64);
        }
    }
    let mut t = *beats.last().unwrap_or(&0.0);
    while t < dur {
        out.push(t);
        t += p;
    }
    out
}

/// A strict grid at `period`, phase fitted to `beats` (circular mean), from the start.
fn strict_grid(beats: &[f64], period: f64, dur: f64) -> Vec<f64> {
    let period = period.max(0.05);
    let phase = if beats.is_empty() {
        0.0
    } else {
        let (s, c) = beats.iter().fold((0.0, 0.0), |(s, c), b| {
            let a = 2.0 * std::f64::consts::PI * (b / period).fract();
            (s + a.sin(), c + a.cos())
        });
        (s.atan2(c) / (2.0 * std::f64::consts::PI)).rem_euclid(1.0) * period
    };
    let n = ((dur - phase) / period).ceil().max(0.0) as usize;
    (0..n).map(|k| phase + k as f64 * period).collect()
}

struct Bars {
    /// Index in the unit grid of each bar's first unit (whole bars only).
    starts: Vec<usize>,
    bpb: usize,
    kind: &'static str,
}

fn bar_grid(tg: &TempoGrid, f: &UnitFeatures, override_bpb: Option<usize>) -> Bars {
    let n = tg.units.len();
    let (bpb, phase, kind) = if tg.fallback {
        (4, 0, "seconds")
    } else {
        let bpb = override_bpb.unwrap_or(tg.meter_bpb).max(1);
        if !tg.downbeats.is_empty() && bpb == tg.meter_bpb {
            let mut votes = vec![0usize; bpb];
            for d in &tg.downbeats {
                votes[nearest(&tg.units, *d) % bpb] += 1;
            }
            let phase = (0..bpb).max_by_key(|p| (votes[*p], std::cmp::Reverse(*p))).unwrap_or(0);
            (bpb, phase, "downbeats")
        } else {
            (bpb, accent_phase(f, bpb), "beats")
        }
    };
    let starts = (phase..).step_by(bpb).take_while(|s| s + bpb <= n).collect();
    Bars { starts, bpb, kind }
}

fn nearest(v: &[f64], t: f64) -> usize {
    let i = v.partition_point(|x| *x < t);
    if i == 0 {
        0
    } else if i >= v.len() {
        v.len() - 1
    } else if (v[i] - t) < (t - v[i - 1]) {
        i
    } else {
        i - 1
    }
}

/// The beat phase (0..bpb) whose beats carry the strongest accents: low-band onsets
/// (kick drums) plus chord changes, each z-scored.
fn accent_phase(f: &UnitFeatures, bpb: usize) -> usize {
    let n = f.accent.len();
    if bpb <= 1 || n < 2 * bpb {
        return 0;
    }
    let change: Vec<f64> =
        (0..n).map(|i| if i == 0 { 0.0 } else { 1.0 - cos_sim(&f.chroma[i - 1], &f.chroma[i]) }).collect();
    let z = |v: &[f64]| -> Vec<f64> {
        let m = util::mean(v);
        let sd = util::std_dev(v).max(1e-9);
        v.iter().map(|x| (x - m) / sd).collect()
    };
    let (a, c) = (z(&f.accent), z(&change));
    let score = |p: usize| -> f64 {
        let v: Vec<f64> = (p..n).step_by(bpb).map(|i| a[i] + c[i]).collect();
        util::mean(&v)
    };
    (0..bpb).max_by(|x, y| score(*x).partial_cmp(&score(*y)).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// features

struct UnitFeatures {
    chroma: Vec<[f64; 12]>,
    /// Cepstral coefficients 1..=13 of the unit's mean log-mel spectrum (dB units).
    timbre: Vec<[f64; N_CEPS]>,
    /// Mean square of the mono signal.
    power: Vec<f64>,
    /// Low-band onset strength at the unit start (dB rise).
    accent: Vec<f64>,
    /// Onset strength in [`SLOTS`] slots of the unit, below and above ~200 Hz.
    rhythm: Vec<[f64; 2 * SLOTS]>,
}

/// Mel band powers per frame (frames centred on `i * hop`), the hop in seconds, and the
/// number of bands below ~200 Hz.
fn mel_power(x: &[f32], sr: f64) -> (Vec<[f32; N_MELS]>, f64, usize) {
    let n_fft = util::next_pow2((sr * 0.046) as usize).max(256);
    let hop = n_fft / 2;
    let bank = util::mel_filterbank(sr, n_fft, N_MELS, 30.0, 16000.0f64.min(sr / 2.0));
    let low_bands = bank
        .iter()
        .filter(|f| f.iter().map(|(k, _)| *k).max().unwrap_or(0) as f64 * sr / (n_fft as f64) < 200.0)
        .count()
        .max(1);
    if x.is_empty() {
        return (Vec::new(), hop as f64 / sr, low_bands);
    }
    let win = util::hann(n_fft);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let mut input = fft.make_input_vec();
    let mut out = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut power = vec![0.0f32; n_fft / 2 + 1];
    let half = (n_fft / 2) as isize;
    let n_frames = x.len() / hop + 1;
    let mut frames = Vec::with_capacity(n_frames);
    for f in 0..n_frames {
        let start = (f * hop) as isize - half;
        for (k, slot) in input.iter_mut().enumerate() {
            let idx = start + k as isize;
            *slot = if idx >= 0 && (idx as usize) < x.len() { x[idx as usize] * win[k] } else { 0.0 };
        }
        fft.process_with_scratch(&mut input, &mut out, &mut scratch).expect("fft sizes match");
        for (p, c) in power.iter_mut().zip(&out) {
            *p = c.norm_sqr();
        }
        let mut m = [0.0f32; N_MELS];
        for (b, filt) in bank.iter().enumerate() {
            m[b] = filt.iter().map(|(k, w)| power[*k] * w).sum();
        }
        frames.push(m);
    }
    (frames, hop as f64 / sr, low_bands)
}

/// Frames (centred on `i * hop_s`) whose centre lies in `[a, b)`; the nearest frame when none does.
fn frame_span(hop_s: f64, count: usize, a: f64, b: f64) -> (usize, usize) {
    if count == 0 {
        return (0, 0);
    }
    let lo = ((a / hop_s).ceil().max(0.0) as usize).min(count);
    let hi = ((b / hop_s).ceil().max(0.0) as usize).min(count);
    if lo < hi {
        (lo, hi)
    } else {
        let c = ((0.5 * (a + b) / hop_s).round().max(0.0) as usize).min(count - 1);
        (c, c + 1)
    }
}

fn unit_features(mono: &AudioBuffer, units: &[f64]) -> UnitFeatures {
    let sr = mono.sample_rate as f64;
    let x: &[f32] = mono.data.first().map(|c| c.as_slice()).unwrap_or(&[]);
    let dur = mono.duration_s();
    let n = units.len();
    let cg = chords::chromagram(mono);
    let (mel, mel_hop, low_bands) = mel_power(x, sr);
    let top = mel.iter().flat_map(|f| f.iter()).fold(0.0f32, |m, v| m.max(*v)) as f64;
    let floor = if top > 0.0 { top * 1e-8 } else { 1e-20 };
    let low_db: Vec<f64> =
        mel.iter().map(|f| 10.0 * (f[..low_bands].iter().map(|v| *v as f64).sum::<f64>() + floor).log10()).collect();
    let low_flux: Vec<f64> = (0..low_db.len())
        .map(|i| if i < 2 { 0.0 } else { (low_db[i] - low_db[i - 1].min(low_db[i - 2])).max(0.0) })
        .collect();
    // spectral flux (mean dB rise per band) below and above ~200 Hz, per frame
    let mut band_flux: Vec<(f64, f64)> = Vec::with_capacity(mel.len());
    let mut prev = [0.0f64; N_MELS];
    for (fi, f) in mel.iter().enumerate() {
        let db: [f64; N_MELS] = std::array::from_fn(|b| 10.0 * (f[b] as f64 + floor).log10());
        let (mut lo, mut hi) = (0.0, 0.0);
        if fi > 0 {
            for (b, (d, p)) in db.iter().zip(&prev).enumerate() {
                let rise = (d - p).max(0.0);
                if b < low_bands {
                    lo += rise;
                } else {
                    hi += rise;
                }
            }
        }
        band_flux.push((lo / low_bands as f64, hi / (N_MELS - low_bands).max(1) as f64));
        prev = db;
    }
    let mut out = UnitFeatures {
        chroma: Vec::with_capacity(n),
        timbre: Vec::with_capacity(n),
        power: Vec::with_capacity(n),
        accent: Vec::with_capacity(n),
        rhythm: Vec::with_capacity(n),
    };
    for i in 0..n {
        let a = units[i];
        let b = if i + 1 < n { units[i + 1] } else { dur.max(a) };
        // chroma
        let mut c = [0.0f64; 12];
        let (lo, hi) = frame_span(cg.hop_s, cg.chroma.len(), a, b);
        for f in &cg.chroma[lo..hi] {
            for (s, v) in c.iter_mut().zip(f) {
                *s += v;
            }
        }
        let norm = c.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm > 1e-12 {
            c.iter_mut().for_each(|v| *v /= norm);
        }
        out.chroma.push(c);
        // timbre
        let (lo, hi) = frame_span(mel_hop, mel.len(), a, b);
        let mut t = [0.0f64; N_CEPS];
        if hi > lo {
            let mut acc = [0.0f64; N_MELS];
            for f in &mel[lo..hi] {
                for (s, v) in acc.iter_mut().zip(f) {
                    *s += *v as f64;
                }
            }
            let db: Vec<f32> = acc.iter().map(|p| (10.0 * (p / (hi - lo) as f64 + floor).log10()) as f32).collect();
            let cep = util::dct2(&db, N_CEPS + 1);
            for (s, v) in t.iter_mut().zip(&cep[1..]) {
                *s = *v as f64;
            }
        }
        out.timbre.push(t);
        // level
        let sa = ((a * sr).round() as usize).min(x.len());
        let sb = ((b * sr).round() as usize).clamp(sa, x.len());
        let p =
            if sb > sa { x[sa..sb].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (sb - sa) as f64 } else { 0.0 };
        out.power.push(p);
        // accent: the strongest low-band rise within 30 ms before to 70 ms after the unit start
        let (lo, hi) = frame_span(mel_hop, low_flux.len(), a - 0.03, a + 0.07);
        out.accent.push(low_flux[lo..hi].iter().copied().fold(0.0, f64::max));
        // onset pattern: the strongest flux within a frame of each slot
        let mut r = [0.0f64; 2 * SLOTS];
        if !band_flux.is_empty() {
            for slot in 0..SLOTS {
                let t = a + slot as f64 * (b - a) / SLOTS as f64;
                let c = (t / mel_hop).round().max(0.0) as usize;
                let end = (c + 2).min(band_flux.len());
                for (lf, hf) in &band_flux[c.saturating_sub(1).min(end)..end] {
                    r[slot] = r[slot].max(*lf);
                    r[SLOTS + slot] = r[SLOTS + slot].max(*hf);
                }
            }
        }
        out.rhythm.push(r);
    }
    out
}

struct BarFeatures {
    /// Per bar, the chroma of each of its beats.
    chroma: Vec<Vec<[f64; 12]>>,
    /// Per bar, the sum of its beats' chroma (which pitch classes sound, in any order).
    bag: Vec<[f64; 12]>,
    timbre: Vec<[f64; N_CEPS]>,
    /// Per bar, the onset pattern of its beats (L2-normalised).
    rhythm: Vec<Vec<f64>>,
    level_db: Vec<f64>,
}

fn bar_features(f: &UnitFeatures, bars: &Bars) -> BarFeatures {
    let mut out = BarFeatures {
        chroma: Vec::new(),
        bag: Vec::new(),
        timbre: Vec::new(),
        rhythm: Vec::new(),
        level_db: Vec::new(),
    };
    for &s in &bars.starts {
        let r = s..s + bars.bpb;
        out.chroma.push(f.chroma[r.clone()].to_vec());
        let mut bag = [0.0f64; 12];
        for c in &f.chroma[r.clone()] {
            for (a, v) in bag.iter_mut().zip(c) {
                *a += v;
            }
        }
        out.bag.push(bag);
        let mut pattern: Vec<f64> = f.rhythm[r.clone()].iter().flat_map(|u| u.iter().copied()).collect();
        let norm = pattern.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm > 1e-12 {
            pattern.iter_mut().for_each(|v| *v /= norm);
        }
        out.rhythm.push(pattern);
        let mut t = [0.0f64; N_CEPS];
        for u in &f.timbre[r.clone()] {
            for (a, v) in t.iter_mut().zip(u) {
                *a += v / bars.bpb as f64;
            }
        }
        out.timbre.push(t);
        out.level_db.push(util::pow_db(util::mean(&f.power[r])));
    }
    out
}

fn cos_sim(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let (na, nb) = (a.iter().any(|v| *v != 0.0), b.iter().any(|v| *v != 0.0));
    match (na, nb) {
        (false, false) => 1.0,
        (true, true) => a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>().clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// Distance between two spectral shapes as rms dB per mel band (Parseval over the
/// orthonormal DCT, cepstral smoothing to 13 coefficients).
fn timbre_distance_db(a: &[f64; N_CEPS], b: &[f64; N_CEPS]) -> f64 {
    (a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>() / N_MELS as f64).sqrt()
}

fn timbre_sim(a: &[f64; N_CEPS], b: &[f64; N_CEPS], scale_db: f64) -> f64 {
    (-(timbre_distance_db(a, b) / scale_db).powi(2)).exp()
}

/// Cosine similarity of two non-negative vectors (1 when both are zero).
fn cos_vec(a: &[f64], b: &[f64]) -> f64 {
    let (na, nb) = (a.iter().map(|v| v * v).sum::<f64>().sqrt(), b.iter().map(|v| v * v).sum::<f64>().sqrt());
    if na <= 1e-12 || nb <= 1e-12 {
        return if na <= 1e-12 && nb <= 1e-12 { 1.0 } else { 0.0 };
    }
    (a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>() / (na * nb)).clamp(0.0, 1.0)
}

/// Bar-by-bar similarity matrices.
struct Ssm {
    /// Chroma compared beat by beat (the order of the chords counts).
    chroma: Vec<Vec<f64>>,
    /// Onset patterns.
    rhythm: Vec<Vec<f64>>,
    /// Texture for boundaries: mean of spectral-shape and onset-pattern similarity.
    texture: Vec<Vec<f64>>,
    /// The spectral-shape distance scale used (dB).
    timbre_scale_db: f64,
}

fn combined_ssm(bf: &BarFeatures) -> Ssm {
    let m = bf.chroma.len();
    let mut dist = Vec::with_capacity(m * m.saturating_sub(1) / 2);
    for k in 0..m {
        for l in k + 1..m {
            dist.push(timbre_distance_db(&bf.timbre[k], &bf.timbre[l]));
        }
    }
    let timbre_scale_db = if dist.is_empty() {
        TIMBRE_SCALE_MAX_DB
    } else {
        util::median(&dist).clamp(TIMBRE_SCALE_MIN_DB, TIMBRE_SCALE_MAX_DB)
    };
    let mut ssm = Ssm {
        chroma: vec![vec![1.0; m]; m],
        rhythm: vec![vec![1.0; m]; m],
        texture: vec![vec![1.0; m]; m],
        timbre_scale_db,
    };
    for k in 0..m {
        for l in k + 1..m {
            let beats = bf.chroma[k].len().max(1);
            let c = bf.chroma[k].iter().zip(&bf.chroma[l]).map(|(a, b)| cos_sim(a, b)).sum::<f64>() / beats as f64;
            let r = cos_vec(&bf.rhythm[k], &bf.rhythm[l]);
            let t = 0.5 * timbre_sim(&bf.timbre[k], &bf.timbre[l], timbre_scale_db) + 0.5 * r;
            for (mat, v) in [(&mut ssm.chroma, c), (&mut ssm.rhythm, r), (&mut ssm.texture, t)] {
                mat[k][l] = v;
                mat[l][k] = v;
            }
        }
    }
    ssm
}

// ---------------------------------------------------------------------------------------
// boundaries

/// Foote novelty with a Gaussian-tapered checkerboard kernel of half-width `w` bars:
/// mean within-block similarity minus mean cross-block similarity, for a boundary before
/// each bar (index `k` = boundary between bars `k-1` and `k`; entries 0 and `m` are 0).
fn foote(s: &[Vec<f64>], w: usize) -> Vec<f64> {
    let m = s.len();
    let sigma = (w as f64 / 2.0).max(0.5);
    let mut out = vec![0.0; m + 1];
    for (k, o) in out.iter_mut().enumerate().take(m).skip(1) {
        let (lo, hi) = (k.saturating_sub(w), (k + w).min(m));
        let (mut win, mut wsum, mut cross, mut csum) = (0.0, 0.0, 0.0, 0.0);
        for (a, row) in s.iter().enumerate().take(hi).skip(lo) {
            let ia = a as f64 - k as f64 + 0.5;
            for (b, v) in row.iter().enumerate().take(hi).skip(lo) {
                if a == b {
                    continue;
                }
                let ib = b as f64 - k as f64 + 0.5;
                let g = (-(ia * ia + ib * ib) / (2.0 * sigma * sigma)).exp();
                if (a < k) == (b < k) {
                    win += g * v;
                    wsum += g;
                } else {
                    cross += g * v;
                    csum += g;
                }
            }
        }
        *o = if wsum > 0.0 && csum > 0.0 { win / wsum - cross / csum } else { 0.0 };
    }
    out
}

/// Harmonic change before bar `k`: 1 - cosine between the summed chroma of the `w` bars
/// before and the `w` bars after (pitch content in any order, so a section that cycles a
/// progression looks the same throughout).
fn harmonic_change(bag: &[[f64; 12]], k: usize, w: usize) -> f64 {
    let sum = |r: std::ops::Range<usize>| -> [f64; 12] {
        let mut s = [0.0; 12];
        for b in &bag[r] {
            for (a, v) in s.iter_mut().zip(b) {
                *a += v;
            }
        }
        s
    };
    let m = bag.len();
    if k == 0 || k >= m {
        return 0.0;
    }
    1.0 - cos_vec(&sum(k.saturating_sub(w)..k), &sum(k..(k + w).min(m)))
}

/// Novelty before each bar: Foote novelty of the texture matrix at two scales, harmonic
/// change at two scales, and level jumps between neighbouring bar pairs.
fn novelty_curve(ssm: &Ssm, bf: &BarFeatures, min_bars: usize) -> Vec<f64> {
    let m = ssm.texture.len();
    let w1 = min_bars.clamp(2, 8);
    let w2 = (2 * min_bars).clamp(4, 12);
    let n1 = foote(&ssm.texture, w1);
    let n2 = foote(&ssm.texture, w2);
    let h1 = min_bars.clamp(4, 8);
    let energy_db = |r: std::ops::Range<usize>| -> f64 {
        let p: Vec<f64> = bf.level_db[r].iter().map(|d| 10f64.powf(d / 10.0)).collect();
        util::pow_db(util::mean(&p))
    };
    (0..=m)
        .map(|k| {
            if k == 0 || k >= m {
                return 0.0;
            }
            let jump = (energy_db(k..(k + 2).min(m)) - energy_db(k.saturating_sub(2)..k)).abs();
            let harmony = 0.5 * (harmonic_change(&bf.bag, k, h1) + harmonic_change(&bf.bag, k, 2 * h1));
            0.5 * (n1[k] + n2[k]) + HARMONY_NOVELTY_WEIGHT * harmony + LEVEL_NOVELTY_PER_DB * jump.min(10.0)
        })
        .collect()
}

/// Peaks of the novelty curve above a moving median plus a margin and above an absolute
/// floor (both lower at higher sensitivity), strongest first, at least `min_bars` apart
/// and from either end. Returns sorted bar indices.
fn pick_boundaries(nov: &[f64], min_bars: usize, sensitivity: f64) -> Vec<usize> {
    let m = nov.len().saturating_sub(1);
    let s = sensitivity.clamp(0.0, 1.0);
    let floor = 0.03 + 0.12 * (1.0 - s);
    let delta = 0.08 * (1.0 - s);
    let half = (2 * min_bars).clamp(8, 16);
    let mut cands: Vec<usize> = (min_bars..=m.saturating_sub(min_bars))
        .filter(|&k| k >= 1 && k < m)
        .filter(|&k| nov[k] >= nov[k - 1] && nov[k] >= nov[k + 1] && nov[k] >= floor)
        .filter(|&k| {
            let r: Vec<f64> = nov[k.saturating_sub(half).max(1)..(k + half + 1).min(m)].to_vec();
            nov[k] >= util::median(&r) + delta
        })
        .collect();
    cands.sort_by(|a, b| nov[*b].partial_cmp(&nov[*a]).unwrap_or(std::cmp::Ordering::Equal));
    let mut chosen: Vec<usize> = Vec::new();
    for k in cands {
        if chosen.iter().all(|c| c.abs_diff(k) >= min_bars) {
            chosen.push(k);
        }
    }
    chosen.sort_unstable();
    chosen
}

// ---------------------------------------------------------------------------------------
// labels

/// Similarity of two segments (bar ranges) and the bar lag that aligns them best: chroma
/// and onset patterns compared bar by bar (shifted by up to the length difference plus one
/// bar, overlapping all but at most one bar of the shorter segment), mean spectral shape,
/// and mean level. Chroma weighs most and is rescaled from [`CHROMA_FLOOR`], because the
/// melody over the chords is what tells a verse from a chorus on the same progression.
/// Bar `j` of `a` lines up with bar `j + lag` of `b`.
fn segment_similarity(bf: &BarFeatures, ssm: &Ssm, a: (usize, usize), b: (usize, usize)) -> (f64, isize) {
    let (la, lb) = (a.1 - a.0, b.1 - b.0);
    if la == 0 || lb == 0 {
        return (0.0, 0);
    }
    // the shorter segment must overlap the longer one completely, or all but one bar
    let need = la.min(lb).saturating_sub(1).max(1);
    // lags up to the length difference plus one bar, so a section with extra bars in front
    // (an intro or a pickup merged into it) still lines up with its repeat
    let max_lag = (la.abs_diff(lb) + 1).min(16) as isize;
    let mut lags: Vec<isize> = (-max_lag..=max_lag).collect();
    lags.sort_by_key(|l| l.unsigned_abs());
    let mut best = (f64::NEG_INFINITY, 0isize, 0.0, 0.0);
    for lag in lags {
        let (mut c, mut r) = (0.0, 0.0);
        let mut cnt = 0usize;
        for j in 0..la {
            let q = b.0 as isize + j as isize + lag;
            if q < b.0 as isize || q >= b.1 as isize {
                continue;
            }
            c += ssm.chroma[a.0 + j][q as usize];
            r += ssm.rhythm[a.0 + j][q as usize];
            cnt += 1;
        }
        if cnt >= need {
            let (c, r) = (c / cnt as f64, r / cnt as f64);
            // a small preference for the least shifted alignment
            let v = 0.7 * c + 0.3 * r - 0.005 * lag.unsigned_abs() as f64;
            if v > best.0 {
                best = (v, lag, c, r);
            }
        }
    }
    let c = ((best.2 - CHROMA_FLOOR) / (1.0 - CHROMA_FLOOR)).clamp(0.0, 1.0);
    let r = best.3;
    let mean_t = |r: (usize, usize)| -> [f64; N_CEPS] {
        let mut t = [0.0; N_CEPS];
        for bt in &bf.timbre[r.0..r.1] {
            for (s, v) in t.iter_mut().zip(bt) {
                *s += v / (r.1 - r.0) as f64;
            }
        }
        t
    };
    let level = |r: (usize, usize)| -> f64 {
        let p: Vec<f64> = bf.level_db[r.0..r.1].iter().map(|d| 10f64.powf(d / 10.0)).collect();
        util::pow_db(util::mean(&p))
    };
    let t = timbre_sim(&mean_t(a), &mean_t(b), ssm.timbre_scale_db);
    let l = (-((level(a) - level(b)) / LEVEL_SCALE_DB).powi(2)).exp();
    ((0.65 * c + 0.15 * r + 0.15 * t + 0.05 * l).clamp(0.0, 1.0), best.1)
}

/// Average-linkage agglomerative clustering: the most similar pair of clusters merges
/// while its linkage is at least `threshold` and, when a cluster already holds several
/// sections, no more than [`LABEL_GAP`] below how alike its own sections are (a verse at
/// 0.85 to two choruses that match each other at 0.96 stays apart). Ids by first appearance.
fn cluster(sim: &[Vec<f64>], threshold: f64) -> Vec<usize> {
    let n = sim.len();
    let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let cohesion = |c: &[usize]| -> Option<f64> {
        if c.len() < 2 {
            return None;
        }
        let mut s = 0.0;
        let mut k = 0usize;
        for (x, &i) in c.iter().enumerate() {
            for &j in &c[x + 1..] {
                s += sim[i][j];
                k += 1;
            }
        }
        Some(s / k as f64)
    };
    loop {
        let mut best: Option<(f64, usize, usize)> = None;
        for a in 0..clusters.len() {
            for b in a + 1..clusters.len() {
                let mut s = 0.0;
                for &i in &clusters[a] {
                    for &j in &clusters[b] {
                        s += sim[i][j];
                    }
                }
                let link = s / (clusters[a].len() * clusters[b].len()) as f64;
                let reference = [cohesion(&clusters[a]), cohesion(&clusters[b])].into_iter().flatten().reduce(f64::min);
                let allowed = link >= threshold && reference.is_none_or(|r| link >= r - LABEL_GAP);
                if allowed && best.is_none_or(|bst| link > bst.0) {
                    best = Some((link, a, b));
                }
            }
        }
        let Some((_, a, b)) = best else { break };
        let merged = clusters.remove(b);
        clusters[a].extend(merged);
    }
    clusters.sort_by_key(|c| c.iter().copied().min().unwrap_or(0));
    let mut ids = vec![0; n];
    for (rank, c) in clusters.iter().enumerate() {
        for &i in c {
            ids[i] = rank;
        }
    }
    ids
}

// ---------------------------------------------------------------------------------------
// vocal activity and roles

/// Per 50 ms frame of the vocal stem, whether it is active: within 25 dB of its loud
/// level (95th percentile of frame RMS) and above -60 dBFS.
fn vocal_activity_frames(v: &AudioBuffer) -> (Vec<bool>, f64) {
    let sr = v.sample_rate as f64;
    let hop = ((sr * 0.05) as usize).max(1);
    let x = v.mono_mix();
    let db: Vec<f64> = x
        .chunks(hop)
        .map(|c| util::pow_db(c.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / c.len() as f64))
        .collect();
    if db.is_empty() {
        return (Vec::new(), hop as f64 / sr);
    }
    let thr = (util::percentile(&db, 95.0) - 25.0).max(-60.0);
    (db.iter().map(|d| *d > thr).collect(), hop as f64 / sr)
}

fn activity_between(act: &[bool], hop_s: f64, a: f64, b: f64) -> f64 {
    let lo = ((a / hop_s).floor().max(0.0) as usize).min(act.len());
    let hi = ((b / hop_s).floor().max(0.0) as usize).clamp(lo, act.len());
    if hi == lo {
        return 0.0;
    }
    act[lo..hi].iter().filter(|x| **x).count() as f64 / (hi - lo) as f64
}

/// Heuristic roles. The repeating label with the highest mean loudness (sung, when a vocal
/// stem is given) is the chorus; another repeating (sung) label that first appears before
/// the chorus is the verse; a short distinct section right before each chorus is a
/// pre-chorus; a unique section in the second half between choruses is the bridge; a
/// distinct (quieter or unsung) first section is the intro and last one the outro; an
/// unsung section that is none of these is instrumental; anything else is `section`.
fn assign_roles(secs: &mut [Section], duration_s: f64) {
    let n = secs.len();
    if n == 0 {
        return;
    }
    let has_vocal = secs.iter().any(|s| s.vocal_activity.is_some());
    let va: Vec<f64> = secs.iter().map(|s| s.vocal_activity.unwrap_or(0.0)).collect();
    let lufs: Vec<f64> = secs.iter().map(|s| if s.lufs.is_finite() { s.lufs } else { -70.0 }).collect();
    let median_lufs = util::median(&lufs);
    let mut members: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, s) in secs.iter().enumerate() {
        match members.iter_mut().find(|m| m.0 == s.label) {
            Some(m) => m.1.push(i),
            None => members.push((s.label.clone(), vec![i])),
        }
    }
    let count = |i: usize| members.iter().find(|m| m.0 == secs[i].label).map(|m| m.1.len()).unwrap_or(1);
    let mean_of = |v: &[f64], idx: &[usize]| util::mean(&idx.iter().map(|&i| v[i]).collect::<Vec<f64>>());
    let mut role: Vec<Option<(&'static str, f64)>> = vec![None; n];

    // chorus: the loudest repeating label (sung, when a vocal stem is given); a song that is
    // one label throughout has nothing to contrast it with
    let repeating: Vec<&(String, Vec<usize>)> = members.iter().filter(|m| m.1.len() >= 2 && m.1.len() < n).collect();
    let sung: Vec<&(String, Vec<usize>)> =
        repeating.iter().copied().filter(|m| !has_vocal || mean_of(&va, &m.1) >= SUNG).collect();
    let pool = if sung.is_empty() { repeating.clone() } else { sung };
    let score = |m: &(String, Vec<usize>)| mean_of(&lufs, &m.1) + 0.3 * (m.1.len().min(4) as f64 - 2.0);
    let chorus =
        pool.iter().copied().max_by(|a, b| score(a).partial_cmp(&score(b)).unwrap_or(std::cmp::Ordering::Equal));
    let mut chorus_label = None;
    let mut chorus_bars = 0;
    if let Some(ch) = chorus {
        let ch_lufs = mean_of(&lufs, &ch.1);
        let others: Vec<f64> = repeating.iter().filter(|m| m.0 != ch.0).map(|m| mean_of(&lufs, &m.1)).collect();
        let margin = if let Some(next) = others.iter().copied().reduce(f64::max) {
            ch_lufs - next
        } else {
            let rest: Vec<f64> = (0..n).filter(|i| !ch.1.contains(i)).map(|i| lufs[i]).collect();
            if rest.is_empty() {
                0.0
            } else {
                ch_lufs - util::median(&rest)
            }
        };
        let mut conf = (0.4 + 0.08 * margin).clamp(0.25, 0.8);
        if has_vocal && mean_of(&va, &ch.1) >= SUNG {
            conf += 0.1;
        }
        for &i in &ch.1 {
            role[i] = Some(("chorus", conf.min(0.9)));
        }
        chorus_label = Some(ch.0.clone());
        chorus_bars = ch.1.iter().map(|&i| secs[i].bars).max().unwrap_or(0);
    }
    let is_chorus = |i: usize| chorus_label.as_deref() == Some(secs[i].label.as_str());
    let first_chorus = (0..n).find(|&i| is_chorus(i));

    // verse: another repeating (sung) label, preferably first heard before the chorus
    if let Some(fc) = first_chorus {
        let cands: Vec<&(String, Vec<usize>)> = repeating
            .iter()
            .copied()
            .filter(|m| Some(&m.0) != chorus_label.as_ref())
            .filter(|m| !has_vocal || mean_of(&va, &m.1) >= SUNG)
            .collect();
        let before: Vec<&(String, Vec<usize>)> = cands.iter().copied().filter(|m| m.1[0] < fc).collect();
        fn pick<'a>(v: &[&'a (String, Vec<usize>)]) -> Option<&'a (String, Vec<usize>)> {
            v.iter().copied().max_by_key(|m| (m.1.len(), std::cmp::Reverse(m.1[0])))
        }
        let (verse, precedes) = match pick(&before) {
            Some(v) => (Some(v), true),
            None if has_vocal => (pick(&cands), false),
            None => (None, false),
        };
        if let Some(v) = verse {
            let chorus_idx: Vec<usize> = (0..n).filter(|&i| is_chorus(i)).collect();
            let gap = mean_of(&lufs, &chorus_idx) - mean_of(&lufs, &v.1);
            let mut conf = 0.35 + 0.05 * gap.clamp(0.0, 4.0);
            if precedes {
                conf += 0.1;
            }
            if has_vocal {
                conf += 0.15;
            }
            for &i in &v.1 {
                role[i] = Some(("verse", conf.min(0.85)));
            }
        }
    }
    // no repeating verse: the section right before the first chorus (after the opening
    // one), quieter than the chorus and sung when a vocal stem is given, is taken as the verse
    if let Some(fc) = first_chorus {
        let has_verse = role.iter().any(|r| r.map(|r| r.0) == Some("verse"));
        if !has_verse && fc >= 2 && role[fc - 1].is_none() {
            let i = fc - 1;
            let chorus_lufs = mean_of(&lufs, &(0..n).filter(|&k| is_chorus(k)).collect::<Vec<usize>>());
            if lufs[i] < chorus_lufs && (!has_vocal || va[i] >= SUNG) {
                role[i] = Some(("verse", if has_vocal { 0.45 } else { 0.35 }));
            }
        }
    }
    let label_role = |role: &[Option<(&'static str, f64)>], i: usize| role[i].map(|r| r.0);

    // intro and outro
    let quiet = |i: usize| lufs[i] < median_lufs - 1.0;
    let unsung = |i: usize| has_vocal && va[i] < UNSUNG;
    let edge_conf = |i: usize| {
        let mut c = 0.45 + 0.05 * (median_lufs - lufs[i]).clamp(0.0, 4.0);
        if unsung(i) {
            c += 0.15;
        }
        c.min(0.85)
    };
    // an edge section is the intro or outro when its label is its own (and it is not sung
    // throughout), when the vocal is silent there, or when it is quieter than most and its
    // label (an intro riff that comes back) has no other role
    let edge = |role: &[Option<(&'static str, f64)>], i: usize| {
        label_role(role, i) != Some("chorus")
            && ((count(i) == 1 && (!has_vocal || va[i] < SUNG)) || unsung(i) || (role[i].is_none() && quiet(i)))
    };
    if n >= 2 && edge(&role, 0) {
        role[0] = Some(("intro", edge_conf(0)));
    }
    if n >= 3 && edge(&role, n - 1) {
        role[n - 1] = Some(("outro", edge_conf(n - 1)));
    }

    // pre-chorus: a distinct, short label whose every occurrence sits between a verse and
    // a chorus
    if chorus_label.is_some() {
        for m in &members {
            let idx: Vec<usize> = m.1.iter().copied().filter(|&i| role[i].is_none()).collect();
            if idx.is_empty() || idx.len() != m.1.len() {
                continue;
            }
            let between = idx
                .iter()
                .all(|&i| i > 0 && i + 1 < n && is_chorus(i + 1) && label_role(&role, i - 1) == Some("verse"));
            let short = idx.iter().all(|&i| secs[i].bars <= chorus_bars.max(4) && secs[i].bars <= 8);
            if between && short {
                let conf = if idx.len() >= 2 { 0.6 } else { 0.45 };
                for &i in &idx {
                    role[i] = Some(("pre-chorus", conf));
                }
            }
        }
    }

    // bridge or instrumental break: a unique section in the second half between choruses
    for i in 1..n.saturating_sub(1) {
        if role[i].is_some() || count(i) != 1 {
            continue;
        }
        let before = (0..i).any(is_chorus);
        let after = (i + 1..n).any(is_chorus);
        if before && after && secs[i].start_s >= 0.4 * duration_s {
            role[i] =
                Some(if unsung(i) { ("instrumental", 0.6) } else { ("bridge", if has_vocal { 0.55 } else { 0.45 }) });
        }
    }

    // anything else: instrumental when the vocal stem is silent there
    for i in 0..n {
        if role[i].is_none() && unsung(i) {
            role[i] = Some(("instrumental", (0.5 + 2.0 * (UNSUNG - va[i])).min(0.8)));
        }
    }
    for (s, r) in secs.iter_mut().zip(role) {
        let (name, conf) = r.unwrap_or(("section", 0.2));
        s.role = name.to_string();
        s.role_confidence = conf;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::f64::consts::PI;

    pub(crate) const SR: u32 = 22050;
    pub(crate) const BPM: f64 = 120.0;

    fn midi_hz(m: f64) -> f64 {
        440.0 * 2f64.powf((m - 69.0) / 12.0)
    }

    /// Chords (MIDI notes, one per bar, cycling), voice harmonics `(k, amp)`, chord level,
    /// whether hats play, and the vocal melody (one MIDI note per half bar, cycling).
    struct Part {
        chords: &'static [[u8; 3]],
        harmonics: &'static [(f64, f64)],
        amp: f64,
        hats: bool,
        tremolo: bool,
        melody: &'static [u8],
    }

    const SINE: &[(f64, f64)] = &[(1.0, 1.0)];
    const SAW: &[(f64, f64)] = &[(1.0, 1.0), (2.0, 0.5), (3.0, 0.33), (4.0, 0.25), (5.0, 0.2), (6.0, 0.17)];
    const SQUARE: &[(f64, f64)] = &[(1.0, 1.0), (3.0, 0.33), (5.0, 0.2), (7.0, 0.14)];

    fn part(letter: char) -> Part {
        match letter {
            // I - vi in C, soft sines
            'A' => Part {
                chords: &[[60, 64, 67], [57, 60, 64]],
                harmonics: SINE,
                amp: 0.05,
                hats: false,
                tremolo: false,
                melody: &[72, 76, 69, 72],
            },
            // IV - V, loud saws with hats
            'B' => Part {
                chords: &[[53, 57, 60], [55, 59, 62]],
                harmonics: SAW,
                amp: 0.07,
                hats: true,
                tremolo: false,
                melody: &[77, 81, 79, 74],
            },
            // bVI - bVII, square tones with tremolo
            _ => Part {
                chords: &[[56, 60, 63], [58, 62, 65]],
                harmonics: SQUARE,
                amp: 0.05,
                hats: false,
                tremolo: true,
                melody: &[75, 80, 77, 82],
            },
        }
    }

    /// A synthetic song at 120 BPM in 4/4: a kick on every beat (accented on beat 1) and,
    /// per section letter, its own chords and timbre. Returns the mix (vocal included) and
    /// the vocal stem: tone bursts with vibrato in the sections whose letter is in `sung`.
    pub(crate) fn synth_song(arrangement: &str, bars_per_section: usize, sung: &str) -> (AudioBuffer, AudioBuffer) {
        synth_song_with(arrangement, bars_per_section, sung, true, 0.0)
    }

    /// [`synth_song`] with or without the louder kick on beat 1, delayed by `lead_s` of silence.
    fn synth_song_with(
        arrangement: &str,
        bars_per_section: usize,
        sung: &str,
        accent: bool,
        lead_s: f64,
    ) -> (AudioBuffer, AudioBuffer) {
        let sr = SR as f64;
        let bar_s = 4.0 * 60.0 / BPM;
        let total = arrangement.len() * bars_per_section;
        let lead = (lead_s * sr) as usize;
        let n = (total as f64 * bar_s * sr) as usize;
        let mut x = vec![0.0f32; n];
        let mut v = vec![0.0f32; n];
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        };
        for (si, letter) in arrangement.chars().enumerate() {
            let p = part(letter);
            for bar in 0..bars_per_section {
                let bar_idx = si * bars_per_section + bar;
                let t0 = bar_idx as f64 * bar_s;
                let s0 = (t0 * sr) as usize;
                let len = (bar_s * sr) as usize;
                let chord = p.chords[bar % p.chords.len()];
                for i in 0..len.min(n - s0) {
                    let t = i as f64 / sr;
                    let env = (t / 0.01).min(1.0) * ((bar_s - t) / 0.02).clamp(0.0, 1.0);
                    let trem = if p.tremolo { 0.7 + 0.3 * (2.0 * PI * 6.0 * t).sin() } else { 1.0 };
                    let mut s = 0.0;
                    for &note in &chord {
                        let f = midi_hz(note as f64);
                        for &(k, a) in p.harmonics {
                            s += a * (2.0 * PI * f * k * t).sin();
                        }
                    }
                    // bass: chord root two octaves down
                    s += 1.5 * (2.0 * PI * midi_hz(chord[0] as f64 - 24.0) * t).sin();
                    x[s0 + i] += (p.amp * env * trem * s) as f32;
                }
                for beat in 0..4 {
                    let bt = t0 + beat as f64 * 60.0 / BPM;
                    let bs = (bt * sr) as usize;
                    // kick: a falling sine
                    let amp = if beat == 0 && accent { 0.6 } else { 0.35 };
                    let mut ph = 0.0;
                    for i in 0..((0.3 * sr) as usize).min(n.saturating_sub(bs)) {
                        let t = i as f64 / sr;
                        ph += 2.0 * PI * (50.0 + 90.0 * (-t / 0.03).exp()) / sr;
                        let fade = ((0.3 - t) / 0.05).clamp(0.0, 1.0);
                        x[bs + i] += (amp * fade * (-t / 0.08).exp() * ph.sin()) as f32;
                    }
                    if p.hats {
                        for half in 0..2 {
                            let hs = ((bt + half as f64 * 30.0 / BPM) * sr) as usize;
                            let mut prev = 0.0;
                            for i in 0..((0.06 * sr) as usize).min(n.saturating_sub(hs)) {
                                let t = i as f64 / sr;
                                let r = rnd();
                                x[hs + i] += (0.12 * (-t / 0.015).exp() * (r - prev)) as f32;
                                prev = r;
                            }
                        }
                    }
                }
                if sung.contains(letter) {
                    for half in 0..2 {
                        let note = p.melody[(bar * 2 + half) % p.melody.len()] as f64;
                        let f = midi_hz(note);
                        let hs = ((t0 + half as f64 * bar_s / 2.0) * sr) as usize;
                        let len = (0.8 * sr) as usize;
                        let mut ph = 0.0;
                        for i in 0..len.min(n.saturating_sub(hs)) {
                            let t = i as f64 / sr;
                            ph += 2.0 * PI * f * (1.0 + 0.006 * (2.0 * PI * 5.5 * t).sin()) / sr;
                            let env = (t / 0.03).min(1.0) * ((0.8 - t) / 0.05).clamp(0.0, 1.0);
                            let s = 0.12 * env * (ph.sin() + 0.4 * (2.0 * ph).sin() + 0.2 * (3.0 * ph).sin());
                            v[hs + i] += s as f32;
                        }
                    }
                }
            }
        }
        for (a, b) in x.iter_mut().zip(&v) {
            *a += *b;
        }
        let delay = |s: Vec<f32>| -> AudioBuffer {
            let mut d = vec![0.0f32; lead];
            d.extend(s);
            AudioBuffer::from_mono(SR, d)
        };
        (delay(x), delay(v))
    }

    fn labels(r: &SectionsReport) -> String {
        r.sections.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join("")
    }

    fn summary(r: &SectionsReport) -> String {
        let s: Vec<String> = r
            .sections
            .iter()
            .map(|s| {
                format!(
                    "{}@{:.1}s({} bars, {:.1} LUFS, {}, va {:?})",
                    s.label, s.start_s, s.bars, s.lufs, s.role, s.vocal_activity
                )
            })
            .collect();
        format!(
            "tempo {:.2} ({}, conf {:.2}) grid {} bpb {} conf {:.2}: {}",
            r.tempo_bpm,
            r.tempo_source,
            r.tempo_confidence,
            r.grid,
            r.beats_per_bar,
            r.confidence,
            s.join(" ")
        )
    }

    #[test]
    fn finds_the_structure_of_a_synthetic_song() {
        let (mix, _) = synth_song("ABABCB", 8, "");
        let r = sections(&mix, None, &SectionOptions::default());
        eprintln!("{}", summary(&r));
        let bar_s = 2.0;
        assert!((r.tempo_bpm - 120.0).abs() < 1.0, "{}", summary(&r));
        assert_eq!(r.tempo_source, "detected");
        assert_eq!(r.beats_per_bar, 4);
        assert!((r.bar_s - bar_s).abs() < 0.05);
        assert_eq!(r.boundaries_s.len(), 5, "{}", summary(&r));
        for (k, b) in r.boundaries_s.iter().enumerate() {
            let truth = (k + 1) as f64 * 8.0 * bar_s;
            assert!((b - truth).abs() <= bar_s + 0.05, "boundary {b} vs {truth}: {}", summary(&r));
            assert!((b - truth).abs() <= 0.1, "boundary {b} vs {truth}: {}", summary(&r));
        }
        assert_eq!(labels(&r), "ABABCB", "{}", summary(&r));
        let roles: Vec<&str> = r.sections.iter().map(|s| s.role.as_str()).collect();
        assert_eq!(roles[1], "chorus", "{}", summary(&r));
        assert_eq!(roles[3], "chorus");
        assert_eq!(roles[5], "chorus");
        assert_eq!(roles[0], "verse", "{}", summary(&r));
        assert_eq!(roles[4], "bridge", "{}", summary(&r));
        assert_eq!(r.labels["B"].role, "chorus");
        assert_eq!(r.labels["B"].count, 3);
        assert_eq!(r.sections[2].repeat_of, Some(0));
        assert_eq!(r.sections[5].repeat_of, Some(1));
        assert!(r.sections[0].repeat_of.is_none());
        assert!(r.sections[2].similarity_to_repeat.unwrap() > 0.8);
        let find = |a: usize, b: usize| r.repeats.iter().find(|p| p.a == a && p.b == b);
        for (a, b, off) in [(0, 2, 32.0), (1, 3, 32.0), (1, 5, 64.0), (3, 5, 32.0)] {
            let p = find(a, b).unwrap_or_else(|| panic!("repeat {a}-{b} missing: {:?}", r.repeats));
            assert!((p.offset_s - off).abs() <= bar_s + 0.05, "{p:?}");
        }
        assert_eq!(r.repeats.len(), 4);
        assert!(r.sections[1].delta_lu > r.sections[0].delta_lu);
        assert!(r.confidence > 0.6, "{}", r.confidence);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn bars_follow_chord_changes_without_a_downbeat_accent() {
        // every kick equal, and the song starts after 1.5 beats of silence: bars come from
        // the beat phase where the chords change, and the lead-in belongs to the first section
        let (mix, _) = synth_song_with("ABAB", 8, "", false, 0.75);
        let r = sections(&mix, None, &SectionOptions::default());
        eprintln!("{}", summary(&r));
        assert_eq!(labels(&r), "ABAB", "{}", summary(&r));
        assert_eq!(r.sections[0].start_s, 0.0);
        for (k, b) in r.boundaries_s.iter().enumerate() {
            let truth = 0.75 + (k + 1) as f64 * 16.0;
            assert!((b - truth).abs() <= 0.1, "boundary {b} vs {truth}: {}", summary(&r));
        }
        let rep = r.repeats.iter().find(|p| p.a == 0 && p.b == 2).expect("A repeats");
        assert!((rep.offset_s - 32.0).abs() < 0.1, "{rep:?}");
    }

    #[test]
    fn every_other_beat_keeps_the_stronger_parity() {
        // loud clicks on odd quarter beats, soft ones in between
        let sr = SR as usize;
        let mut x = vec![0.0f32; sr * 8];
        let beats: Vec<f64> = (0..32).map(|i| 0.25 * i as f64).collect();
        for (i, t) in beats.iter().enumerate() {
            x[(t * sr as f64) as usize] = if i % 2 == 1 { 0.9 } else { 0.2 };
        }
        let slow = every_other_beat(&AudioBuffer::from_mono(SR, x), &beats);
        assert_eq!(slow.len(), 16);
        assert!((slow[0] - 0.25).abs() < 1e-9, "{slow:?}");
    }

    fn plain_sections(labels: &str, lufs: &[f64]) -> Vec<Section> {
        labels
            .chars()
            .enumerate()
            .map(|(i, l)| Section {
                index: i,
                label: l.to_string(),
                start_s: i as f64 * 10.0,
                end_s: (i + 1) as f64 * 10.0,
                start_bar: 1 + 5 * i,
                bars: 5,
                lufs: lufs[i],
                delta_lu: 0.0,
                vocal_activity: None,
                repeat_of: None,
                similarity_to_repeat: None,
                role: String::new(),
                role_confidence: 0.0,
            })
            .collect()
    }

    #[test]
    fn one_label_throughout_has_no_chorus() {
        let mut secs = plain_sections("AAA", &[-14.0; 3]);
        assign_roles(&mut secs, 30.0);
        assert!(secs.iter().all(|s| s.role == "section"), "{secs:?}");
    }

    #[test]
    fn a_lone_section_before_the_first_chorus_is_the_verse() {
        // intro, verse (its own label), chorus, break, chorus
        let mut secs = plain_sections("ABCDC", &[-26.0, -20.0, -18.5, -20.5, -18.5]);
        assign_roles(&mut secs, 50.0);
        let roles: Vec<&str> = secs.iter().map(|s| s.role.as_str()).collect();
        assert_eq!(roles, ["intro", "verse", "chorus", "bridge", "chorus"], "{secs:?}");
    }

    #[test]
    fn given_tempo_is_used() {
        let (mix, _) = synth_song("ABAB", 4, "");
        let opts = SectionOptions { bpm: Some(120.0), min_bars: 2, ..Default::default() };
        let r = sections(&mix, None, &opts);
        eprintln!("{}", summary(&r));
        assert_eq!(r.tempo_source, "given");
        assert_eq!(r.tempo_bpm, 120.0);
        assert_eq!(labels(&r), "ABAB", "{}", summary(&r));
        for (k, b) in r.boundaries_s.iter().enumerate() {
            let truth = (k + 1) as f64 * 8.0;
            assert!((b - truth).abs() <= 2.05, "{}", summary(&r));
        }
    }

    #[test]
    fn vocal_activity_and_roles() {
        let (mix, vocal) = synth_song("ABABCB", 8, "AB");
        let r = sections(&mix, Some(&vocal), &SectionOptions::default());
        eprintln!("{}", summary(&r));
        assert_eq!(labels(&r), "ABABCB", "{}", summary(&r));
        for s in &r.sections {
            let va = s.vocal_activity.unwrap();
            if s.label == "C" {
                assert!(va < 0.05, "{}", summary(&r));
                assert!(s.role == "instrumental" || s.role == "bridge", "{}", summary(&r));
            } else {
                assert!(va > 0.5, "{}", summary(&r));
            }
        }
        assert_eq!(r.sections[1].role, "chorus");
        assert_eq!(r.sections[0].role, "verse");
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        // a vocal stem of another length
        let short = vocal.slice(0, vocal.frames() - SR as usize);
        let r = sections(&mix, Some(&short), &SectionOptions::default());
        assert!(r.warnings.iter().any(|w| w.code == "vocal_length_differs"), "{:?}", r.warnings);
    }

    #[test]
    fn uniform_texture_is_one_label_with_low_confidence() {
        let (mix, _) = synth_song("AAAAAA", 8, "");
        let r = sections(&mix, None, &SectionOptions::default());
        eprintln!("{}", summary(&r));
        assert!(r.sections.len() <= 2, "{}", summary(&r));
        assert_eq!(r.labels.len(), 1, "{}", summary(&r));
        assert!(r.confidence < 0.3, "{}", summary(&r));
    }

    #[test]
    fn short_file_is_one_section() {
        let (mix, _) = synth_song("AB", 2, "");
        let r = sections(&mix, None, &SectionOptions::default());
        assert_eq!(r.sections.len(), 1);
        assert_eq!(r.sections[0].start_s, 0.0);
        assert!((r.sections[0].end_s - 8.0).abs() < 1e-6);
        assert!(r.warnings.iter().any(|w| w.code == "too_short"), "{:?}", r.warnings);
        assert!(!r.warnings.iter().any(|w| w.code == "no_repeats"));
    }

    #[test]
    fn silence_noise_and_empty_input() {
        let r = sections(&AudioBuffer::new(SR, 1, 0), None, &SectionOptions::default());
        assert!(r.sections.is_empty());
        assert_eq!(r.confidence, 0.0);
        let r = sections(&AudioBuffer::new(SR, 2, SR as usize * 20), None, &SectionOptions::default());
        assert_eq!(r.grid, "seconds");
        assert_eq!(r.sections.len(), 1);
        assert!(r.sections[0].lufs.is_infinite());
        assert!(r.warnings.iter().any(|w| w.code == "low_tempo_confidence"));
        let nz = ferraus_core::testing::noise(SR, 30.0, 0.3, 4);
        let r = sections(&nz, None, &SectionOptions::default());
        assert!(r.confidence < 0.3, "{}", summary(&r));
    }

    #[test]
    fn grid_helpers() {
        let b = complete_beats(&[1.0, 1.5, 2.0, 3.0, 3.5], 0.5, 4.2);
        let want = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0];
        assert_eq!(b.len(), want.len(), "{b:?}");
        for (x, y) in b.iter().zip(want) {
            assert!((x - y).abs() < 1e-9, "{b:?}");
        }
        let g = strict_grid(&[0.3, 0.8, 1.3], 0.5, 2.0);
        assert!((g[0] - 0.3).abs() < 1e-9 && g.len() == 4, "{g:?}");
        assert_eq!(label_name(0), "A");
        assert_eq!(label_name(27), "B1");
        assert_eq!(cluster(&[vec![1.0, 0.9, 0.2], vec![0.9, 1.0, 0.1], vec![0.2, 0.1, 1.0]], 0.8), vec![0, 0, 1]);
        // a verse at 0.85 to two choruses that match each other at 0.96 keeps its own label
        let s = [vec![1.0, 0.85, 0.85], vec![0.85, 1.0, 0.96], vec![0.85, 0.96, 1.0]];
        assert_eq!(cluster(&s, 0.8), vec![0, 1, 1]);
        // within the gap it joins
        let s = [vec![1.0, 0.9, 0.9], vec![0.9, 1.0, 0.96], vec![0.9, 0.96, 1.0]];
        assert_eq!(cluster(&s, 0.8), vec![0, 0, 0]);
    }
}
