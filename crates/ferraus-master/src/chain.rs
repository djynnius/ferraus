//! The automatic mastering chain (PRD steps 1–6). The planner *simulates* the chain on the
//! actual input: every stage is resolved from measurements of the signal as it arrives at
//! that stage, run through the registry exactly as `master --plan` will run it, and
//! recorded. The result is a plan whose every parameter is explicit, plus the processed
//! audio (so `master` does not have to run the chain twice).

use crate::genres::{self, GenreData};
use crate::plan::{run_stage, run_stage_filtered, GenreChoice, IntensityChoice, Plan, Stage, StageTrace, PLAN_VERSION};
use crate::targets::Target;
use crate::util::{fmt_db, fmt_freq, fmt_pct, round_to};
use ferraus_analysis::spectrum::{self, BandLevel};
use ferraus_analysis::{loudness, stereo, MasteringAnalysis};
use ferraus_core::prelude::*;
use ferraus_dsp::biquad::{Coeffs, FilterType, MultibandSplitter};
use serde::Serialize;
use serde_json::Map;

/// Every correction of the corrective EQ is capped at this many dB per band (PRD).
pub const MAX_EQ_DB: f64 = 4.0;
/// Broadband correlation the stereo stage refuses to go below (PRD), plus a small margin
/// for the later nonlinear stages.
pub const MIN_CORRELATION: f64 = 0.3;
const CORRELATION_MARGIN: f64 = 0.03;
/// Q of a 1/3-octave graphic EQ band.
const THIRD_OCT_Q: f64 = 4.318;
/// Multiband crossovers (the `multiband --bands 4` defaults).
pub const CROSSOVERS: [f64; 3] = [120.0, 2000.0, 10000.0];

/// User choices for `master` / `master plan`.
#[derive(Clone, Debug)]
pub struct Options {
    pub target: Target,
    /// `auto` or a genre name.
    pub genre: String,
    /// Explicit `--intensity`, if given.
    pub intensity: Option<String>,
    pub width: f64,
    pub bass_mono: f64,
    pub warmth: f64,
    pub no_eq: bool,
    pub no_widen: bool,
    /// `None` = no head/tail trim.
    pub trim_db: Option<f64>,
    /// Reference track (path for the plan, decoded audio for the measurement).
    pub reference: Option<(String, AudioBuffer)>,
    pub match_amount: f64,
}

/// What the loudness stage achieved.
#[derive(Clone, Debug, Serialize)]
pub struct LoudnessOutcome {
    pub target_lufs: f64,
    pub ceiling_dbtp: f64,
    pub gain_db: f64,
    /// Largest limiter gain reduction used (dB).
    pub limiter_gain_reduction_db: f64,
    /// The intensity's bound on limiter gain reduction (dB).
    pub limiter_bound_db: f64,
    pub landed_lufs: f64,
    pub true_peak_dbtp: f64,
    pub reached: bool,
    /// Target minus landed loudness (LU), when the target was not reached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortfall_lu: Option<f64>,
}

/// The planner's output.
pub struct Built {
    pub plan: Plan,
    pub output: AudioBuffer,
    pub trace: Vec<StageTrace>,
    pub loudness: LoudnessOutcome,
    pub declip: Option<String>,
}

/// Limiter gain-reduction bound for an intensity (light 2, medium 4, heavy 6 dB).
pub fn intensity_bound(name: &str) -> f64 {
    match name {
        "light" => 2.0,
        "heavy" => 6.0,
        _ => 4.0,
    }
}

fn multiband_ratio(intensity: &str) -> f64 {
    match intensity {
        "light" => 1.5,
        "heavy" => 2.0,
        _ => 1.75,
    }
}

fn depth_db(intensity: &str) -> f64 {
    match intensity {
        "light" => 2.0,
        "heavy" => 4.0,
        _ => 3.0,
    }
}

/// Resolve the intensity: explicit, or `light` when the input already looks mastered.
pub fn resolve_intensity(explicit: Option<&str>, m: &MasteringAnalysis) -> IntensityChoice {
    if let Some(n) = explicit {
        return IntensityChoice {
            name: n.into(),
            limiter_max_gain_reduction_db: intensity_bound(n),
            source: "explicit".into(),
            why: "given with --intensity".into(),
        };
    }
    let h = &m.dynamics.mastered;
    if h.looks_mastered {
        IntensityChoice {
            name: "light".into(),
            limiter_max_gain_reduction_db: intensity_bound("light"),
            source: "looks_mastered".into(),
            why: format!(
                "the input already looks mastered ({}), so master defaults to light intensity to avoid squashing it further",
                h.reasons.join("; ")
            ),
        }
    } else {
        IntensityChoice {
            name: "medium".into(),
            limiter_max_gain_reduction_db: intensity_bound("medium"),
            source: "default".into(),
            why: "the default; the input does not look mastered".into(),
        }
    }
}

/// Resolve the genre: explicit, or the `auto` heuristic.
pub fn resolve_genre(arg: &str, m: &MasteringAnalysis) -> GenreChoice {
    if arg != "auto" && genres::genre(arg).is_some() {
        return GenreChoice { name: arg.into(), source: "explicit".into(), reasons: vec!["given with --genre".into()] };
    }
    let g = genres::guess(m);
    GenreChoice { name: g.genre, source: "auto".into(), reasons: g.reasons }
}

struct Sim<'c, 'a> {
    ctx: &'c mut OpCtx<'a>,
    stages: Vec<Stage>,
    trace: Vec<StageTrace>,
}

