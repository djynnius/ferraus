//! # ferraus-denoise
//!
//! Noise reduction: learn a noise fingerprint from a region of the recording, then remove
//! it from the whole file with a soft spectral gate; or track the noise floor adaptively;
//! or run a speech model (RNNoise built in, DeepFilterNet 3 through its model slot).
//!
//! Commands: `denoise`, `denoise profile`, `denoise profile merge`.
//!
//! ## DeepFilterNet 3
//!
//! The crates.io `deep_filter` crate (0.2.x) ships no inference code; the runtime with the
//! bundled DF3 model lives only in the upstream repository and needs tract pinned to
//! 0.21.4. A git dependency is not allowed by the workspace source policy, so `--model
//! dfn3` resolves through the model slot `denoise/dfn3` and fails explicitly (exit 5 with a
//! `ferraus models pull` hint, or "no runtime in this build" when cached). It never falls
//! back to another model silently.

pub mod engine;
pub mod metrics;
pub mod ml;
pub mod profile;
pub mod stft;

use engine::{Engine, GateConfig, NoiseModel};
use ferraus_core::exec::blend;
use ferraus_core::prelude::*;
use profile::{NoiseProfile, ProfileSource, QuietRegion};
use std::path::Path;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    vec![denoise_spec(), profile_spec(), merge_spec()]
}

