//! The mastering commands: `master`, `master analyze`, `master plan`, `master verify`,
//! `master deliver`.

use crate::analyze::{self, AnalyzeRequest};
use crate::chain::{self, LoudnessOutcome, Options};
use crate::deliver;
use crate::genres::GENRE_NAMES;
use crate::plan::{run_plan_guarded, run_stage, Plan, StageTrace};
use crate::stems;
use crate::targets::{self, Target, TARGET_NAMES};
use crate::util::{jnum, round_to};
use crate::verify;
use ferraus_analysis::loudness;
use ferraus_core::exec::default_ext;
use ferraus_core::prelude::*;
use ferraus_core::{EncodeOptions, ParamSpec, SourceInfo};
use std::path::{Path, PathBuf};

const GENRE_CHOICES: &[&str] = &["auto", "pop", "hiphop", "edm", "rock", "acoustic", "classical", "jazz", "podcast"];

fn target_params() -> Vec<ParamSpec> {
    vec![
        param("target", ParamKind::Choice)
            .choices(TARGET_NAMES)
            .default("streaming")
            .help("Delivery target: streaming -14 LUFS/-1 dBTP, apple -16/-1, youtube -14/-1, club -8/-0.3, cd -10/-0.3 (16-bit 44.1 kHz), broadcast -23/-1 (EBU R128), podcast -16 stereo or -19 mono/-1, custom (--lufs, --true-peak)"),
        param("lufs", ParamKind::Gain).help("Integrated loudness for --target custom (e.g. -12LUFS); overrides a named target's loudness"),
        param("true-peak", ParamKind::Gain).help("True-peak ceiling for --target custom (e.g. -1dBTP, default -1dBTP); overrides a named target's ceiling"),
        param("standard", ParamKind::Choice)
            .choices(&["ebu", "atsc"])
            .default("ebu")
            .help("Broadcast standard for --target broadcast: ebu (-23 LUFS, -1 dBTP) or atsc (-24 LKFS, -2 dBTP)"),
    ]
}