impl Sim<'_, '_> {
    fn run(&mut self, stage: Stage, buf: AudioBuffer) -> Result<(AudioBuffer, Map<String, Value>)> {
        let (out, metrics) = run_stage(self.ctx, &stage, buf)?;
        self.trace.push(StageTrace { op: stage.op.clone(), metrics: metrics.clone() });
        self.stages.push(stage);
        Ok((out, metrics))
    }

    fn has_op(&self, name: &str) -> bool {
        self.ctx.registry.and_then(|r| r.get(name)).map(|s| s.is_transform()).unwrap_or(false)
    }
}

/// Build the plan by simulating the chain on `input`.
pub fn build(ctx: &mut OpCtx, input: AudioBuffer, m: &MasteringAnalysis, opts: &Options) -> Result<Built> {
    let genre_choice = resolve_genre(&opts.genre, m);
    let genre = genres::genre(&genre_choice.name).unwrap_or(&genres::GENRES[0]);
    let intensity = resolve_intensity(opts.intensity.as_deref(), m);
    let mut notes: Vec<String> = Vec::new();
    if intensity.source == "looks_mastered" {
        notes.push(intensity.why.clone());
    }
    let mut sim = Sim { ctx, stages: Vec::new(), trace: Vec::new() };

    // 1. pre-flight
    let (pre, declip) = preflight(&mut sim, input, m, opts, &mut notes)?;
    if !loudness::integrated_lufs(&pre).is_finite() {
        return Err(Error::input(
            "the mix is silent or shorter than 400 ms after trimming; there is nothing to master",
        ));
    }
    let n_pre = sim.stages.len();

    // 2-5: corrective EQ, speech, dynamics, stereo, color — simulated, then refined once
    // if the later stages drifted the tonal balance away from what the EQ intended
    let speech = opts.target.is_speech() || genre.name == "podcast";
    let mut rest = middle(&mut sim, pre.clone(), opts, genre, &intensity, m, speech, None, &mut notes)?;
    if let Some(eqp) = &rest.eq {
        let drift = tonal_drift(&rest.buf, eqp);
        let worst = drift.iter().zip(&eqp.valid_mid).filter(|(_, v)| **v).map(|(d, _)| d.abs()).fold(0.0, f64::max);
        if worst > 0.75 {
            sim.stages.truncate(n_pre);
            sim.trace.truncate(n_pre);
            let mut notes2 = Vec::new();
            let refined = middle(&mut sim, pre, opts, genre, &intensity, m, speech, Some(&drift), &mut notes2)?;
            notes.extend(notes2);
            notes.push(format!(
                "corrective EQ refined once: the dynamics and color stages moved the tonal balance by up to {worst:.1} dB, so the EQ pre-compensates (still capped at {MAX_EQ_DB} dB per band)"
            ));
            rest = refined;
        }
    }
    let pre_loud = rest.buf;

    // 6. loudness
    let (output, outcome) = loudness_stage(&mut sim, pre_loud.clone(), &opts.target, &intensity, rest.release_base)?;
    if !outcome.reached {
        let msg = format!(
            "landed at {:.1} LUFS, {:.1} LU short of the {:.1} LUFS target: --intensity {} allows at most {:.0} dB of limiter gain reduction and reaching the target needs more. Use --intensity heavy, or accept the quieter master",
            outcome.landed_lufs,
            outcome.shortfall_lu.unwrap_or(0.0),
            outcome.target_lufs,
            intensity.name,
            outcome.limiter_bound_db
        );
        notes.push(msg.clone());
        sim.ctx.warn("target_not_reached", msg);
    }
    let expected = json!({
        "integrated_lufs": round_to(outcome.landed_lufs, 2),
        "true_peak_dbtp": round_to(outcome.true_peak_dbtp, 2),
        "limiter_gain_reduction_db": round_to(outcome.limiter_gain_reduction_db, 2),
        "limiter_bound_db": outcome.limiter_bound_db,
        "pre_limiter_lufs": round_to(loudness::integrated_lufs(&pre_loud), 2),
        "pre_limiter_true_peak_dbtp": round_to(loudness::true_peak_db(&pre_loud), 2),
        "correlation": round_to(stereo::stereo_analysis(&output).correlation, 3),
        "duration_s": round_to(output.duration_s(), 3),
    });
    let plan = Plan {
        version: PLAN_VERSION,
        target: opts.target.clone(),
        genre: genre_choice,
        intensity,
        reference: opts.reference.as_ref().map(|r| r.0.clone()),
        match_amount: Some(round_to(opts.match_amount, 3)),
        stages: sim.stages,
        notes,
        expected: Some(expected),
    };
    Ok(Built { plan, output, trace: sim.trace, loudness: outcome, declip })
}

fn preflight(
    sim: &mut Sim,
    input: AudioBuffer,
    m: &MasteringAnalysis,
    opts: &Options,
    notes: &mut Vec<String>,
) -> Result<(AudioBuffer, Option<String>)> {
    let mut buf = input;
    let mut declip = None;
    if let Some(p) = m.problems.problems.iter().find(|p| p.problem == "clipping") {
        if sim.has_op("declip") {
            let why = format!("analyze problems found clipping ({}); declip runs first", p.details);
            buf = sim.run(Stage::new("preflight", "declip", vec![], why.clone()), buf)?.0;
            notes.push(format!("clipping found: declip ran first ({})", p.details));
            declip = Some(why);
        } else {
            let msg = format!(
                "clipping found ({}) but declip is not available in this build, so the clipped peaks remain",
                p.details
            );
            sim.ctx.warn("declip_unavailable", msg.clone());
            notes.push(msg.clone());
            declip = Some(msg);
        }
    }
    if let Some(sr) = opts.target.sample_rate {
        if sr != buf.sample_rate {
            let why = format!(
                "the {} target delivers at {sr} Hz; resampling first keeps the limiter's true-peak guarantee",
                opts.target.name
            );
            buf = sim.run(Stage::new("preflight", "resample", vec![("to", sr.to_string())], why), buf)?.0;
        }
    }
    buf = sim
        .run(
            Stage::new("preflight", "dc-remove", vec![("freq", "5Hz".into())], "remove DC offset (5 Hz high-pass)"),
            buf,
        )?
        .0;
    if let Some(t) = opts.trim_db {
        let why = format!("trim head and tail silence below {t} dBFS");
        buf = sim
            .run(
                Stage::new("preflight", "silence trim", vec![("threshold", fmt_db(t)), ("keep", "10ms".into())], why),
                buf,
            )?
            .0;
    }
    Ok((buf, declip))
}