fn denoise_spec() -> OpSpec {
    OpSpec::transform(
        "denoise",
        Category::Clean,
        "Remove steady background noise (hiss, hum, fan, room tone) or run a speech denoiser",
        denoise,
    )
    .about(
        "Learns what the noise sounds like from a noise-only region (a saved profile, \
         --noise-range, or an automatically found quiet second) and subtracts that \
         fingerprint from the whole file with a soft spectral gate that avoids the \
         chirping 'musical noise' of simple gates. --mode adaptive tracks a changing \
         noise floor with no profile; --mode ml runs a speech model (RNNoise built in, \
         DeepFilterNet 3 through its model slot); --mode hybrid runs the profile pass and \
         then the model on what remains. With no options it finds the quietest second \
         and learns the noise from it. --reduction 0dB returns the input exactly.",
    )
    .param(param("profile", ParamKind::Path).help(
        "Noise profile (.noiseprof JSON) learned with 'ferraus denoise profile'",
    ))
    .param(param("noise-range", ParamKind::Range).help(
        "One shot: learn the noise from this noise-only region of the input, e.g. 0s..2s",
    ))
    .param(param("auto", ParamKind::Flag).help(
        "One shot: find the quietest spectrally stable 1 s region and learn the noise from it (reported as noise_region in --json)",
    ))
    .param(
        param("mode", ParamKind::Choice)
            .choices(&["profile", "adaptive", "ml", "hybrid"])
            .default("profile")
            .help("profile: spectral gate from a noise profile; adaptive: minimum-statistics tracking with no profile; ml: speech model; hybrid: profile pass, then the model"),
    )
    .param(param("adaptive", ParamKind::Flag).help("Same as --mode adaptive"))
    .param(param("reduction", ParamKind::Gain).default("12dB").range("0..60").help(
        "How far noise is pushed down, in dB (0dB leaves the input unchanged)",
    ))
    .param(param("sensitivity", ParamKind::Float).default("6").range("0..48").help(
        "Gate threshold above the noise profile: threshold = mean + sensitivity × std, scaled so 6 means 6 dB above steady hiss; higher removes more",
    ))
    .param(param("smoothing", ParamKind::Int).default("3").range("0..32").help(
        "Smooth the gain mask across ± this many frequency bins (fewer chirps, slightly softer edges)",
    ))
    .param(param("attack", ParamKind::Time).default("20ms").help(
        "How fast the gate opens (a matching lookahead keeps onsets intact), e.g. 20ms",
    ).advanced())
    .param(param("release", ParamKind::Time).default("120ms").help(
        "How fast the gate closes after a sound ends, e.g. 120ms",
    ).advanced())
    .param(param("artifact-control", ParamKind::Percent).default("50%").help(
        "Blend in a median-filtered mask and a steady noise floor: higher removes more chirping at the cost of a little more residual noise",
    ).advanced())
    .param(param("link", ParamKind::Switch).default("on").help(
        "on: one gain mask for all channels so the stereo image stays put; off: per-channel masks",
    ).advanced())
    .param(param("preserve-transients", ParamKind::Switch).default("on").help(
        "Detect onsets and hold the gate open through them so attacks stay crisp",
    ).advanced())
    .param(param("noisiness", ParamKind::Percent).default("30%").help(
        "Adaptive mode: how much of the audio is noise; higher sets the threshold further above the tracked floor (20 dB × noisiness)",
    ).advanced())
    .param(param("decay", ParamKind::Percent).default("65%").help(
        "Adaptive mode: how slowly reduction relaxes after a sound (release = 200 ms × decay unless --release is given)",
    ).advanced())
    .param(param("floor", ParamKind::Gain).default("0dB").help(
        "Adaptive mode: shift the tracked noise floor in dB (positive removes more)",
    ).advanced())
    .param(
        param("model", ParamKind::Choice)
            .choices(&["dfn3", "rnnoise", "music"])
            .default("dfn3")
            .help("ml/hybrid modes: dfn3 = DeepFilterNet 3 (48 kHz, speech; model slot denoise/dfn3), rnnoise = RNNoise (fast, light, built in, speech), music = music-aware slot denoise/music-*"),
    )
    .param(param("strength", ParamKind::Percent).default("100%").help(
        "ml/hybrid modes: how much of the model's change to apply",
    ))
    .param(param("residual", ParamKind::Path).help(
        "Also write the removed component (input minus the fully processed output, before --mix) to this audio file",
    ))
    .param(param("dereverb", ParamKind::Flag).help(
        "Add the best-effort de-reverb stage (model slot denoise/dereverb-*)",
    ).advanced())
    .example(
        "ferraus denoise take.wav --auto -o clean.wav",
        "Find the quietest second, learn the noise from it, and remove it",
    )
    .example(
        "ferraus denoise take.wav --profile room.noiseprof --reduction 18dB -o clean.wav",
        "Apply a saved room profile with stronger reduction",
    )
    .example(
        "ferraus denoise interview.wav --noise-range 0s..1.5s --residual removed.wav -o clean.wav",
        "One shot from a noise-only lead-in, and keep what was removed for listening",
    )
    .example(
        "ferraus denoise street.wav --adaptive --reduction 10dB -o clean.wav",
        "Follow a changing background (traffic, wind) with no profile",
    )
    .example(
        "ferraus denoise call.wav --mode ml --model rnnoise -o clean.wav",
        "Speech denoising with the built-in RNNoise model",
    )
}

fn profile_spec() -> OpSpec {
    OpSpec::custom(
        "denoise profile",
        Category::Clean,
        "Learn a noise profile (.noiseprof) from a noise-only region",
        denoise_profile,
    )
    .about(
        "Measures the noise fingerprint (per-frequency mean and spread of the level) from \
         a region that contains only noise, and saves it as a small JSON file that \
         'ferraus denoise --profile' applies. Choose the region with --range (default \
         0s..2s), let ferraus find the quietest spectrally stable second with --auto, or \
         learn from a separate noise-only recording with --from.",
    )
    .inputs(Inputs { min: 0, max: Some(1), kind: InputKind::Audio })
    .param(
        param("range", ParamKind::Range)
            .default("0s..2s")
            .help("Noise-only region of the input, e.g. 0s..2s or 1:02..1:04"),
    )
    .param(
        param("auto", ParamKind::Flag)
            .help("Find the quietest spectrally stable 1 s window and learn from it (the window is reported)"),
    )
    .param(
        param("from", ParamKind::Path)
            .help("Learn from this whole noise-only recording instead of a region of the input"),
    )
    .example(
        "ferraus denoise profile take.wav --range 0s..2s -o room.noiseprof",
        "Learn the room tone from the first two seconds",
    )
    .example(
        "ferraus denoise profile take.wav --auto -o room.noiseprof --json",
        "Let ferraus find the quietest second and report where it was",
    )
    .example(
        "ferraus denoise profile --from roomtone.wav -o room.noiseprof",
        "Learn from a separate room-tone recording",
    )
}