fn chain_params() -> Vec<ParamSpec> {
    vec![
        param("reference", ParamKind::Path).help("Reference track whose tonal balance the corrective EQ moves toward (replaces the genre curve)"),
        param("match-amount", ParamKind::Percent).default("50%").help("How much of the measured tonal difference the corrective EQ corrects (each band is capped at 4 dB)"),
        param("genre", ParamKind::Choice).choices(GENRE_CHOICES).default("auto").help("Genre for the tonal curve and dynamics targets; auto guesses from tempo, spectral tilt, low-end share, and crest factor"),
        param("intensity", ParamKind::Choice)
            .choices(&["light", "medium", "heavy"])
            .default("medium")
            .help("How hard to push: bounds limiter gain reduction (light 2 dB, medium 4 dB, heavy 6 dB) and sets compression ratios; an input that already looks mastered defaults to light"),
        param("width", ParamKind::Percent).default("100%").help("Stereo width of the side signal (100% unchanged); never narrower than mono bass or wider than broadband correlation 0.3 allows"),
        param("bass-mono", ParamKind::Freq).default("120Hz").help("Side signal below this frequency is collapsed to mono"),
        param("warmth", ParamKind::Percent).default("15%").help("Tape saturation and exciter amount (0% skips the color stage)"),
        param("no-eq", ParamKind::Flag).help("Skip the corrective EQ"),
        param("no-widen", ParamKind::Flag).help("Skip the stereo stage"),
        param("trim-silence", ParamKind::Str).default("-70dB").help("Trim head and tail silence below this level (dBFS), or 'off'"),
    ]
}

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("master", Category::Mastering, "Automatic master of a finished mix to a delivery target", master)
            .inputs(Inputs::ONE_AUDIO)
            .about("Runs the automatic mastering chain and writes a release-ready master. Pre-flight: dc-remove, head/tail silence trim at -70 dB, analyze problems (clipping means declip runs first). Corrective EQ: match-eq toward --reference or the genre curve at --match-amount (1/3-octave bands, at most 4 dB per band), static cuts on persistent narrow resonances, 20 Hz high-pass. Dynamics: four-band multiband compression at 1.5:1 to 2:1 with tempo-dependent release, plus glue compression (1.5:1, 30 ms) when the crest factor is above the genre target. Stereo: widen with mono bass below --bass-mono, never letting broadband correlation drop below 0.3. Color: tape saturation and an exciter layer at --warmth. Loudness: gain to the target, then a 4x-oversampled true-peak limiter whose gain reduction is bounded by --intensity; if the target cannot be reached within the bound the master lands as close as possible and the report says so. Output: TPDF dither when writing 16-bit, master verify, the before/after report (--report), and a loudness-matched before/after pair (--ab). Every stage is an ordinary ferraus command; 'master plan' shows the exact chain and 'master --plan' runs an edited one. --stems (best-effort) separates the mix first and treats vocals, drums, bass, and other separately.")
            .params(target_params())
            .params(chain_params())
            .param(param("report", ParamKind::Path).help("Write the before/after report (JSON) here"))
            .param(param("ab", ParamKind::Flag).help("Also write a loudness-matched before/after pair (within 0.2 LU) for listening: <output>.ab-before.wav and <output>.ab-after.wav"))
            .param(param("plan", ParamKind::Path).help("Run this (edited) plan instead of planning: the JSON from 'master plan'"))
            .param(param("stems", ParamKind::Flag).help("Best-effort stem-aware mastering: separate --tier quality, per-stem chains, then sum and master"))
            .example("ferraus master mix.wav -o master.wav", "Master for streaming (-14 LUFS, -1 dBTP) with every default")
            .example("ferraus master mix.wav --plan plan.json -o master_v1.wav --report r1.json", "Run an edited plan and write the before/after report")
            .example("ferraus master mix.wav --reference ref.wav --match-amount 75% --ab -o master.wav", "Move toward a reference's tonal balance and write an A/B pair")
            .example("ferraus master mix.wav --target club --genre edm --intensity heavy -o club.wav", "A loud club master"),
        OpSpec::custom("master analyze", Category::Mastering, "The measurement report an agent decides from", master_analyze)
            .inputs(Inputs::ONE_AUDIO)
            .about("Measures everything master decides from: loudness (integrated, short-term and momentary maximum, loudness range, true peak, sample peak, crest factor, PLR), tonal balance per 1/3-octave band against --reference or the genre curve with the three largest deviations, dynamics (gain and limiting needed to reach --target, transient density, much louder or quieter sections), stereo (width per band, correlation broadband and below 120 Hz, side energy, mono compatibility), problems (each with a suggested fix as a runnable fx stage), and context (tempo, key, duration, sample rate, bit depth, and whether the input already looks mastered, in which case master defaults to --intensity light and says why). -o writes the report as JSON.")
            .params(target_params())
            .param(param("reference", ParamKind::Path).help("Reference track for the tonal-balance comparison"))
            .param(param("genre", ParamKind::Choice).choices(GENRE_CHOICES).default("auto").help("Genre curve for the tonal-balance comparison (auto guesses)"))
            .example("ferraus master analyze mix.wav --reference ref.wav --json > report.json", "Read the numbers and the problem list")
            .example("ferraus master analyze mix.wav --genre hiphop --target apple --json", "Compare with the hip-hop curve and the Apple target"),
        OpSpec::custom("master plan", Category::Mastering, "The exact chain master would apply, every parameter resolved, editable", master_plan)
            .inputs(Inputs::ONE_AUDIO)
            .about("Plans the master without writing audio: the planner simulates the chain on this mix and records every stage with all of its parameters resolved, as JSON ({\"version\":1, \"target\":..., \"stages\":[{\"op\":\"multiband\",\"args\":{...}}, ...]}). Edit any value and run it with 'master <mix> --plan plan.json'. Stage args are ordinary command parameters (see 'ferraus schema <op>'); 'stage' and 'why' fields are informational. -o writes the plan file.")
            .params(target_params())
            .params(chain_params())
            .example("ferraus master plan mix.wav --target streaming --json > plan.json", "See the proposed chain and edit any value")
            .example("ferraus master plan mix.wav --target cd --genre rock -o plan.json", "Plan a CD master for a rock mix and save it"),
        OpSpec::custom("master verify", Category::Mastering, "Pass or fail against a delivery spec", master_verify)
            .inputs(Inputs::ONE_AUDIO)
            .about("Checks a file against a delivery target: integrated loudness within --tolerance, true peak at or below the ceiling (4x oversampled), no clipping, DC offset at most 0.001, no out-of-phase bass (correlation below 120 Hz at least 0.5), no more than 1 s of head silence, broadband correlation not negative (below 0.3 warns), and for cd 44.1 kHz 16-bit. Exit code 9 (verification failed) when any check fails, with the failed checks in the message; the full list with a runnable fix per check is in the result.")
            .params(target_params())
            .param(param("tolerance", ParamKind::Str).default("0.5LU").help("Allowed distance from the target loudness (LU)"))
            .example("ferraus master verify master_v1.wav --target streaming --json", "Pass or fail per spec")
            .example("ferraus master verify spot.wav --target broadcast --standard atsc --tolerance 1LU", "Check an ATSC A/85 broadcast deliverable"),
        OpSpec::custom("master deliver", Category::Mastering, "Write the deliverable set", master_deliver)
            .inputs(Inputs::ONE_AUDIO)
            .about("Writes one file per --formats entry from a finished master: wav24 (24-bit, original rate), wav16 (16-bit 44.1 kHz with TPDF dither), mp3 (320 kbps, 44.1 kHz), flac (24-bit), aac (256 kbps .m4a, 44.1 kHz; needs FFmpeg). Resampling re-limits any new inter-sample peaks to the master's true peak. --tags and --isrc are written into every file; --loudness-sheet also writes a JSON and a CSV loudness sheet (integrated loudness, LRA, true peak, PLR per deliverable). Output names: <output>.<format>.<ext>.")
            .param(param("formats", ParamKind::List).default("wav24,wav16,mp3").help("Comma list of wav24, wav16, mp3, flac, aac"))
            .param(param("tags", ParamKind::Str).repeatable().help("Metadata key=value (title, artist, album, date, genre, track, ...); repeat or comma-separate"))
            .param(param("isrc", ParamKind::Str).help("ISRC code written to every deliverable (CC-XXX-YY-NNNNN)"))
            .param(param("loudness-sheet", ParamKind::Flag).help("Also write a loudness sheet (<output>.loudness.json and .csv)"))
            .example("ferraus master deliver master.wav --tags title=Song,artist=Me --isrc US-RC1-76-07839 --loudness-sheet -o release/song.wav", "WAV 24, WAV 16, and MP3 with tags and a loudness sheet")
            .example("ferraus master deliver master.wav --formats wav24,flac,aac -o release/song.wav", "Lossless and AAC deliverables"),
    ]
}