/// The intended corrective EQ: per 1/3-octave band, the correction (dB) the EQ applies.
#[derive(Clone, Debug)]
pub struct EqIntent {
    pub centers: Vec<f64>,
    /// Post-preflight band levels.
    pub levels: Vec<f64>,
    pub corr: Vec<f64>,
    pub valid: Vec<bool>,
    /// Valid and inside 60 Hz–12.5 kHz (where tonal balance is judged).
    pub valid_mid: Vec<bool>,
}

struct Middle {
    buf: AudioBuffer,
    eq: Option<EqIntent>,
    release_base: f64,
}

#[allow(clippy::too_many_arguments)]
fn middle(
    sim: &mut Sim,
    mut buf: AudioBuffer,
    opts: &Options,
    genre: &GenreData,
    intensity: &IntensityChoice,
    m: &MasteringAnalysis,
    speech: bool,
    drift: Option<&[f64]>,
    notes: &mut Vec<String>,
) -> Result<Middle> {
    let rate = buf.sample_rate as f64;
    // 2. corrective EQ
    let mut eq_intent = None;
    if !opts.no_eq {
        let levels = spectrum::third_octave_levels(&buf);
        let (label, target): (String, Vec<Option<f64>>) = match &opts.reference {
            Some((path, r)) => {
                let rl = spectrum::third_octave_levels(r);
                let t = levels
                    .iter()
                    .map(|b| rl.iter().find(|x| (x.center_hz - b.center_hz).abs() < 0.01 * b.center_hz).map(|x| x.db))
                    .collect();
                (format!("the reference {path}"), t)
            }
            None => (
                format!("the {} genre curve", genre.name),
                levels.iter().map(|b| Some(genres::curve_at(genre.curve, b.center_hz))).collect(),
            ),
        };
        let mut intent = eq_intent_for(&levels, &target, opts.match_amount, rate);
        if let Some(d) = drift {
            for i in 0..intent.corr.len() {
                if intent.valid[i] {
                    intent.corr[i] = (intent.corr[i] - d[i]).clamp(-MAX_EQ_DB, MAX_EQ_DB);
                }
            }
        }
        if let Some(stage) = graphic_eq_stage(&intent, rate, &label, opts.match_amount) {
            buf = sim.run(stage, buf)?.0;
        }
        eq_intent = Some(intent);
        // narrow resonances
        let res = find_resonances(&buf);
        if !res.is_empty() {
            let mut args = Vec::new();
            let mut desc = Vec::new();
            for (f, excess) in &res {
                let cut = ((excess - 9.0) / 3.0).clamp(0.5, 2.0);
                args.push(("band", format!("peak:{}:{}:q=8", fmt_freq(*f), fmt_db(-round_to(cut, 1)))));
                desc.push(format!("{} (+{excess:.0} dB over its neighbourhood)", fmt_freq(*f)));
            }
            let why = format!(
                "persistent narrow resonances found by spectrum analysis at {}; a static narrow cut stands in for a dynamic EQ band (ferraus eq has no dynamic bands yet)",
                desc.join(", ")
            );
            buf = sim.run(Stage::new("corrective_eq", "eq", args, why), buf)?.0;
        }
        let rumble = m
            .problems
            .problems
            .iter()
            .any(|p| p.problem == "rumble" && p.severity >= ferraus_analysis::Severity::Medium);
        let hp = if rumble { 30.0 } else { 20.0 };
        let why = if rumble {
            "sub-sonic rumble found by analyze problems: 30 Hz high-pass (24 dB/oct)".to_string()
        } else {
            "20 Hz high-pass (24 dB/oct) removes inaudible sub-sonic energy that wastes limiter headroom".to_string()
        };
        buf = sim
            .run(
                Stage::new("corrective_eq", "highpass", vec![("freq", fmt_freq(hp)), ("order", "4".into())], why),
                buf,
            )?
            .0;
    } else {
        notes.push("--no-eq: corrective EQ skipped".into());
    }

    // speech chain
    if speech {
        if sim.has_op("leveler") {
            buf = sim
                .run(Stage::new("speech", "leveler", vec![], "speech chain: even out the level of the voice"), buf)?
                .0;
        } else {
            let msg = "speech chain: leveler is not available in this build; skipped";
            sim.ctx.warn("leveler_unavailable", msg);
            notes.push(msg.into());
        }
        buf = sim.run(Stage::new("speech", "deess", vec![], "speech chain: tame sibilance"), buf)?.0;
    }

    // 3. dynamics
    let release_base = release_base(m);
    let xs: Vec<f64> = CROSSOVERS.iter().copied().filter(|f| *f < rate * 0.45).collect();
    let p90 = band_p90_db(&buf, &xs);
    let depth = depth_db(&intensity.name);
    let ratio = (multiband_ratio(&intensity.name) + genre.ratio_offset).clamp(1.5, 2.0);
    let nb = xs.len() + 1;
    let thr: Vec<String> = p90.iter().map(|l| fmt_db(round_to((l - depth).clamp(-60.0, -3.0), 1))).collect();
    let attack_ms = [30.0, 20.0, 10.0, 5.0];
    let rel_mult = [1.6, 1.2, 0.9, 0.7];
    let dens = m.dynamics.transient_density_per_s;
    let dens_mult = if dens > 4.0 { 0.85 } else { 1.0 };
    let att: Vec<String> = (0..nb).map(|b| format!("{}ms", attack_ms[b.min(3)])).collect();
    let rel: Vec<String> = (0..nb)
        .map(|b| format!("{:.0}ms", (release_base * rel_mult[b.min(3)] * dens_mult).clamp(0.04, 0.5) * 1000.0))
        .collect();
    let why = format!(
        "{nb} bands at {}:1; thresholds sit {depth:.0} dB under each band's loud (90th percentile) level so only peaks are compressed; releases follow the program ({:.0} ms base from {})",
        round_to(ratio, 2),
        release_base * 1000.0,
        if m.context.tempo_confidence >= 0.3 {
            format!("{:.0} BPM, {:.1} transients/s", m.context.tempo_bpm, dens)
        } else {
            "no steady tempo".to_string()
        }
    );
    let mb = Stage::new(
        "dynamics",
        "multiband",
        vec![
            ("crossovers", xs.iter().map(|f| fmt_freq(*f)).collect::<Vec<_>>().join(",")),
            ("threshold", thr.join(",")),
            ("ratio", format!("{}:1", round_to(ratio, 2))),
            ("attack", att.join(",")),
            ("release", rel.join(",")),
            ("makeup", "0dB".into()),
        ],
        why,
    );
    buf = sim.run(mb, buf)?.0;
    let crest = ferraus_analysis::levels::crest_factor_db(&buf);
    if crest > genre.crest_target_db {
        let glue_depth = match intensity.name.as_str() {
            "light" => 1.5,
            "heavy" => 3.0,
            _ => 2.0,
        };
        let full = band_p90_db(&buf, &[]);
        let t = (full[0] - glue_depth).clamp(-60.0, -3.0);
        let why = format!(
            "crest factor {crest:.1} dB is above the {} target of {:.0} dB: gentle single-band glue",
            genre.name, genre.crest_target_db
        );
        let st = Stage::new(
            "dynamics",
            "compress",
            vec![
                ("threshold", fmt_db(round_to(t, 1))),
                ("ratio", "1.5:1".into()),
                ("attack", "30ms".into()),
                ("release", format!("{:.0}ms", (2.0 * release_base).clamp(0.1, 0.4) * 1000.0)),
                ("knee", "6dB".into()),
                ("makeup", "0dB".into()),
                ("lookahead", "5ms".into()),
                ("detector", "rms".into()),
                ("sidechain-hpf", "80Hz".into()),
            ],
            why,
        );
        buf = sim.run(st, buf)?.0;
    }

    // 4. stereo
    if buf.channels() >= 2 && !opts.no_widen {
        let (amount, mono_below, guard) = stereo_settings(&buf, opts.width, opts.bass_mono);
        let mut why =
            format!("width {} with the side signal collapsed below {}", fmt_pct(amount), fmt_freq(mono_below));
        if let Some(g) = guard {
            why = format!("{why}; {g}");
            sim.ctx.warn("correlation_guard", g.clone());
            notes.push(g);
        }
        let st =
            Stage::new("stereo", "widen", vec![("amount", fmt_pct(amount)), ("mono-below", fmt_freq(mono_below))], why);
        buf = sim.run(st, buf)?.0;
    } else if opts.no_widen {
        notes.push("--no-widen: stereo stage skipped".into());
    }

    // 5. color
    if opts.warmth > 0.0 {
        let drive = round_to((opts.warmth * 20.0).min(12.0), 1);
        buf = sim
            .run(
                Stage::new(
                    "color",
                    "saturate",
                    vec![
                        ("type", "tape".into()),
                        ("drive", fmt_db(drive)),
                        ("tone", "0".into()),
                        ("mix", "100%".into()),
                    ],
                    format!("tape warmth at {} (drive {drive} dB)", fmt_pct(opts.warmth)),
                ),
                buf,
            )?
            .0;
        let mix = round_to((opts.warmth * 20.0).min(25.0), 1);
        buf = sim
            .run(
                Stage::new(
                    "color",
                    "saturate",
                    vec![("type", "soft".into()), ("drive", "12dB".into()), ("tone", "100".into()), ("mix", format!("{mix}%"))],
                    format!(
                        "exciter: a brightened (high-tilted) harmonic layer blended in at {mix}% adds sheen mostly above 3 kHz (ferraus has no band-limited exciter yet)"
                    ),
                ),
                buf,
            )?
            .0;
    }
    Ok(Middle { buf, eq: eq_intent, release_base })
}