fn merge_spec() -> OpSpec {
    OpSpec::custom(
        "denoise profile merge",
        Category::Clean,
        "Combine noise profiles from several takes into one",
        denoise_profile_merge,
    )
    .about(
        "Merges two or more .noiseprof files learned at the same sample rate. --method max \
         keeps the louder noise of every bin (removes the worst noise seen in any take); \
         --method mean pools the statistics as if all the noise had been profiled at once.",
    )
    .inputs(Inputs { min: 2, max: None, kind: InputKind::Any })
    .param(
        param("method", ParamKind::Choice)
            .choices(&["max", "mean"])
            .default("max")
            .help("max: per-bin maximum of mean and spread; mean: frame-weighted pooled statistics"),
    )
    .example(
        "ferraus denoise profile merge a.noiseprof b.noiseprof -o c.noiseprof",
        "Keep the louder noise of two takes in every band",
    )
    .example(
        "ferraus denoise profile merge day1.noiseprof day2.noiseprof day3.noiseprof --method mean -o room.noiseprof",
        "Average the room tone of three sessions",
    )
}

// ---------------------------------------------------------------------------------
// denoise
// ---------------------------------------------------------------------------------

fn gate_config(a: &Args) -> Result<GateConfig> {
    let reduction = a.db("reduction")?.abs() as f32;
    let sensitivity = a.f64("sensitivity")?;
    if !(0.0..=48.0).contains(&sensitivity) {
        return Err(Error::usage("--sensitivity must be between 0 and 48"));
    }
    Ok(GateConfig {
        reduction_db: reduction,
        sensitivity: sensitivity as f32,
        smoothing: a.usize("smoothing")?.min(64),
        attack_s: a.seconds("attack")? as f32,
        release_s: a.seconds("release")? as f32,
        artifact_control: a.percent("artifact-control")?.clamp(0.0, 1.0) as f32,
        link: a.switch("link", true),
        preserve_transients: a.switch("preserve-transients", true),
    })
}

fn resolve_mode(a: &Args) -> Result<String> {
    let mode = a.choice("mode");
    if a.flag("adaptive") {
        if a.is_explicit("mode") && mode != "adaptive" {
            return Err(Error::usage(format!("--adaptive conflicts with --mode {mode}; pass one of them")));
        }
        return Ok("adaptive".into());
    }
    Ok(mode)
}

fn region_json(r: &QuietRegion, method: &str) -> Value {
    json!({
        "start_s": round_s(r.start_s),
        "end_s": round_s(r.end_s),
        "range": format!("{:.3}s..{:.3}s", r.start_s, r.end_s),
        "level_db": (r.level_db * 10.0).round() / 10.0,
        "stability_db": (r.stability_db * 100.0).round() / 100.0,
        "method": method,
    })
}

fn round_s(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn load_profile(ctx: &mut OpCtx, path: &Path) -> Result<NoiseProfile> {
    ctx.check_allowed(path)?;
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("cannot read noise profile {}: {e}", path.display()),
        )
    })?;
    ctx.note_input(path);
    NoiseProfile::from_json(&bytes)
}