// ---------------------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------------------

fn probe_info(ctx: &OpCtx, path: &Path) -> Option<SourceInfo> {
    if path.as_os_str() == "-" {
        return None;
    }
    ctx.io.probe(path).ok()
}

fn read_reference(a: &Args, ctx: &mut OpCtx) -> Result<Option<(String, AudioBuffer)>> {
    match a.path("reference") {
        None => Ok(None),
        Some(p) => {
            let b = ctx.read_audio_path(&p)?;
            if b.is_empty() {
                return Err(Error::input(format!("the reference {} has no audio", p.display())));
            }
            Ok(Some((p.display().to_string(), b)))
        }
    }
}

fn options_from_args(a: &Args, ctx: &mut OpCtx, channels: usize) -> Result<Options> {
    let target = targets::resolve(a, channels)?;
    let genre = a.str_or("genre", "auto");
    if genre != "auto" && !GENRE_NAMES.contains(&genre.as_str()) {
        return Err(Error::usage(format!("unknown --genre '{genre}'")));
    }
    let width = a.percent("width")?;
    if !(0.0..=2.0).contains(&width) {
        return Err(Error::usage("--width must be between 0% and 200%"));
    }
    let warmth = a.percent("warmth")?;
    if !(0.0..=1.0).contains(&warmth) {
        return Err(Error::usage("--warmth must be between 0% and 100%"));
    }
    let match_amount = a.percent("match-amount")?;
    if !(0.0..=1.0).contains(&match_amount) {
        return Err(Error::usage("--match-amount must be between 0% and 100%"));
    }
    let bass_mono = a.freq("bass-mono")?;
    if !(0.0..=500.0).contains(&bass_mono) {
        return Err(Error::usage("--bass-mono must be between 0Hz (off) and 500Hz"));
    }
    let trim = a.str_or("trim-silence", "-70dB");
    let trim_db = match trim.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "false" | "no" => None,
        t => {
            let v = units::parse_db(t).map_err(|e| Error::usage(format!("--trim-silence: {} (or 'off')", e.message)))?;
            if v > -20.0 {
                return Err(Error::usage("--trim-silence must be -20dB or lower (it trims silence, not music)"));
            }
            Some(v)
        }
    };
    let reference = read_reference(a, ctx)?;
    Ok(Options {
        target,
        genre,
        intensity: if a.is_explicit("intensity") { Some(a.choice("intensity")) } else { None },
        width,
        bass_mono,
        warmth,
        no_eq: a.flag("no-eq"),
        no_widen: a.flag("no-widen"),
        trim_db,
        reference,
        match_amount,
    })
}