/// Program-dependent release base: an eighth note at the detected tempo (80–300 ms), or
/// 150 ms without a steady tempo.
fn release_base(m: &MasteringAnalysis) -> f64 {
    let bpm = m.context.tempo_bpm;
    if m.context.tempo_confidence >= 0.3 && (50.0..=220.0).contains(&bpm) {
        (0.5 * 60.0 / bpm).clamp(0.08, 0.3)
    } else {
        0.15
    }
}

/// 90th percentile of 10 ms RMS levels (dB, mean power over channels) of each band.
pub fn band_p90_db(buf: &AudioBuffer, xs: &[f64]) -> Vec<f64> {
    let rate = buf.sample_rate as f64;
    let nb = xs.len() + 1;
    let n = buf.frames();
    let win = ((rate * 0.01) as usize).max(1);
    let nwin = n / win;
    let mut power = vec![vec![0.0f64; nwin]; nb];
    for ch in &buf.data {
        let bands: Vec<Vec<f32>> =
            if xs.is_empty() { vec![ch.clone()] } else { MultibandSplitter::new(xs, rate).split_buffer(ch) };
        for (b, sig) in bands.iter().enumerate() {
            for (w, p) in power[b].iter_mut().enumerate() {
                let s = &sig[w * win..(w + 1) * win];
                *p += s.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / win as f64;
            }
        }
    }
    let nch = buf.channels().max(1) as f64;
    power
        .into_iter()
        .map(|p| {
            let mut db: Vec<f64> =
                p.iter().map(|v| 10.0 * (v / nch).max(1e-12).log10()).filter(|d| *d > -80.0).collect();
            if db.is_empty() {
                return -80.0;
            }
            db.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            db[((db.len() - 1) as f64 * 0.9).round() as usize]
        })
        .collect()
}