/// Where the noise profile for the profile/hybrid passes comes from.
fn resolve_profile(a: &Args, buf: &AudioBuffer, ctx: &mut OpCtx) -> Result<Option<NoiseProfile>> {
    let given = [a.has("profile"), a.has("noise-range"), a.flag("auto")];
    if given.iter().filter(|g| **g).count() > 1 {
        return Err(Error::usage("choose one noise source: --profile, --noise-range, or --auto"));
    }
    if let Some(p) = a.path("profile") {
        let prof = load_profile(ctx, &p)?;
        if prof.sample_rate != buf.sample_rate {
            return Err(Error::usage(format!(
                "the profile was learned at {} Hz but the input is {} Hz",
                prof.sample_rate, buf.sample_rate
            ))
            .with_hint(format!(
                "ferraus convert <input> --sample-rate {} -o converted.wav (or learn a new profile from this file with --auto)",
                prof.sample_rate
            )));
        }
        if prof.channels != buf.channels() {
            ctx.warn(
                "profile_channels",
                format!(
                    "the profile has {} channel(s) but the input has {}; using the profile's linked statistics for every channel",
                    prof.channels,
                    buf.channels()
                ),
            );
        }
        ctx.note("noise_profile", json!({"path": p.display().to_string(), "frames": prof.frames}));
        return Ok(Some(prof));
    }
    if let Some(r) = a.range("noise-range")? {
        let (s, e) = r.frames(buf.sample_rate, buf.frames(), ctx.bpm())?;
        let src = ProfileSource {
            method: "range".into(),
            start_s: Some(round_s(s as f64 / buf.sample_rate as f64)),
            end_s: Some(round_s(e as f64 / buf.sample_rate as f64)),
            ..Default::default()
        };
        let prof = profile::learn(buf, s, e, src)?;
        ctx.note(
            "noise_region",
            json!({
                "start_s": round_s(s as f64 / buf.sample_rate as f64),
                "end_s": round_s(e as f64 / buf.sample_rate as f64),
                "level_db": (prof.mean_level_db() * 10.0).round() / 10.0,
                "method": "range",
            }),
        );
        return Ok(Some(prof));
    }
    if !a.flag("auto") {
        ctx.warn(
            "no_noise_profile",
            "no --profile, --noise-range, or --auto given: learning the noise from the quietest 1 s region (pass --auto to make this explicit)",
        );
    }
    // the PRD window is 1 s; very short inputs use half their length
    let window = if buf.duration_s() < 2.0 { (buf.duration_s() / 2.0).clamp(0.05, 1.0) } else { 1.0 };
    match profile::find_quiet_region(buf, window) {
        None => {
            ctx.warn("input_too_short", "the input is too short to estimate the noise; it was left unchanged");
            Ok(None)
        }
        Some(r) => {
            if window < 1.0 {
                ctx.warn(
                    "short_noise_region",
                    format!("the input is short, so the noise was learned from a {window:.2} s window"),
                );
            }
            let src = ProfileSource {
                method: "auto".into(),
                start_s: Some(round_s(r.start_s)),
                end_s: Some(round_s(r.end_s)),
                ..Default::default()
            };
            let prof = profile::learn(buf, r.start, r.end, src)?;
            ctx.note("noise_region", region_json(&r, "auto"));
            Ok(Some(prof))
        }
    }
}

fn run_engine(cfg: GateConfig, model: &NoiseModel, buf: &AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    if cfg.reduction_db == 0.0 {
        // exact identity, bit for bit
        return Ok(buf.clone());
    }
    let mut eng = Engine::new(cfg, model, buf.sample_rate, buf.channels());
    let out = {
        let c: &OpCtx = ctx;
        eng.process_buffer(buf, &mut |f| {
            c.set_progress(f);
            c.check_cancel()
        })?
    };
    ctx.metric("mean_gain_db", (eng.stats.mean_gain_db * 100.0).round() / 100.0);
    ctx.metric("onsets", eng.stats.onsets);
    ctx.metric("fft_size", eng.fft_size());
    Ok(out)
}

fn model_pass(a: &Args, buf: &AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let model = a.choice("model");
    let strength = a.percent("strength")?.clamp(0.0, 1.0) as f32;
    let y = match model.as_str() {
        "rnnoise" => {
            let y = ml::rnnoise(buf)?;
            ctx.model_used(ml::RNNOISE_NAME, ml::RNNOISE_VERSION, ml::RNNOISE_SHA256);
            y
        }
        "dfn3" => return Err(ml::dfn3_unavailable()),
        "music" => {
            return Err(ml::unavailable_slot(
                "denoise/music-",
                "denoise/music-roformer",
                "--model music (music-aware denoise)",
            ))
        }
        other => return Err(Error::usage(format!("unknown --model '{other}'"))),
    };
    ctx.warn(
        "speech_model",
        format!("--model {model} is a speech-tuned model; on music the default profile path (--mode profile) usually sounds better"),
    );
    ctx.note("model_domain", ml::speech_note(&model));
    Ok(if strength >= 1.0 { y } else { blend(buf, &y, strength) })
}