/// Path for a secondary output with its own extension: `<output stem>.<role>.<ext>`
/// (the core helper keeps the `-o` extension, which is wrong for a JSON sheet or an mp3).
fn role_path(ctx: &OpCtx, role: &str, ext: &str) -> Result<PathBuf> {
    let p = ctx.output_path(Some(role), ext)?;
    match &ctx.globals.output {
        Some(o) if o != "-" && !o.contains('{') => {
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            Ok(p.with_file_name(format!("{stem}.{ext}")))
        }
        _ => Ok(p),
    }
}

fn dedupe_warnings(ctx: &mut OpCtx) {
    let mut seen: Vec<(String, String)> = Vec::new();
    ctx.warnings.retain(|w| {
        let k = (w.code.clone(), w.message.clone());
        if seen.contains(&k) {
            false
        } else {
            seen.push(k);
            true
        }
    });
}

/// Encoding options for the master: the target's bit depth (16 for cd) or the global
/// `--bit-depth`, else 24-bit PCM; TPDF dither whenever the depth is reduced.
fn master_encode_options(ctx: &OpCtx, path: &Path, target: &Target) -> EncodeOptions {
    let mut eo = ctx.encode_options(path);
    let fmt = eo.format.clone().unwrap_or_else(|| "wav".into());
    let pcm = matches!(fmt.as_str(), "wav" | "wave" | "aiff" | "aif" | "flac");
    if eo.bit_depth.is_none() && pcm {
        eo.bit_depth = Some(target.bit_depth.unwrap_or(24));
    }
    if eo.bit_depth.map(|b| b < 32).unwrap_or(false) && pcm {
        eo.dither = Some(target.dither.clone().unwrap_or_else(|| "triangular".into()));
    }
    eo
}

// ---------------------------------------------------------------------------------------
// master
// ---------------------------------------------------------------------------------------