/// The correction each band needs to move `levels` towards `target` (offset removed,
/// scaled by `amount`, capped at ±4 dB). Bands with no energy are left alone.
pub fn eq_intent_for(levels: &[BandLevel], target: &[Option<f64>], amount: f64, rate: f64) -> EqIntent {
    let max_level = levels.iter().map(|b| b.db).fold(-200.0, f64::max);
    let hi = 16000f64.min(rate * 0.45);
    let valid: Vec<bool> = levels
        .iter()
        .zip(target)
        .map(|(b, t)| b.center_hz >= 24.0 && b.center_hz <= hi && b.db > max_level - 60.0 && t.is_some())
        .collect();
    let valid_mid: Vec<bool> =
        levels.iter().zip(&valid).map(|(b, v)| *v && b.center_hz >= 60.0 && b.center_hz <= 12600.0).collect();
    let raw: Vec<f64> = levels.iter().zip(target).map(|(b, t)| t.map(|t| t - b.db).unwrap_or(0.0)).collect();
    // 1/3-octave smoothing of the difference, so single notes are not "corrected"
    let delta = smooth_bands(&raw, &valid);
    let mid: Vec<f64> = delta.iter().zip(&valid_mid).filter(|(_, v)| **v).map(|(d, _)| *d).collect();
    let offset = if mid.is_empty() { 0.0 } else { mid.iter().sum::<f64>() / mid.len() as f64 };
    let corr = delta
        .iter()
        .zip(&valid)
        .zip(levels)
        .map(|((d, v), b)| {
            if !*v {
                return 0.0;
            }
            let c = ((d - offset) * amount).clamp(-MAX_EQ_DB, MAX_EQ_DB);
            // never pump up sub-bass that is not there: boosts below 40 Hz stay small
            if b.center_hz < 40.0 {
                c.min(SUB_BOOST_MAX_DB)
            } else {
                c
            }
        })
        .collect();
    EqIntent {
        centers: levels.iter().map(|b| b.center_hz).collect(),
        levels: levels.iter().map(|b| b.db).collect(),
        corr,
        valid,
        valid_mid,
    }
}

/// Largest boost the corrective EQ applies below 40 Hz.
const SUB_BOOST_MAX_DB: f64 = 1.5;

/// Three-tap (1/4, 1/2, 1/4) smoothing across neighbouring valid bands.
fn smooth_bands(x: &[f64], valid: &[bool]) -> Vec<f64> {
    (0..x.len())
        .map(|i| {
            if !valid[i] {
                return x[i];
            }
            let (mut s, mut w) = (0.5 * x[i], 0.5);
            if i > 0 && valid[i - 1] {
                s += 0.25 * x[i - 1];
                w += 0.25;
            }
            if i + 1 < x.len() && valid[i + 1] {
                s += 0.25 * x[i + 1];
                w += 0.25;
            }
            s / w
        })
        .collect()
}

/// Mean response (dB) of a set of 1/3-octave peak bands over the 1/3-octave band at `f`.
fn graphic_response(f: f64, centers: &[f64], gains: &[f64], rate: f64) -> f64 {
    let mut acc = 0.0;
    for k in -2..=2 {
        let x = f * 2f64.powf(k as f64 / 15.0);
        let mut db = 0.0;
        for (c, g) in centers.iter().zip(gains) {
            if g.abs() >= 0.01 && (x / c).log2().abs() < 2.0 {
                db += Coeffs::design(FilterType::Peak, *c, THIRD_OCT_Q, *g, rate).magnitude_db(x, rate);
            }
        }
        acc += db;
    }
    acc / 5.0
}

/// Band gains whose combined response matches `want` at every band (iterative fit).
pub fn fit_graphic(centers: &[f64], want: &[f64], rate: f64) -> Vec<f64> {
    let mut g = want.to_vec();
    for _ in 0..25 {
        let mut max_err: f64 = 0.0;
        let resp: Vec<f64> = centers.iter().map(|c| graphic_response(*c, centers, &g, rate)).collect();
        for i in 0..g.len() {
            let e = want[i] - resp[i];
            max_err = max_err.max(e.abs());
            g[i] = (g[i] + 0.8 * e).clamp(-MAX_EQ_DB, MAX_EQ_DB);
        }
        if max_err < 0.05 {
            break;
        }
    }
    g
}