fn denoise(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let mode = resolve_mode(a)?;
    if a.flag("dereverb") {
        // fail fast, before any processing
        return Err(ml::unavailable_slot(
            "denoise/dereverb-",
            "denoise/dereverb-roformer",
            "--dereverb (best-effort de-reverb stage)",
        ));
    }
    ctx.note("mode", mode.as_str());
    if buf.frames() == 0 {
        return Ok(buf);
    }
    let mut cfg = gate_config(a)?;
    let out = match mode.as_str() {
        "profile" => match resolve_profile(a, &buf, ctx)? {
            None => buf.clone(),
            Some(p) => {
                ctx.metric("noise_floor_db", (p.mean_level_db() * 10.0).round() / 10.0);
                run_engine(cfg, &NoiseModel::Profile(Box::new(p)), &buf, ctx)?
            }
        },
        "adaptive" => {
            if a.has("profile") || a.has("noise-range") || a.flag("auto") {
                ctx.warn(
                    "ignored_profile",
                    "adaptive mode tracks the noise itself; --profile/--noise-range/--auto were ignored",
                );
            }
            if !a.is_explicit("release") {
                cfg.release_s = 0.2 * a.percent("decay")?.clamp(0.0, 1.0) as f32;
            }
            let model =
                NoiseModel::Adaptive { noisiness: a.percent("noisiness")? as f32, floor_db: a.db("floor")? as f32 };
            run_engine(cfg, &model, &buf, ctx)?
        }
        "ml" => model_pass(a, &buf, ctx)?,
        "hybrid" => {
            let first = match resolve_profile(a, &buf, ctx)? {
                None => buf.clone(),
                Some(p) => run_engine(cfg, &NoiseModel::Profile(Box::new(p)), &buf, ctx)?,
            };
            model_pass(a, &first, ctx)?
        }
        other => return Err(Error::usage(format!("unknown --mode '{other}'"))),
    };
    // residual = what was removed
    let mut residual = buf.clone();
    for (rc, oc) in residual.data.iter_mut().zip(&out.data) {
        for (r, o) in rc.iter_mut().zip(oc) {
            *r -= o;
        }
    }
    let rms_db = |b: &AudioBuffer| (lin_to_db(b.rms() as f64) * 10.0).round() / 10.0;
    ctx.metric("input_rms_dbfs", rms_db(&buf));
    ctx.metric("output_rms_dbfs", rms_db(&out));
    ctx.metric("removed_rms_dbfs", rms_db(&residual));
    if let Some(rp) = a.path("residual") {
        ctx.write_audio(&rp, &residual, Some("residual"))?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------
// denoise profile
// ---------------------------------------------------------------------------------

fn denoise_profile(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let auto = a.flag("auto");
    let from = a.path("from");
    let range_explicit = a.is_explicit("range");
    if [auto, from.is_some(), range_explicit].iter().filter(|x| **x).count() > 1 {
        return Err(Error::usage("choose one noise source: --range, --auto, or --from"));
    }
    if from.is_none() && ctx.inputs.is_empty() {
        return Err(Error::usage("'denoise profile' needs an input file (or --from <noise-only file>)")
            .with_hint("ferraus denoise profile take.wav --range 0s..2s -o room.noiseprof"));
    }
    let out_path = ctx.output_path(None, "noiseprof")?;
    let (prof, region) = if let Some(f) = &from {
        let buf = ctx.read_audio_path(f)?;
        if !ctx.inputs.is_empty() {
            if let Ok(info) = ctx.io.probe(ctx.input(0)?) {
                if info.sample_rate != buf.sample_rate {
                    ctx.warn(
                        "sample_rate_mismatch",
                        format!(
                            "the noise file is {} Hz but the input is {} Hz; the profile only applies to {} Hz audio",
                            buf.sample_rate, info.sample_rate, buf.sample_rate
                        ),
                    );
                }
            }
        }
        let src = ProfileSource {
            method: "from".into(),
            path: Some(f.display().to_string()),
            start_s: Some(0.0),
            end_s: Some(round_s(buf.duration_s())),
            ..Default::default()
        };
        let p = profile::learn(&buf, 0, buf.frames(), src)?;
        let region = json!({"start_s": 0.0, "end_s": round_s(buf.duration_s()), "method": "from"});
        (p, region)
    } else {
        let buf = ctx.read_audio(0)?;
        let input = ctx.input(0)?.display().to_string();
        if auto {
            let r = profile::find_quiet_region(&buf, 1.0)
                .ok_or_else(|| Error::usage("the input is shorter than one analysis frame; nothing to learn from"))?;
            if r.end_s - r.start_s < 0.99 {
                ctx.warn(
                    "short_noise_region",
                    format!("the input is shorter than 1 s; learned from {:.2} s", r.end_s - r.start_s),
                );
            }
            let src = ProfileSource {
                method: "auto".into(),
                path: Some(input),
                start_s: Some(round_s(r.start_s)),
                end_s: Some(round_s(r.end_s)),
                ..Default::default()
            };
            let p = profile::learn(&buf, r.start, r.end, src)?;
            let region = region_json(&r, "auto");
            ctx.note("noise_region", region.clone());
            (p, region)
        } else {
            let r = a.range("range")?.ok_or_else(|| Error::usage("missing --range"))?;
            let (s, e) = r.frames(buf.sample_rate, buf.frames(), ctx.bpm())?;
            let src = ProfileSource {
                method: "range".into(),
                path: Some(input),
                start_s: Some(round_s(s as f64 / buf.sample_rate as f64)),
                end_s: Some(round_s(e as f64 / buf.sample_rate as f64)),
                ..Default::default()
            };
            let p = profile::learn(&buf, s, e, src)?;
            let region = json!({
                "start_s": round_s(s as f64 / buf.sample_rate as f64),
                "end_s": round_s(e as f64 / buf.sample_rate as f64),
                "method": "range",
            });
            (p, region)
        }
    };
    ctx.write_file(&out_path, &prof.to_json(), None)?;
    ctx.metric("noise_floor_db", (prof.mean_level_db() * 10.0).round() / 10.0);
    Ok(profile_result(&prof, &out_path, Some(region)))
}

fn profile_result(p: &NoiseProfile, path: &Path, region: Option<Value>) -> Value {
    let mut v = json!({
        "profile": path.display().to_string(),
        "sample_rate": p.sample_rate,
        "fft_size": p.fft_size,
        "hop_size": p.hop_size,
        "channels": p.channels,
        "frames": p.frames,
        "mean_level_db": (p.mean_level_db() * 10.0).round() / 10.0,
        "method": p.source.method,
    });
    if let Some(r) = region {
        v["noise_region"] = r;
    }
    v
}

// ---------------------------------------------------------------------------------
// denoise profile merge
// ---------------------------------------------------------------------------------

fn denoise_profile_merge(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let method = profile::MergeMethod::parse(&a.choice("method"))?;
    let out_path = ctx.output_path(None, "noiseprof")?;
    let paths: Vec<std::path::PathBuf> = ctx.inputs.clone();
    let mut profiles = Vec::with_capacity(paths.len());
    let mut names = Vec::with_capacity(paths.len());
    for p in &paths {
        profiles.push(load_profile(ctx, p)?);
        names.push(p.display().to_string());
    }
    let (merged, warnings) = profile::merge(&profiles, &names, method)?;
    for w in warnings {
        ctx.warn("profile_channels", w);
    }
    ctx.write_file(&out_path, &merged.to_json(), None)?;
    let mut v = profile_result(&merged, &out_path, None);
    v["merged_from"] = json!(names);
    Ok(v)
}