fn outcome_from_run(plan: &Plan, trace: &[StageTrace], out: &AudioBuffer) -> LoudnessOutcome {
    let landed = loudness::integrated_lufs(out);
    let tp = loudness::true_peak_db(out);
    let gr = trace
        .iter()
        .rev()
        .find(|t| t.op == "limit")
        .and_then(|t| t.metrics.get("gain_reduction_max_db"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let gain = plan
        .stages
        .iter()
        .rev()
        .find(|s| s.op == "gain")
        .and_then(|s| s.args.get("by"))
        .and_then(|v| v.as_str())
        .and_then(|s| units::parse_db(s).ok())
        .unwrap_or(0.0);
    let bound = plan.intensity.limiter_max_gain_reduction_db;
    let reached = (plan.target.lufs - landed).abs() <= 0.5;
    LoudnessOutcome {
        target_lufs: plan.target.lufs,
        ceiling_dbtp: plan.target.true_peak_dbtp,
        gain_db: round_to(gain, 2),
        limiter_gain_reduction_db: round_to(gr, 2),
        limiter_bound_db: if bound > 0.0 { bound } else { chain::intensity_bound(&plan.intensity.name) },
        landed_lufs: round_to(landed, 2),
        true_peak_dbtp: round_to(tp, 2),
        reached,
        shortfall_lu: if reached { None } else { Some(round_to(plan.target.lufs - landed, 2)) },
    }
}

fn master(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let input_path = ctx.input(0)?.to_path_buf();
    let info = probe_info(ctx, &input_path);
    let mut input = ctx.read_audio(0)?;
    if input.is_empty() {
        return Err(Error::input("the mix has no audio"));
    }
    let ext = default_ext(ctx);
    let out_path = ctx.output_path(None, &ext)?;
    ctx.prepare_output(&out_path)?;
    let registry = ctx.registry.ok_or_else(|| Error::internal("master needs the command registry"))?;

    let mut stems_info = Value::Null;
    let mut declip = None;
    let (plan, output, trace, outcome, reference, before_m) = if let Some(pp) = a.path("plan") {
        let mut plan = Plan::load(&pp)?;
        ctx.note_input(&pp);
        plan.validate(registry)?;
        for name in ["target", "genre", "intensity", "reference", "match-amount", "width", "warmth", "stems"] {
            if a.is_explicit(name) {
                ctx.warn("plan_overrides", format!("--{name} is ignored because --plan fixes every stage; edit the plan instead"));
            }
        }
        let reference = match &plan.reference {
            Some(p) if Path::new(p).exists() => Some((p.clone(), ctx.read_audio_path(Path::new(p))?)),
            _ => None,
        };
        let (out, trace, guard) = run_plan_guarded(ctx, &plan, input.clone())?;
        if let Some(g) = guard {
            ctx.warn("true_peak_guard", g.clone());
            plan.notes.push(g);
        }
        let outcome = outcome_from_run(&plan, &trace, &out);
        let bm = analyze::measure(&input, reference.as_ref().map(|r| &r.1));
        (plan, out, trace, outcome, reference, bm)
    } else {
        let opts = options_from_args(a, ctx, input.channels())?;
        if a.flag("stems") {
            if ctx.globals.dry_run {
                ctx.warn("stems_skipped", "--stems needs to run separate, which a dry run does not do");
            } else {
                let (sum, info) = stems::stem_aware_mix(ctx, &input_path, &input)?;
                input = sum;
                stems_info = info;
            }
        }
        let m = analyze::measure(&input, opts.reference.as_ref().map(|r| &r.1));
        let built = chain::build(ctx, input.clone(), &m, &opts)?;
        declip = built.declip.clone();
        let mut plan = built.plan;
        if !stems_info.is_null() {
            plan.notes.push("stem-aware: the stages below ran on the re-summed stems; --plan reruns them on the plain mix".into());
        }
        (plan, built.output, built.trace, built.loudness, opts.reference, m)
    };

    // output conversions the plan does not cover
    let mut output = output;
    if let Some(sr) = ctx.globals.sample_rate {
        if sr != output.sample_rate {
            ctx.warn("resample_after_limiter", format!("--sample-rate {sr} resamples after the limiter, which can create new inter-sample peaks; prefer a target with a fixed rate (cd) or 'master deliver'"));
            output = ferraus_core::exec::conform_output(output, &ctx.globals);
        }
    }
    let eo = master_encode_options(ctx, &out_path, &plan.target);
    ctx.write_audio_with(&out_path, &output, None, &eo)?;

    // verify what was written
    let (verify_buf, verify_info) = if !ctx.globals.dry_run && out_path.as_os_str() != "-" {
        match ctx.io.read(&out_path) {
            Ok((b, i)) => (b, Some(i)),
            Err(_) => (output.clone(), None),
        }
    } else {
        (output.clone(), None)
    };
    let vr = verify::verify(&verify_buf, verify_info.as_ref(), &plan.target, 0.5);
    if !vr.pass {
        ctx.warn("verify_failed", format!("master verify failed: {}", vr.failure_lines().join("; ")));
    }

    // before/after
    let refbuf = reference.as_ref().map(|r| &r.1);
    let after_m = analyze::measure(&verify_buf, refbuf);
    let (label, before_tone) = analyze::tone_against(&before_m, &plan.genre.name, refbuf.is_some());
    let (_, after_tone) = analyze::tone_against(&after_m, &plan.genre.name, refbuf.is_some());

    // A/B pair
    let mut ab = Value::Null;
    if a.flag("ab") {
        ab = write_ab(ctx, &plan, &input, &output)?;
    }

    let report = json!({
        "input": input_path.display().to_string(),
        "output": out_path.display().to_string(),
        "input_bit_depth": info.as_ref().and_then(|i| i.bit_depth),
        "output_bit_depth": eo.bit_depth,
        "output_dither": eo.dither,
        "target": plan.target,
        "genre": plan.genre,
        "intensity": plan.intensity,
        "tonal_reference": label,
        "declip": declip,
        "stems": stems_info,
        "before": analyze::summary(&before_m, &before_tone),
        "after": analyze::summary(&after_m, &after_tone),
        "loudness": outcome,
        "verify": vr,
        "ab": ab,
        "notes": plan.notes,
        "plan": plan,
        "stages_run": trace,
    });
    if let Some(rp) = a.path("report") {
        let bytes = serde_json::to_vec_pretty(&report)?;
        ctx.write_file(&rp, &bytes, Some("report"))?;
    }
    ctx.metric("integrated_lufs", jnum(outcome.landed_lufs, 2));
    ctx.metric("true_peak_dbtp", jnum(outcome.true_peak_dbtp, 2));
    ctx.metric("limiter_gain_reduction_db", outcome.limiter_gain_reduction_db);
    ctx.metric("target_reached", outcome.reached);
    ctx.metric("verify_pass", vr.pass);
    ctx.note("genre", &plan.genre.name);
    ctx.note("intensity", &plan.intensity.name);
    dedupe_warnings(ctx);
    Ok(report)
}

fn write_ab(ctx: &mut OpCtx, plan: &Plan, input: &AudioBuffer, master: &AudioBuffer) -> Result<Value> {
    // the "before" gets the same trim and rate change as the master so the two line up
    let mut before = input.clone();
    for s in plan.stages.iter().filter(|s| s.op == "silence trim" || s.op == "resample") {
        before = run_stage(ctx, s, before)?.0;
    }
    let lb = loudness::integrated_lufs(&before);
    let la = loudness::integrated_lufs(master);
    if !lb.is_finite() || !la.is_finite() {
        ctx.warn("ab_skipped", "cannot loudness-match a silent file; no A/B pair written");
        return Ok(Value::Null);
    }
    // match both to the quieter of the two, so neither is turned up into clipping
    let common = lb.min(la);
    before.scale(db_to_lin(common - lb) as f32);
    let mut after = master.clone();
    after.scale(db_to_lin(common - la) as f32);
    let eo = EncodeOptions {
        format: Some("wav".into()),
        bit_depth: Some(32),
        float: true,
        seed: ctx.globals.seed,
        ..Default::default()
    };
    let pb = role_path(ctx, "ab-before", "wav")?;
    let pa = role_path(ctx, "ab-after", "wav")?;
    ctx.write_audio_with(&pb, &before, Some("ab-before"), &eo)?;
    ctx.write_audio_with(&pa, &after, Some("ab-after"), &eo)?;
    let diff = (loudness::integrated_lufs(&before) - loudness::integrated_lufs(&after)).abs();
    ctx.metric("ab_loudness_difference_lu", round_to(diff, 3));
    Ok(json!({
        "before": pb.display().to_string(),
        "after": pa.display().to_string(),
        "matched_lufs": round_to(common, 2),
        "loudness_difference_lu": round_to(diff, 3),
    }))
}

// ---------------------------------------------------------------------------------------
// master analyze / plan
// ---------------------------------------------------------------------------------------

fn master_analyze(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let input_path = ctx.input(0)?.to_path_buf();
    let info = probe_info(ctx, &input_path);
    let buf = ctx.read_audio(0)?;
    if buf.is_empty() {
        return Err(Error::input("the file has no audio"));
    }
    let target = targets::resolve(a, buf.channels())?;
    let reference = read_reference(a, ctx)?;
    let m = analyze::measure(&buf, reference.as_ref().map(|r| &r.1));
    let path = input_path.display().to_string();
    let req = AnalyzeRequest {
        buf: &buf,
        path: &path,
        bit_depth: info.and_then(|i| i.bit_depth),
        reference: reference.as_ref().map(|(p, b)| (p.as_str(), b)),
        genre: &a.str_or("genre", "auto"),
        target: &target,
        registry: ctx.registry,
    };
    let report = analyze::report(&req, &m);
    ctx.metric("integrated_lufs", report["loudness"]["integrated_lufs"].clone());
    ctx.metric("true_peak_dbtp", report["loudness"]["true_peak_dbtp"].clone());
    ctx.metric("looks_mastered", report["context"]["looks_mastered"].clone());
    if ctx.globals.output.is_some() {
        let p = ctx.output_path(None, "json")?;
        ctx.write_file(&p, &serde_json::to_vec_pretty(&report)?, Some("report"))?;
    }
    Ok(report)
}

fn master_plan(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let input = ctx.read_audio(0)?;
    if input.is_empty() {
        return Err(Error::input("the mix has no audio"));
    }
    let opts = options_from_args(a, ctx, input.channels())?;
    let m = analyze::measure(&input, opts.reference.as_ref().map(|r| &r.1));
    let built = chain::build(ctx, input, &m, &opts)?;
    let plan = serde_json::to_value(&built.plan)?;
    if ctx.globals.output.is_some() {
        let p = ctx.output_path(None, "json")?;
        ctx.write_file(&p, &serde_json::to_vec_pretty(&plan)?, Some("plan"))?;
    }
    ctx.metric("stages", built.plan.stages.len());
    ctx.metric("expected_integrated_lufs", built.loudness.landed_lufs);
    ctx.metric("expected_limiter_gain_reduction_db", built.loudness.limiter_gain_reduction_db);
    dedupe_warnings(ctx);
    Ok(plan)
}

// ---------------------------------------------------------------------------------------
// master verify
// ---------------------------------------------------------------------------------------

fn master_verify(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let input_path = ctx.input(0)?.to_path_buf();
    let info = probe_info(ctx, &input_path);
    let buf = ctx.read_audio(0)?;
    if buf.is_empty() {
        return Err(Error::input("the file has no audio"));
    }
    let target = targets::resolve(a, buf.channels())?;
    let tol = verify::parse_tolerance(&a.str_or("tolerance", "0.5LU"))?;
    let r = verify::verify(&buf, info.as_ref(), &target, tol);
    let v = serde_json::to_value(&r)?;
    ctx.metric("pass", r.pass);
    ctx.metric("checks", v["checks"].clone());
    ctx.metric("failed_checks", json!(r.failed));
    for w in &r.warnings {
        ctx.warn("verify_warning", w.clone());
    }
    if !r.pass {
        return Err(Error::verification(format!(
            "{} fails the {} spec: {}",
            input_path.display(),
            target.name,
            r.failure_lines().join("; ")
        ))
        .with_hint(format!(
            "ferraus master {} --target {} -o fixed.wav",
            input_path.display(),
            target.name
        )));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------------------
// master deliver
// ---------------------------------------------------------------------------------------

fn master_deliver(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let keys = a.list("formats");
    if keys.is_empty() {
        return Err(Error::usage("--formats needs at least one of wav24, wav16, mp3, flac, aac"));
    }
    let mut fmts = Vec::new();
    for k in &keys {
        let f = deliver::format(&k.to_ascii_lowercase()).ok_or_else(|| {
            Error::usage(format!("unknown delivery format '{k}'; use wav24, wav16, mp3, flac, aac"))
        })?;
        if !fmts.iter().any(|x: &&deliver::DeliveryFormat| x.key == f.key) {
            fmts.push(f);
        }
    }
    let missing: Vec<&str> = fmts.iter().filter(|f| !ctx.io.can_encode(f.ext)).map(|f| f.key).collect();
    if !missing.is_empty() {
        return Err(Error::unsupported(format!(
            "this build cannot encode {} (nothing was written)",
            missing.join(", ")
        ))
        .with_hint("ferraus doctor   # aac and some mp3 builds need FFmpeg on PATH"));
    }
    let mut tags = deliver::parse_tags(&a.all("tags"))?;
    if let Some(isrc) = a.str("isrc").filter(|s| !s.trim().is_empty()) {
        tags.insert("isrc".into(), deliver::parse_isrc(&isrc)?);
    }
    let master = ctx.read_audio(0)?;
    if master.is_empty() {
        return Err(Error::input("the master has no audio"));
    }
    let mut rows = Vec::new();
    let mut cd_cache: Option<(AudioBuffer, Option<String>)> = None;
    for f in &fmts {
        let (buf, note) = if f.cd_rate && master.sample_rate != 44100 {
            if cd_cache.is_none() {
                cd_cache = Some(deliver::to_cd_rate(&master));
            }
            let c = cd_cache.as_ref().unwrap();
            (c.0.clone(), c.1.clone())
        } else {
            (master.clone(), None)
        };
        if let Some(n) = &note {
            ctx.note(&format!("{}_resample", f.key), n);
        }
        let path: PathBuf = role_path(ctx, f.key, f.ext)?;
        let eo = deliver::encode_options(f, ctx, &tags);
        ctx.write_audio_with(&path, &buf, Some(f.key), &eo)?;
        let (measured, how) = if !ctx.globals.dry_run {
            match ctx.io.read(&path) {
                Ok((b, _)) => (b, "decoded deliverable"),
                Err(_) => (buf.clone(), "before encoding"),
            }
        } else {
            (buf.clone(), "before encoding")
        };
        let mut row = deliver::measure_row(f.key, &path, &measured, f.bit_depth, eo.bitrate, how);
        if let Some(n) = note {
            row["note"] = json!(n);
        }
        rows.push(row);
    }
    let mut sheet = Value::Null;
    if a.flag("loudness-sheet") {
        let pj = role_path(ctx, "loudness", "json")?;
        let pc = role_path(ctx, "loudness", "csv")?;
        let doc = json!({"master": ctx.input(0)?.display().to_string(), "tags": tags, "deliverables": rows});
        ctx.write_file(&pj, &serde_json::to_vec_pretty(&doc)?, Some("loudness-sheet"))?;
        ctx.write_file(&pc, deliver::sheet_csv(&rows).as_bytes(), Some("loudness-sheet-csv"))?;
        sheet = json!({"json": pj.display().to_string(), "csv": pc.display().to_string()});
    }
    ctx.metric("deliverables", rows.len());
    Ok(json!({"deliverables": rows, "tags": tags, "loudness_sheet": sheet}))
}