fn graphic_eq_stage(intent: &EqIntent, rate: f64, label: &str, amount: f64) -> Option<Stage> {
    let idx: Vec<usize> = (0..intent.centers.len()).filter(|i| intent.valid[*i]).collect();
    if idx.is_empty() {
        return None;
    }
    let centers: Vec<f64> = idx.iter().map(|i| intent.centers[*i]).collect();
    let want: Vec<f64> = idx.iter().map(|i| intent.corr[*i]).collect();
    if want.iter().all(|w| w.abs() < 0.1) {
        return None;
    }
    let gains = fit_graphic(&centers, &want, rate);
    let mut args: Vec<(&str, String)> = Vec::new();
    for (c, g) in centers.iter().zip(&gains) {
        let g = round_to(*g, 1);
        if g.abs() >= 0.1 {
            args.push(("band", format!("peak:{}:{}:q=4.32", fmt_freq(crate::util::nominal(*c)), fmt_db(g))));
        }
    }
    if args.is_empty() {
        return None;
    }
    args.push(("linear-phase", "true".into()));
    let max = want.iter().fold(0.0f64, |m, w| m.max(w.abs()));
    Some(Stage::new(
        "corrective_eq",
        "eq",
        args,
        format!(
            "match-eq toward {label} at {} (1/3-octave bands, at most {MAX_EQ_DB} dB per band; largest correction {max:.1} dB), applied as linear-phase 1/3-octave bands",
            fmt_pct(amount)
        ),
    ))
}

/// How far the processed signal's tonal balance drifted from the EQ's intent, per band
/// (offset removed over 60 Hz–12.5 kHz).
fn tonal_drift(buf: &AudioBuffer, eq: &EqIntent) -> Vec<f64> {
    let now = spectrum::third_octave_levels(buf);
    let raw: Vec<f64> = eq
        .centers
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let m = now.iter().find(|b| (b.center_hz - c).abs() < 0.01 * c).map(|b| b.db).unwrap_or(eq.levels[i]);
            m - (eq.levels[i] + eq.corr[i])
        })
        .collect();
    let mid: Vec<f64> = raw.iter().zip(&eq.valid_mid).filter(|(_, v)| **v).map(|(d, _)| *d).collect();
    let off = if mid.is_empty() { 0.0 } else { mid.iter().sum::<f64>() / mid.len() as f64 };
    let d: Vec<f64> = raw.iter().zip(&eq.valid).map(|(d, v)| if *v { d - off } else { 0.0 }).collect();
    smooth_bands(&d, &eq.valid)
}

/// Persistent narrow spectral peaks (resonances) between 150 Hz and 8 kHz:
/// `(frequency, excess over the 1/3-octave neighbourhood in dB)`, at most two.
pub fn find_resonances(buf: &AudioBuffer) -> Vec<(f64, f64)> {
    let rate = buf.sample_rate as f64;
    let mono = buf.mono_mix();
    let size = 8192usize;
    if mono.len() < size * 2 {
        return Vec::new();
    }
    let df = rate / size as f64;
    let to_db = |p: &[f64]| -> Vec<f64> { p.iter().map(|v| 10.0 * v.max(1e-20).log10()).collect() };
    let db = to_db(&ferraus_dsp::stft::welch_psd(&mono, size));
    let gmax = db.iter().skip(2).cloned().fold(-400.0, f64::max);
    let local_median = |d: &[f64], k: usize| -> f64 {
        let lo = ((k as f64) * 2f64.powf(-1.0 / 6.0)).floor() as usize;
        let hi = (((k as f64) * 2f64.powf(1.0 / 6.0)).ceil() as usize).min(d.len() - 1);
        let mut w: Vec<f64> = d[lo.max(1)..=hi].to_vec();
        w.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        w[w.len() / 2]
    };
    let k0 = (150.0 / df).ceil() as usize;
    let k1 = ((8000.0f64.min(rate * 0.45)) / df).floor() as usize;
    let mut cands: Vec<(usize, f64)> = Vec::new();
    for k in k0.max(3)..k1.min(db.len() - 3) {
        let v = db[k];
        if v < gmax - 35.0 || !(v > db[k - 1] && v >= db[k + 1] && v > db[k - 2] && v >= db[k + 2]) {
            continue;
        }
        let ex = v - local_median(&db, k);
        if ex >= 12.0 {
            cands.push((k, ex));
        }
    }
    if cands.is_empty() {
        return Vec::new();
    }
    // persistence: the peak must stand out in most loud half-second segments
    let seg = (rate * 0.5) as usize;
    let seg_size = 4096usize.min(seg.next_power_of_two() / 2).max(1024);
    let overall = mono.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / mono.len() as f64;
    let mut segs: Vec<Vec<f64>> = Vec::new();
    let mut s = 0;
    while s + seg <= mono.len() {
        let x = &mono[s..s + seg];
        let e = x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / seg as f64;
        if e > overall * 0.01 {
            segs.push(to_db(&ferraus_dsp::stft::welch_psd(x, seg_size)));
        }
        s += seg;
    }
    if segs.len() < 4 {
        return Vec::new();
    }
    let sdf = rate / seg_size as f64;
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (k, ex) in cands {
        let f = k as f64 * df;
        let sk = (f / sdf).round() as usize;
        if sk < 2 || sk + 2 >= segs[0].len() {
            continue;
        }
        let hits = segs
            .iter()
            .filter(|d| {
                let v = d[sk - 1..=sk + 1].iter().cloned().fold(-400.0, f64::max);
                v - local_median(d, sk) >= 6.0
            })
            .count();
        if hits as f64 >= 0.9 * segs.len() as f64 {
            out.push((f, ex));
        }
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<(f64, f64)> = Vec::new();
    for (f, e) in out {
        if kept.iter().all(|(g, _)| (f / g).log2().abs() > 1.0 / 6.0) {
            kept.push((round_to(f, 0), round_to(e, 1)));
        }
        if kept.len() == 2 {
            break;
        }
    }
    kept
}

/// Widen amount and mono-bass frequency honouring the correlation guard: the largest
/// amount up to `want` that keeps broadband correlation at or above 0.3 (plus margin), and
/// a mono-below frequency that keeps the correlation below 120 Hz at or above 0.9.
fn stereo_settings(buf: &AudioBuffer, want: f64, bass_mono: f64) -> (f64, f64, Option<String>) {
    let rate = buf.sample_rate as f64;
    let mut mono_below = bass_mono;
    let mut note = None;
    // side signal after the mono-bass crossover, as `widen` computes it
    let side_mid = |mb: f64| -> (Vec<f32>, Vec<f32>) {
        let mut m = Vec::with_capacity(buf.frames());
        let mut s = Vec::with_capacity(buf.frames());
        let mut hp = (mb > 0.0).then(|| ferraus_dsp::biquad::Cascade::linkwitz_riley_highpass(4, mb, rate));
        let mut ap = (mb > 0.0).then(|| ferraus_dsp::biquad::Biquad::new(ferraus_dsp::biquad::lr4_allpass(mb, rate)));
        for i in 0..buf.frames() {
            let (l, r) = (buf.data[0][i], buf.data[1][i]);
            let (mut mm, mut ss) = (0.5 * (l + r), 0.5 * (l - r));
            if let (Some(h), Some(a)) = (&mut hp, &mut ap) {
                ss = h.process(ss);
                mm = a.process(mm);
            }
            m.push(mm);
            s.push(ss);
        }
        (m, s)
    };
    let corr_at = |m: &[f32], s: &[f32], w: f64| -> f64 {
        let (mut em, mut es, mut ems) = (0.0f64, 0.0f64, 0.0f64);
        for (a, b) in m.iter().zip(s) {
            let (a, b) = (*a as f64, *b as f64);
            em += a * a;
            es += b * b;
            ems += a * b;
        }
        let num = em - w * w * es;
        let den = ((em + w * w * es).powi(2) - 4.0 * w * w * ems * ems).max(1e-30).sqrt();
        if em + es <= 1e-20 {
            1.0
        } else {
            num / den
        }
    };
    let low_corr = |m: &[f32], s: &[f32], w: f64| -> f64 {
        let l: Vec<f32> = m.iter().zip(s).map(|(a, b)| a + (w as f32) * b).collect();
        let r: Vec<f32> = m.iter().zip(s).map(|(a, b)| a - (w as f32) * b).collect();
        stereo::low_correlation(&AudioBuffer::from_planar(buf.sample_rate, vec![l, r]), 120.0)
    };
    let (mut m, mut s) = side_mid(mono_below);
    let mut amount = want;
    let floor = MIN_CORRELATION + CORRELATION_MARGIN;
    if corr_at(&m, &s, amount) < floor {
        let (mut lo, mut hi) = (0.0, amount);
        for _ in 0..30 {
            let mid = 0.5 * (lo + hi);
            if corr_at(&m, &s, mid) >= floor {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        amount = (lo * 100.0).floor() / 100.0;
        note = Some(format!(
            "correlation guard: width reduced from {} to {} so broadband L/R correlation stays at or above {MIN_CORRELATION}",
            fmt_pct(want),
            fmt_pct(amount)
        ));
    }
    if mono_below > 0.0 || buf.channels() >= 2 {
        let mut tries = 0;
        while low_corr(&m, &s, amount) < 0.9 && tries < 4 {
            mono_below = if mono_below <= 0.0 { 120.0 } else { (mono_below * 1.4).min(300.0) };
            let ms = side_mid(mono_below);
            m = ms.0;
            s = ms.1;
            tries += 1;
        }
        if mono_below != bass_mono {
            let msg = format!(
                "correlation guard: mono bass raised from {} to {} so L/R correlation below 120 Hz stays at or above 0.9",
                fmt_freq(bass_mono),
                fmt_freq(mono_below)
            );
            note = Some(match note {
                Some(n) => format!("{n}; {msg}"),
                None => msg,
            });
        }
    }
    (amount, round_to(mono_below, 0), note)
}

/// Gain to the target, then the true-peak limiter, iterated so the result lands on the
/// target without the limiter exceeding the intensity's gain-reduction bound.
fn loudness_stage(
    sim: &mut Sim,
    buf: AudioBuffer,
    target: &Target,
    intensity: &IntensityChoice,
    release_base: f64,
) -> Result<(AudioBuffer, LoudnessOutcome)> {
    let bound = intensity.limiter_max_gain_reduction_db;
    let l0 = loudness::integrated_lufs(&buf);
    let tp0 = loudness::true_peak_db(&buf);
    let mut ceiling = target.true_peak_dbtp;
    let release = format!("{:.0}ms", (0.5 * release_base).clamp(0.04, 0.15) * 1000.0);
    let limit_stage = |ceiling: f64| {
        Stage::new(
            "loudness",
            "limit",
            vec![
                ("ceiling", format!("{}TP", fmt_db(round_to(ceiling, 2)))),
                ("release", release.clone()),
                ("lookahead", "5ms".into()),
                ("oversample", "4x".into()),
            ],
            format!(
                "true-peak limiter at {:.2} dBTP with 4x oversampling; --intensity {} allows up to {bound:.0} dB of gain reduction",
                ceiling, intensity.name
            ),
        )
    };
    let gain_stage = |g: f64| {
        Stage::new(
            "loudness",
            "gain",
            vec![("by", fmt_db(round_to(g, 2)))],
            format!("gain to the {:.1} LUFS target", target.lufs),
        )
    };
    // a scratch simulation for the search (stages are recorded once the gain is known)
    let try_gain = |sim: &mut Sim, g: f64, ceiling: f64| -> Result<(AudioBuffer, f64, f64)> {
        let (b, _) = run_stage_filtered(sim.ctx, &gain_stage(g), buf.clone(), None)?;
        let (b, m) = run_stage_filtered(sim.ctx, &limit_stage(ceiling), b, None)?;
        let gr = m.get("gain_reduction_max_db").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let l = loudness::integrated_lufs(&b);
        Ok((b, l, gr))
    };
    let want = target.lufs - l0;
    let mut g = want.min(ceiling + bound - tp0);
    let mut best: Option<(f64, AudioBuffer, f64, f64)> = None;
    for _ in 0..8 {
        let g_round = round_to(g, 2);
        let (b, l, gr) = try_gain(sim, g_round, ceiling)?;
        let valid = gr <= bound + 0.05;
        if valid {
            let better = match &best {
                None => true,
                Some((_, _, bl, _)) => (target.lufs - l).abs() < (target.lufs - bl).abs(),
            };
            if better {
                best = Some((g_round, b, l, gr));
            }
        }
        if !valid {
            g = g_round - (gr - bound).max(0.1);
            continue;
        }
        let err = target.lufs - l;
        if err.abs() <= 0.08 {
            break;
        }
        if err > 0.0 {
            let room = bound - gr;
            if room <= 0.05 {
                break;
            }
            g = g_round + err.min(room);
        } else {
            g = g_round + err;
        }
    }
    let (mut g, mut out, mut landed, mut gr) = match best {
        Some(b) => b,
        None => {
            // even the smallest search step needed too much limiting: take the safest gain
            let g = round_to(ceiling + bound - tp0 - 0.5, 2).min(want);
            let (b, l, gr) = try_gain(sim, g, ceiling)?;
            (g, b, l, gr)
        }
    };
    // meter-mismatch guard: the analysis meter must agree that the ceiling holds
    let mut tp = loudness::true_peak_db(&out);
    let mut guard = 0;
    while tp > target.true_peak_dbtp && guard < 4 {
        ceiling = round_to(ceiling - (tp - target.true_peak_dbtp + 0.02), 2);
        let mut r = try_gain(sim, g, ceiling)?;
        if r.2 > bound + 0.05 {
            // keep the intensity bound: give up a little loudness instead
            g = round_to(g - (r.2 - bound), 2);
            r = try_gain(sim, g, ceiling)?;
        }
        out = r.0;
        landed = r.1;
        gr = r.2;
        tp = loudness::true_peak_db(&out);
        guard += 1;
    }
    if tp > target.true_peak_dbtp {
        // last resort: a static trim under the ceiling
        let trim = target.true_peak_dbtp - tp - 0.02;
        g += trim;
        let r = try_gain(sim, g, ceiling)?;
        out = r.0;
        landed = r.1;
        gr = r.2;
        tp = loudness::true_peak_db(&out);
    }
    // record the two stages with the final values (and their trace)
    let gs = gain_stage(round_to(g, 2));
    let ls = limit_stage(ceiling);
    sim.trace.push(StageTrace { op: "gain".into(), metrics: Map::new() });
    let mut lm = Map::new();
    lm.insert("gain_reduction_max_db".into(), json!(round_to(gr, 2)));
    sim.trace.push(StageTrace { op: "limit".into(), metrics: lm });
    sim.stages.push(gs);
    sim.stages.push(ls);
    let reached = (target.lufs - landed).abs() <= 0.5;
    let outcome = LoudnessOutcome {
        target_lufs: target.lufs,
        ceiling_dbtp: target.true_peak_dbtp,
        gain_db: round_to(g, 2),
        limiter_gain_reduction_db: round_to(gr, 2),
        limiter_bound_db: bound,
        landed_lufs: round_to(landed, 2),
        true_peak_dbtp: round_to(tp, 2),
        reached,
        shortfall_lu: if reached { None } else { Some(round_to(target.lufs - landed, 2)) },
    };
    Ok((out, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphic_fit_matches_a_smooth_target() {
        let rate = 48000.0;
        let centers: Vec<f64> = (0..28).map(|k| 25.0 * 2f64.powf(k as f64 / 3.0)).collect();
        let want: Vec<f64> = centers.iter().map(|c| 3.0 * (c.log2() - 10.0).sin().clamp(-1.0, 1.0)).collect();
        let g = fit_graphic(&centers, &want, rate);
        for (i, c) in centers.iter().enumerate() {
            let r = graphic_response(*c, &centers, &g, rate);
            assert!((r - want[i]).abs() < 0.4, "{c}: {r} vs {}", want[i]);
        }
    }

    #[test]
    fn eq_intent_caps_and_removes_offset() {
        let levels: Vec<BandLevel> = (0..20)
            .map(|k| {
                let c = 63.0 * 2f64.powf(k as f64 / 3.0);
                BandLevel { center_hz: c, low_hz: c * 0.89, high_hz: c * 1.12, db: -20.0 }
            })
            .collect();
        let target: Vec<Option<f64>> =
            levels.iter().enumerate().map(|(i, _)| Some(if i == 5 { 10.0 } else { 0.0 })).collect();
        let e = eq_intent_for(&levels, &target, 1.0, 48000.0);
        assert!((e.corr[5] - MAX_EQ_DB).abs() < 1e-9);
        assert!(e.corr[0].abs() < 1.0);
        let half = eq_intent_for(&levels, &target, 0.5, 48000.0);
        assert!(half.corr[5] <= MAX_EQ_DB);
    }
}
