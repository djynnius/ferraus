//! ferraus-separate: the `separate` command.
//!
//! Tiers map to registry models (`fast` = Demucs v4 `htdemucs`, ...); `--model` pins one
//! model (a registry name or an ONNX file path), `--ensemble` builds a custom ensemble. The
//! pipeline (see [`pipeline`]) resamples to the model rate, runs windowed inference with a
//! raised-cosine crossfade, averages test-time-augmentation passes, combines ensemble members
//! in the magnitude domain, assigns the residual to `other` so the stems sum back to the mix
//! on the output sample grid, runs cascades, and writes each stem as soon as it is final.
//!
//! The command itself runs the streaming versions in [`stream`]: models are opened one at a
//! time, each streams window by window over the mix, and every full-length signal (the mix,
//! member estimates, final stems) lives in memory or in a temp spill file as `--max-memory`
//! requires, so a 60-minute input needs O(model + a few blocks) of memory.

pub mod adapter;
pub mod pipeline;
pub mod stft;
pub mod stream;

use adapter::SeparationModel;
use ferraus_core::prelude::*;
use ferraus_core::EncodeOptions;
use ferraus_models::{active_registry, ensure_model_with, not_cached_error, Cache, EnsureOptions, Manifest, ModelSpec};
use pipeline::{assign_residual, RunOptions, Segment};
use stream::{ConfidenceAcc, Track};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const STEMS_4: &[&str] = &["vocals", "drums", "bass", "other"];
pub const STEMS_6: &[&str] = &["vocals", "drums", "bass", "guitar", "piano", "other"];
pub const DRUM_PARTS: &[&str] = &["kick", "snare", "toms", "hh", "cymbals"];
pub const EXTENDED: &[&str] = &["guitar", "piano", "keys", "strings", "brass"];

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::custom(
        "separate",
        Category::Separate,
        "Split a mix into stems (vocals, drums, bass, other, ...)",
        separate,
    )
    .inputs(ferraus_core::op::Inputs::ONE_AUDIO)
    .about("Separates a song into stems with open models run through ONNX Runtime. Tiers trade speed for quality: fast (Demucs v4), balanced (Demucs v4 fine-tuned bag), quality (Mel-Band RoFormer vocals + fine-tuned Demucs), max (ensemble with test-time augmentation); the default, auto, picks the best tier whose models are verified. Models run one at a time over the song window by window, and long inputs spill to temporary files so the run stays within --max-memory. Models download on first use only with -y or models.auto_download = true; otherwise the command fails with exit code 5 and the exact `ferraus models pull` command. With --sum-to-mix on (default) the residual goes to `other`, so the written stems add back to the input within one LSB. Each stem's JSON result carries a quality estimate (ensemble agreement, or mask confidence for a single model).")
    .param(param("stems", ParamKind::Choice).choices(&["2", "4", "6"]).default("4").help("2 = vocals + instrumental, 4 = vocals/drums/bass/other, 6 = adds guitar and piano"))
    .param(param("tier", ParamKind::Choice).choices(&["auto", "fast", "balanced", "quality", "max"]).default("auto").help("Model tier from the registry; auto picks the best tier whose models are verified and downloadable in this registry (quality, then balanced, then fast) and reports the choice"))
    .param(param("only", ParamKind::List).help("Write only these stems, e.g. vocals,drums"))
    .param(param("instrumental", ParamKind::Flag).help("Write one file with the vocals removed"))
    .param(param("model", ParamKind::Str).help("Pin a registry model (name[@version]) or an ONNX file instead of a tier"))
    .param(param("ensemble", ParamKind::List).help("Custom ensemble of models, e.g. a,b,c"))
    .param(param("weights", ParamKind::List).help("Ensemble weights, e.g. 0.5,0.3,0.2 (default equal)"))
    .param(param("ensemble-mode", ParamKind::Choice).choices(&["avg", "max", "min"]).default("avg").help("avg = balanced, max = fuller (more bleed), min = cleaner (less bleed)"))
    .param(param("vocal-split", ParamKind::Flag).help("Second stage on the vocals: lead and backing"))
    .param(param("drum-split", ParamKind::Flag).help("Second stage on the drums: kick, snare, toms, hh, cymbals"))
    .param(param("extended", ParamKind::List).help("Best-effort extraction from other: guitar,piano,keys,strings,brass"))
    .param(param("overlap", ParamKind::Percent).default("25%").help("Window overlap, crossfaded with a raised-cosine window"))
    .param(param("shifts", ParamKind::Int).default("1").range("1..10").help("Test-time augmentation passes with random sub-second offsets (max tier uses 2)"))
    .param(param("segment", ParamKind::Str).default("auto").help("Window length: auto (the model's trained length), a duration such as 10s, or whole"))
    .param(param("sum-to-mix", ParamKind::Switch).default("on").help("Assign the residual to other so stems add back to the input exactly"))
    .param(param("accept-license", ParamKind::Flag).help("Accept a research-only model license (with --model)"))
    .param(param("cascade-models", ParamKind::List).advanced().help("Override cascade models: vocal-split=<model>, drum-split=<model>, extended=<model>"))
    .example("ferraus separate song.wav", "Four stems with the default tier (auto: the best verified tier, quality today): song.vocals.wav, song.drums.wav, ...")
    .example("ferraus separate song.wav --tier fast --stems 2", "Quick vocals + instrumental")
    .example("ferraus separate song.wav --only vocals --tier max", "Best possible vocal stem")
    .example("ferraus separate song.wav --ensemble separate/htdemucs,separate/htdemucs-6s --weights 0.6,0.4", "Custom ensemble")]
}

/// Where a member's model comes from.
#[derive(Clone, Debug)]
enum ModelRef {
    Registry(String),
    File(PathBuf),
}

#[derive(Clone, Debug)]
struct Member {
    reference: ModelRef,
    weight: f64,
    stems: Vec<String>,
}

fn parse_ref(s: &str) -> ModelRef {
    let p = Path::new(s);
    if s.ends_with(".onnx") || p.exists() {
        ModelRef::File(p.to_path_buf())
    } else {
        ModelRef::Registry(s.to_string())
    }
}

fn label(r: &ModelRef) -> String {
    match r {
        ModelRef::Registry(n) => n.clone(),
        ModelRef::File(p) => p.display().to_string(),
    }
}

/// Canonical stem names (`no_vocals`, `accompaniment` → `instrumental`).
fn canonical(stem: &str) -> String {
    match stem.to_ascii_lowercase().as_str() {
        "no_vocals" | "accompaniment" | "instrument" | "inst" => "instrumental".into(),
        "vocal" | "voice" => "vocals".into(),
        "hihat" | "hi-hat" | "hihats" => "hh".into(),
        "toms" | "tom" => "toms".into(),
        other => other.to_string(),
    }
}

/// Spec for an ONNX file given with `--model path`: `<file>.spec.json`, `spec.json` beside
/// it, or a default inferred from the Demucs convention.
fn spec_for_file(ctx: &mut OpCtx, path: &Path) -> Result<ModelSpec> {
    let sidecar = path.with_extension("spec.json");
    let beside = path.with_file_name("spec.json");
    for p in [sidecar, beside] {
        if p.exists() {
            return ModelSpec::load(&p);
        }
    }
    ctx.warn(
        "model_spec_assumed",
        format!(
            "{} has no spec.json; assuming a 44.1 kHz Demucs-style waveform model (drums, bass, other, vocals)",
            path.display()
        ),
    );
    Ok(ModelSpec {
        name: path.display().to_string(),
        version: "local".into(),
        task: "separate".into(),
        sha256: None,
        sample_rate: 44_100,
        stems: vec!["drums".into(), "bass".into(), "other".into(), "vocals".into()],
        arch: "waveform".into(),
        inputs: vec![],
        outputs: vec![],
        chunk_seconds: None,
        chunk_samples: None,
        overlap: 0.25,
        context_samples: 0,
        provider_hints: vec![],
        license: Default::default(),
        params: Value::Null,
    })
}

/// Resolve the members of the run (explicit model, custom ensemble, or tier).
fn plan_members(
    ctx: &mut OpCtx,
    a: &Args,
    manifest: &Manifest,
    stems_n: &str,
) -> Result<(Vec<Member>, Option<ferraus_models::TierSpec>)> {
    if let Some(m) = a.str("model") {
        if a.has("ensemble") && !a.list("ensemble").is_empty() {
            return Err(Error::usage("--model and --ensemble are mutually exclusive"));
        }
        return Ok((vec![Member { reference: parse_ref(&m), weight: 1.0, stems: vec![] }], None));
    }
    let ens = a.list("ensemble");
    if !ens.is_empty() {
        let w = a.list("weights");
        let weights: Vec<f64> = if w.is_empty() {
            vec![1.0; ens.len()]
        } else {
            if w.len() != ens.len() {
                return Err(Error::usage(format!(
                    "--weights has {} values but --ensemble has {} models",
                    w.len(),
                    ens.len()
                )));
            }
            w.iter()
                .map(|v| {
                    v.trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|x| *x >= 0.0)
                        .ok_or_else(|| Error::usage(format!("--weights: '{v}' is not a non-negative number")))
                })
                .collect::<Result<_>>()?
        };
        return Ok((
            ens.iter()
                .zip(weights)
                .map(|(m, w)| Member { reference: parse_ref(m), weight: w, stems: vec![] })
                .collect(),
            None,
        ));
    }
    let mut tier = a.choice("tier");
    if tier == "auto" {
        tier = auto_tier(manifest, stems_n);
        if tier != "quality" {
            ctx.warn(
                "tier_fallback",
                format!("using the '{tier}' tier: the models of the better tiers are not yet verified for download in this registry (see 'ferraus models list')"),
            );
        }
        ctx.note("tier", format!("{tier} (auto)"));
    }
    let spec = manifest
        .tier("separate", &tier)
        .cloned()
        .ok_or_else(|| Error::usage(format!("the registry has no separate tier '{tier}'")))?;
    let members = if stems_n == "6" && !spec.members_6.is_empty() { &spec.members_6 } else { &spec.members };
    Ok((
        members
            .iter()
            .map(|m| Member {
                reference: ModelRef::Registry(m.model.clone()),
                weight: m.weight,
                stems: m.stems.iter().map(|s| canonical(s)).collect(),
            })
            .collect(),
        Some(spec),
    ))
}

/// The best tier whose every member is verified (has a SHA-256 and a URL, or is built in).
fn auto_tier(manifest: &Manifest, stems_n: &str) -> String {
    for t in ["quality", "balanced", "fast"] {
        let Some(spec) = manifest.tier("separate", t) else { continue };
        let members = if stems_n == "6" && !spec.members_6.is_empty() { &spec.members_6 } else { &spec.members };
        let ok = !members.is_empty()
            && members.iter().all(|m| {
                manifest
                    .resolve(&m.model)
                    .map(|e| e.is_builtin() || (e.sha256.is_some() && !e.urls.is_empty()))
                    .unwrap_or(false)
            });
        if ok {
            return t.to_string();
        }
    }
    "fast".to_string()
}

/// A member whose model file is present; its session is opened only while it runs.
struct Resolved {
    member: Member,
    path: PathBuf,
    spec: ModelSpec,
    label: String,
    /// The adapter's output stems (raw names, output order).
    stems: Vec<String>,
}

/// Ensure every member's model file. Missing registry models are collected into one exit-5 error.
fn resolve_members(ctx: &mut OpCtx, members: &[Member], accept: bool) -> Result<Vec<Resolved>> {
    let cache = Cache::default_root();
    let opts = EnsureOptions { accept_license: accept, cache: Some(cache), ..Default::default() };
    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for m in members {
        match &m.reference {
            ModelRef::File(p) => {
                ctx.check_allowed(p)?;
                if !p.exists() {
                    return Err(Error::input(format!("model file not found: {}", p.display())));
                }
                let spec = spec_for_file(ctx, p)?;
                let sha = ferraus_models::download::sha256_file(p)?;
                ctx.model_used(&p.display().to_string(), &spec.version, &sha);
                let stems = adapter::stems_of(&spec);
                resolved.push(Resolved { member: m.clone(), path: p.clone(), spec, label: p.display().to_string(), stems });
            }
            ModelRef::Registry(name) => match ensure_model_with(ctx, name, &opts) {
                Ok(c) => {
                    let label = c.entry.id();
                    let stems = adapter::stems_of(&c.spec);
                    resolved.push(Resolved { member: m.clone(), path: c.path, spec: c.spec, label, stems });
                }
                Err(e) if e.kind == ErrorKind::ModelNotCached => {
                    let id = active_registry(&Cache::default_root())
                        .manifest
                        .resolve(name)
                        .map(|e| e.id())
                        .unwrap_or_else(|_| name.clone());
                    missing.push(id);
                }
                Err(e) => return Err(e),
            },
        }
    }
    if !missing.is_empty() {
        missing.dedup();
        return Err(not_cached_error(&missing));
    }
    Ok(resolved)
}

/// Open one member's session (lean sessions skip constant folding to save memory).
fn open_model(ctx: &mut OpCtx, r: &Resolved, lean: bool) -> Result<Box<dyn SeparationModel>> {
    let session = ferraus_ml::session::load_for_with(ctx, &r.path, &r.spec.provider_hints, lean)?;
    adapter::open(&r.label, r.spec.clone(), session)
}

/// Free a member's session as soon as it is done (models run one after another).
fn release_model(model: Box<dyn SeparationModel>, r: &Resolved) {
    drop(model);
    ferraus_ml::session::evict(&r.path);
}

/// How the run fits `--max-memory`.
#[derive(Clone, Copy, Debug)]
struct MemPlan {
    budget: u64,
    /// Signals live in temp files instead of memory.
    spill: bool,
    /// Sessions skip ONNX Runtime's constant folding (smaller, slower).
    lean: bool,
    /// Block length in frames for streaming inference and assembly.
    block: usize,
    /// Estimated peak of the chosen plan.
    estimate: u64,
    fits: bool,
}

/// Measured ONNX Runtime peaks of a model (`io.params.ort_peak_mb`, `ort_lean_peak_mb`), bytes.
fn model_peaks(spec: &ModelSpec) -> (u64, u64) {
    let mb = |k: &str| spec.param_f64(k).map(|v| (v * 1024.0 * 1024.0) as u64);
    let folded = mb("ort_peak_mb").unwrap_or(0);
    (folded, mb("ort_lean_peak_mb").unwrap_or(folded))
}

/// Choose the cheapest way to stay under the budget: everything in memory, then spilled
/// signals, then lean sessions, then both. `tracks` counts every full-length signal (mix,
/// member estimates, final stems).
fn plan_memory(budget: u64, frames: usize, channels: usize, tracks: usize, peaks: &[(u64, u64)]) -> MemPlan {
    let ch = channels.max(1) as u64;
    let lanes = 2 * tracks.max(1) as u64;
    let block = ((budget / 16) / (ch * 4 * lanes)).clamp(1 << 14, 1 << 20) as usize;
    let ws = block as u64 * ch * 4 * lanes;
    let resident = stream::signal_bytes(frames, channels) * tracks as u64;
    let folded = peaks.iter().map(|p| p.0).max().unwrap_or(0);
    let lean = peaks.iter().map(|p| p.1).max().unwrap_or(0);
    for (spill, use_lean, need) in
        [(false, false, resident + folded), (true, false, folded), (false, true, resident + lean), (true, true, lean)]
    {
        if need + ws <= budget {
            return MemPlan { budget, spill, lean: use_lean, block, estimate: need + ws, fits: true };
        }
    }
    // the model alone exceeds the budget: run lean, and spill only when the signals are a
    // sizeable share of the budget (spilling a short song gains nothing)
    let spill = resident > budget / 4;
    let estimate = lean + ws + if spill { 0 } else { resident };
    MemPlan { budget, spill, lean: true, block, estimate, fits: false }
}

/// The integer grid of the output format (for exact sum-to-mix), `None` for float output.
fn output_grid(opts: &EncodeOptions) -> Option<f64> {
    let fmt = opts.format.as_deref().unwrap_or("wav");
    if opts.float || !matches!(fmt, "wav" | "flac" | "aiff" | "aif") {
        return None;
    }
    match opts.bit_depth.unwrap_or(24) {
        16 => Some((1u64 << 15) as f64),
        24 => Some((1u64 << 23) as f64),
        8 => Some(128.0),
        _ => None,
    }
}

struct Writer {
    ext: String,
    results: Vec<Value>,
    sum_to_mix_grid: bool,
}

impl Writer {
    fn opts(&self, ctx: &OpCtx, path: &Path) -> EncodeOptions {
        let mut o = ctx.encode_options(path);
        let fmt = o.format.clone().unwrap_or_else(|| self.ext.clone());
        if o.bit_depth.is_none() && matches!(fmt.as_str(), "wav" | "flac" | "aiff" | "aif") {
            o.bit_depth = Some(24);
        }
        if self.sum_to_mix_grid {
            o.dither = Some("none".into());
        }
        o
    }

    fn write(&mut self, ctx: &mut OpCtx, stem: &str, buf: &AudioBuffer, quality: Value) -> Result<()> {
        ctx.check_cancel()?;
        let path = ctx.output_path(Some(stem), &self.ext)?;
        let opts = self.opts(ctx, &path);
        ctx.write_audio_with(&path, buf, Some(stem), &opts)?;
        if let Some(q) = quality.get("score").and_then(|v| v.as_f64()) {
            ctx.metric(&format!("quality_{stem}"), (q * 1000.0).round() / 1000.0);
        }
        self.results.push(json!({
            "stem": stem,
            "path": path.display().to_string(),
            "quality": quality,
        }));
        Ok(())
    }

    /// Load a (possibly spilled) track and write it; only one stem is in memory at a time.
    fn write_track(&mut self, ctx: &mut OpCtx, stem: &str, t: &Track, sr: u32, quality: Value) -> Result<()> {
        let buf = t.to_buffer(sr)?;
        self.write(ctx, stem, &buf, quality)
    }
}

/// A cascade requested on the command line: stage name, source stem, wanted outputs, residual target.
struct CascadeReq {
    stage: &'static str,
    source: &'static str,
    wanted: Vec<String>,
    residual_to: Option<&'static str>,
    sum_to_mix: bool,
}

fn cascade_requests(a: &Args, extended: &[String], sum_to_mix: bool) -> Vec<CascadeReq> {
    let mut v = Vec::new();
    if a.flag("vocal-split") {
        v.push(CascadeReq {
            stage: "vocal-split",
            source: "vocals",
            wanted: vec!["lead".into(), "backing".into()],
            residual_to: Some("backing"),
            sum_to_mix,
        });
    }
    if a.flag("drum-split") {
        v.push(CascadeReq {
            stage: "drum-split",
            source: "drums",
            wanted: DRUM_PARTS.iter().map(|s| s.to_string()).collect(),
            residual_to: Some("cymbals"),
            sum_to_mix,
        });
    }
    if !extended.is_empty() {
        v.push(CascadeReq {
            stage: "extended",
            source: "other",
            wanted: extended.to_vec(),
            residual_to: None,
            sum_to_mix: false,
        });
    }
    v
}

/// The members of a cascade stage (override or registry default), or the exit-4 error.
fn cascade_members(manifest: &Manifest, overrides: &BTreeMap<String, String>, stage: &str) -> Result<Vec<Member>> {
    match overrides.get(stage) {
        Some(m) => Ok(vec![Member { reference: parse_ref(m), weight: 1.0, stems: vec![] }]),
        None => match manifest.tier(&format!("separate.{stage}"), "default") {
            Some(t) => Ok(t
                .members
                .iter()
                .map(|m| Member {
                    reference: ModelRef::Registry(m.model.clone()),
                    weight: m.weight,
                    stems: m.stems.iter().map(|s| canonical(s)).collect(),
                })
                .collect()),
            None => Err(Error::new(
                ErrorKind::Unsupported,
                "no_cascade_model",
                format!("the registry has no license-clean model for --{stage}"),
            )
            .with_hint(format!(
                "pass --cascade-models {stage}=<model> --accept-license (see ferraus models list --task separate)"
            ))),
        },
    }
}

fn separate(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    ctx.input(0)?;
    let cache = Cache::default_root();
    let manifest = active_registry(&cache).manifest;
    let stems_explicit = a.is_explicit("stems");
    let mut stems_n = a.choice("stems");
    let (members, tier_spec) = plan_members(ctx, a, &manifest, &stems_n)?;
    let overlap = a.percent("overlap")?;
    if !(0.0..0.95).contains(&overlap) {
        return Err(Error::usage("--overlap must be between 0% and 95%"));
    }
    let shifts = if a.is_explicit("shifts") {
        a.usize("shifts")?.max(1)
    } else {
        tier_spec.as_ref().and_then(|t| t.shifts).unwrap_or(a.usize("shifts")?.max(1))
    };
    let mode = if a.is_explicit("ensemble-mode") {
        a.choice("ensemble-mode")
    } else {
        tier_spec.as_ref().and_then(|t| t.ensemble_mode.clone()).unwrap_or_else(|| a.choice("ensemble-mode"))
    };
    let run_opts = RunOptions {
        overlap,
        shifts,
        segment: Segment::parse(&a.str_or("segment", "auto"))?,
        channel_swap: tier_spec.as_ref().map(|t| t.channel_swap).unwrap_or(false),
        seed: ctx.seed(),
    };
    let sum_to_mix = a.switch("sum-to-mix", true);
    // vocals + instrumental with sum-to-mix: the instrumental is the mix minus the vocals, so
    // tier members that only contribute drums, bass, or other are not needed (nor downloaded)
    let members: Vec<Member> =
        if stems_n == "2" && sum_to_mix && members.iter().any(|m| m.stems.iter().any(|s| s == "vocals")) {
            members.into_iter().filter(|m| m.stems.is_empty() || m.stems.iter().any(|s| s == "vocals")).collect()
        } else {
            members
        };
    let extended: Vec<String> = a.list("extended").iter().map(|s| canonical(s)).collect();
    for e in &extended {
        if !EXTENDED.contains(&e.as_str()) {
            return Err(Error::usage(format!("--extended: '{e}' is not one of {}", EXTENDED.join(", "))));
        }
    }
    ctx.note("shifts", shifts);
    ctx.note("ensemble_mode", &mode);
    // the tier that ran (auto resolved: plan_members noted "<tier> (auto)")
    let tier_label = tier_spec.as_ref().map(|_| match ctx.notes.get("tier").and_then(|v| v.as_str()) {
        Some(t) if t.ends_with(" (auto)") => t.trim_end_matches(" (auto)").to_string(),
        _ => a.choice("tier"),
    });
    if tier_spec.is_some() && a.choice("tier") != "auto" {
        ctx.note("tier", a.choice("tier"));
    }
    let ext = ctx.globals.format.clone().unwrap_or_else(|| "wav".into());
    let overrides: BTreeMap<String, String> = a
        .list("cascade-models")
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    let cascades_req = cascade_requests(a, &extended, sum_to_mix);
    let only: Vec<String> = a.list("only").iter().map(|s| canonical(s)).collect();
    let instrumental_only = a.flag("instrumental");

    if ctx.globals.dry_run {
        return dry_run_plan(ctx, &manifest, &cache, &members, &stems_n, &only, instrumental_only, &cascades_req, &overrides, &ext, shifts, overlap);
    }

    let resolved = resolve_members(ctx, &members, a.flag("accept-license"))?;

    // which stems the members provide
    let mut provided: Vec<String> = Vec::new();
    for r in &resolved {
        for s in &r.stems {
            let c = canonical(s);
            if (r.member.stems.is_empty() || r.member.stems.contains(&c)) && !provided.contains(&c) {
                provided.push(c);
            }
        }
    }
    let two_stem_model = !provided.iter().any(|s| s == "drums" || s == "bass");
    if two_stem_model && stems_n != "2" {
        if stems_explicit {
            return Err(Error::usage(format!(
                "the selected model(s) only separate {}; --stems {stems_n} is not possible",
                provided.join(" and ")
            ))
            .with_hint("pass --stems 2"));
        }
        stems_n = "2".into();
        ctx.note("stems", "2");
    }
    if stems_n == "6" && !(provided.contains(&"guitar".to_string()) && provided.contains(&"piano".to_string())) {
        return Err(Error::usage("--stems 6 needs a 6-stem model (guitar and piano)")
            .with_hint("ferraus separate <input> --stems 6 --tier fast"));
    }
    let mut order: Vec<String> = match stems_n.as_str() {
        "2" => vec!["vocals".into(), "instrumental".into()],
        "6" => STEMS_6.iter().map(|s| s.to_string()).collect(),
        _ => STEMS_4.iter().map(|s| s.to_string()).collect(),
    };
    if stems_n == "2" {
        if !provided.iter().any(|s| s == "vocals") {
            return Err(Error::processing("the model produced no vocals stem"));
        }
    } else {
        for s in &order {
            if !provided.contains(s) && !(s == "other" && sum_to_mix) {
                return Err(Error::processing(format!("no selected model provides the '{s}' stem")));
            }
        }
    }
    let all_stems = order.clone();
    if !instrumental_only && !only.is_empty() {
        for o in &only {
            if !all_stems.contains(o) {
                return Err(Error::usage(format!("--only: '{o}' is not one of {}", all_stems.join(", "))));
            }
        }
        order.retain(|s| only.contains(s));
    }
    for c in &cascades_req {
        if !all_stems.iter().any(|s| s == c.source) {
            let hint = if c.source == "vocals" { "" } else { " (use --stems 4)" };
            return Err(Error::usage(format!("--{} needs the {} stem{hint}", c.stage, c.source)));
        }
    }

    let mix = ctx.read_audio(0)?;
    let sr = mix.sample_rate;
    let ch = mix.channels();
    let n = mix.frames();
    if ch > 2 {
        return Err(Error::unsupported(format!(
            "separation needs mono or stereo input, got {ch} channels (downmix first with --channels 2)"
        )));
    }

    // estimates kept per member: (member index, model stem index, canonical name)
    let keeps: Vec<Vec<(usize, String)>> = resolved
        .iter()
        .map(|r| {
            r.stems
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    let c = canonical(s);
                    (r.member.stems.is_empty() || r.member.stems.contains(&c)).then_some((i, c))
                })
                .collect()
        })
        .collect();
    let n_estimates: usize = keeps.iter().map(|k| k.len()).sum();
    let out_names: Vec<String> = all_stems
        .iter()
        .filter(|s| order.contains(s) || cascades_req.iter().any(|c| c.source == s.as_str()))
        .cloned()
        .collect();
    // a final stem is written back into its (first) estimate's track; only stems without an
    // estimate (a summed instrumental, a silent residual) get a track of their own
    let estimated: Vec<&String> = keeps.iter().flatten().map(|(_, c)| c).collect();
    let new_outs: Vec<String> = out_names.iter().filter(|s| !estimated.contains(s)).cloned().collect();
    let n_tracks = 1 + n_estimates + new_outs.len() + usize::from(instrumental_only);
    let peaks: Vec<(u64, u64)> = resolved.iter().map(|r| model_peaks(&r.spec)).collect();
    let mem = plan_memory(ctx.globals.max_memory, n, ch, n_tracks, &peaks);
    // A model whose own ONNX Runtime peak exceeds the budget runs lean with spilled signals
    // anyway (result.memory.fits = false); only a budget the user lowered is worth a warning.
    if !mem.fits && mem.budget != ferraus_core::Globals::default().max_memory {
        ctx.warn(
            "memory_budget_exceeded",
            format!(
                "the models need about {} MB even with lean sessions and spilled signals; --max-memory {} MB cannot be met",
                mem.estimate / (1 << 20),
                mem.budget / (1 << 20)
            ),
        );
    }
    let spill = if mem.spill { Some(stream::SpillDir::new()?) } else { None };
    let mix_track = Track::from_buffer(mix, spill.as_ref())?;

    // phase 1: every member streams over the mix, one model in memory at a time
    let n_members = resolved.len();
    let mut estimates: BTreeMap<String, Vec<(f64, Track)>> = BTreeMap::new();
    let mut stem_order: Vec<String> = Vec::new();
    for (mi, (r, keep)) in resolved.iter().zip(&keeps).enumerate() {
        ctx.progress.stage(&r.label, mi + 1, n_members);
        let model = open_model(ctx, r, mem.lean)?;
        if let (_, Some(note)) = pipeline::plan_for(model.as_ref(), &run_opts)? {
            ctx.warn("segment_adjusted", note);
        }
        let mut tracks: Vec<Track> =
            keep.iter().map(|_| Track::zeros(n, ch, spill.as_ref())).collect::<Result<_>>()?;
        {
            let cancel = ctx.cancel.clone();
            let progress = ctx.progress;
            let command = ctx.command.clone();
            let mut tick = |f: f64| {
                cancel.check()?;
                progress.progress(&command, ((mi as f64 + f) / n_members as f64) * 0.9);
                Ok(())
            };
            let mut sink = |at: usize, blk: ferraus_ml::Block| -> Result<()> {
                for ((si, _), t) in keep.iter().zip(tracks.iter_mut()) {
                    t.write(at, &blk[*si])?;
                }
                Ok(())
            };
            stream::run_member(model.as_ref(), &mix_track, sr, &run_opts, mem.block, &mut tick, &mut sink)?;
        }
        release_model(model, r);
        for ((_, c), t) in keep.iter().zip(tracks) {
            if !stem_order.contains(c) {
                stem_order.push(c.clone());
            }
            estimates.entry(c.clone()).or_default().push((r.member.weight, t));
        }
    }

    // phase 2: ensemble, assemble, residual, block by block
    let probe_path = ctx.output_path(Some("vocals"), &ext)?;
    let mut writer = Writer { ext: ext.clone(), results: Vec::new(), sum_to_mix_grid: false };
    let grid = output_grid(&writer.opts(ctx, &probe_path));
    let residual_target = if stems_n == "2" { "instrumental" } else { "other" };
    let mut outs: BTreeMap<String, Track> = BTreeMap::new();
    for s in &new_outs {
        outs.insert(s.clone(), Track::zeros(n, ch, spill.as_ref())?);
    }
    // finals of the previous block, written once the current block has read its margins (they
    // reach at most 6144 samples back, less than one block)
    let mut pending: Option<(usize, Vec<(String, Vec<Vec<f32>>)>)> = None;
    let flush = |pending: Option<(usize, Vec<(String, Vec<Vec<f32>>)>)>,
                 outs: &mut BTreeMap<String, Track>,
                 estimates: &mut BTreeMap<String, Vec<(f64, Track)>>|
     -> Result<()> {
        if let Some((at, finals)) = pending {
            for (s, data) in finals {
                match outs.get_mut(&s) {
                    Some(t) => t.write(at, &data)?,
                    None => {
                        if let Some(e) = estimates.get_mut(&s) {
                            e[0].1.write(at, &data)?;
                        }
                    }
                }
            }
        }
        Ok(())
    };
    let mut inst_only = if instrumental_only { Some(Track::zeros(n, ch, spill.as_ref())?) } else { None };
    let ensemble_any = estimates.values().any(|v| v.len() > 1);
    let need_conf = estimates.values().any(|v| v.len() == 1);
    let conf_margin = ConfidenceAcc::N_FFT / 2;
    let comb_margin = if ensemble_any { 4096 + 1024 } else { 0 };
    let mut conf = ConfidenceAcc::new(stem_order.len(), n);
    let mut agree: BTreeMap<String, stream::AgreementAcc> = BTreeMap::new();
    let mut rep = pipeline::Residual::default();
    let mut start = 0usize;
    while start < n {
        ctx.check_cancel()?;
        let end = (start + mem.block).min(n);
        let (ca, cb) = (start.saturating_sub(conf_margin), (end + conf_margin).min(n));
        let (ra, rb) = (ca.saturating_sub(comb_margin), (cb + comb_margin).min(n));
        let mut comb: BTreeMap<String, Vec<Vec<f32>>> = BTreeMap::new();
        for (stem, ests) in &estimates {
            let reads: Vec<Vec<Vec<f32>>> = ests.iter().map(|(_, t)| t.read(ra, rb)).collect::<Result<_>>()?;
            if ests.len() > 1 {
                let views: Vec<Vec<Vec<f32>>> = reads
                    .iter()
                    .map(|r| r.iter().map(|c| c[start - ra..end - ra].to_vec()).collect())
                    .collect();
                let refs: Vec<&[Vec<f32>]> = views.iter().map(|v| v.as_slice()).collect();
                agree.entry(stem.clone()).or_insert_with(|| stream::AgreementAcc::new(ests.len())).feed(&refs);
                let weights: Vec<f64> = ests.iter().map(|(w, _)| *w).collect();
                let chans = (0..ch)
                    .map(|c| {
                        let v: Vec<&[f32]> = reads.iter().map(|r| r[c].as_slice()).collect();
                        stream::combine_range(&v, &weights, &mode, 4096, 1024, ra, n, ca, cb)
                    })
                    .collect();
                comb.insert(stem.clone(), chans);
            } else {
                comb.insert(stem.clone(), reads[0].iter().map(|c| c[ca - ra..cb - ra].to_vec()).collect());
            }
        }
        if need_conf {
            let monos: Vec<Vec<f32>> = stem_order.iter().map(|s| stream::mono_of(&comb[s])).collect();
            conf.feed(&monos, ca, start, end);
        }
        let slice = |v: &Vec<Vec<f32>>| AudioBuffer {
            sample_rate: sr,
            data: v.iter().map(|c| c[start - ca..end - ca].to_vec()).collect(),
        };
        let zeros = AudioBuffer::new(sr, ch, end - start);
        let mut fin: Vec<(String, AudioBuffer)> = Vec::new();
        if stems_n == "2" {
            fin.push(("vocals".into(), slice(&comb["vocals"])));
            let inst = match comb.get("instrumental").or_else(|| comb.get("other")).filter(|_| two_stem_model) {
                Some(v) => slice(v),
                None => {
                    let mut acc = zeros.clone();
                    for (s, v) in &comb {
                        if s != "vocals" {
                            add_into(&mut acc, &slice(v));
                        }
                    }
                    acc
                }
            };
            fin.push(("instrumental".into(), inst));
        } else {
            for s in &all_stems {
                match comb.get(s) {
                    Some(v) => fin.push((s.clone(), slice(v))),
                    None => fin.push((s.clone(), zeros.clone())),
                }
            }
        }
        let mix_blk = AudioBuffer { sample_rate: sr, data: mix_track.read(start, end)? };
        if sum_to_mix {
            let r = assign_residual(&mut fin, residual_target, &mix_blk, grid)?;
            rep.peak = rep.peak.max(r.peak);
            rep.clipped += r.clipped;
        }
        if let Some(t) = inst_only.as_mut() {
            let v = fin.iter().find(|(s, _)| s == "vocals").map(|(_, b)| b).ok_or_else(|| Error::processing("no vocals stem"))?;
            let mut inst = mix_blk.clone();
            for (ci, c) in inst.data.iter_mut().enumerate() {
                for (i, x) in c.iter_mut().enumerate() {
                    *x -= v.data[ci][i];
                    if let Some(g) = grid {
                        *x = pipeline::quantize(*x, g);
                    }
                }
            }
            t.write(start, &inst.data)?;
        }
        let finals: Vec<(String, Vec<Vec<f32>>)> =
            fin.into_iter().filter(|(s, _)| out_names.contains(s)).map(|(s, b)| (s, b.data)).collect();
        flush(pending.replace((start, finals)), &mut outs, &mut estimates)?;
        start = end;
    }
    flush(pending.take(), &mut outs, &mut estimates)?;
    for s in &out_names {
        if !outs.contains_key(s) {
            if let Some(mut e) = estimates.remove(s) {
                outs.insert(s.clone(), e.swap_remove(0).1);
            }
        }
    }
    drop(estimates);
    drop(mix_track);

    // quality per stem: ensemble agreement, or the mask confidence of single estimates
    let scores = conf.scores();
    let mut qualities: BTreeMap<String, Value> = BTreeMap::new();
    for (i, s) in stem_order.iter().enumerate() {
        let q = match agree.get(s).and_then(|g| g.score()) {
            Some(q) => json!({"score": q, "method": "ensemble_agreement", "members": agree[s].members()}),
            None => json!({"score": scores.get(i).copied().unwrap_or(1.0), "method": "mask_confidence", "calibrated": false}),
        };
        qualities.insert(s.clone(), q);
    }
    if stems_n == "2" {
        let inst_q = if two_stem_model {
            qualities.get("instrumental").or_else(|| qualities.get("other")).cloned()
        } else {
            None
        };
        qualities.insert("instrumental".into(), inst_q.unwrap_or_else(|| json!({"method": "sum_of_stems"})));
    } else if !qualities.contains_key("other") && sum_to_mix {
        qualities.insert("other".into(), json!({"method": "residual"}));
    }

    let mut residual_info = json!({"applied": false});
    if sum_to_mix {
        writer.sum_to_mix_grid = grid.is_some();
        if rep.clipped > 0 {
            ctx.warn(
                "residual_clipped",
                format!(
                    "{} samples of {residual_target} exceeded full scale; the stems no longer sum exactly",
                    rep.clipped
                ),
            );
        }
        residual_info = json!({
            "applied": true,
            "target": residual_target,
            "residual_peak_db": ferraus_core::lin_to_db(rep.peak.max(1e-12)),
            "grid_bits": grid.map(|g| (g.log2() + 1.0).round() as u32),
            "clipped": rep.clipped,
        });
        ctx.metric("residual_peak_db", ferraus_core::lin_to_db(rep.peak.max(1e-12)));
    }

    // phase 3: write each stem (residual target last); one stem in memory at a time
    if let Some(t) = &inst_only {
        let q = qualities.get("vocals").cloned().unwrap_or(Value::Null);
        writer.write_track(ctx, "instrumental", t, sr, q)?;
    } else {
        let mut write_order: Vec<String> = order.iter().filter(|s| s.as_str() != residual_target).cloned().collect();
        if order.iter().any(|s| s == residual_target) {
            write_order.push(residual_target.to_string());
        }
        for s in &write_order {
            if let Some(t) = outs.get(s) {
                let q = qualities.get(s).cloned().unwrap_or(Value::Null);
                writer.write_track(ctx, s, t, sr, q)?;
            }
        }
    }
    ctx.set_progress(0.9);

    // cascades on the final stems
    let mut cascades = Vec::new();
    for c in &cascades_req {
        let src = outs.get(c.source).ok_or_else(|| Error::internal("cascade source stem missing"))?;
        cascades.push(run_cascade(
            ctx,
            &manifest,
            &overrides,
            c,
            src,
            sr,
            grid,
            &run_opts,
            &mut writer,
            a.flag("accept-license"),
            &mem,
            spill.as_ref(),
        )?);
    }

    let models: Vec<Value> = resolved
        .iter()
        .map(|r| json!({"model": r.label, "weight": r.member.weight, "stems": if r.member.stems.is_empty() { r.stems.iter().map(|s| canonical(s)).collect() } else { r.member.stems.clone() }}))
        .collect();
    Ok(json!({
        "stems": writer.results,
        "models": models,
        "tier": tier_label,
        "ensemble_mode": if n_members > 1 { Some(mode) } else { None },
        "shifts": shifts,
        "sum_to_mix": residual_info,
        "cascades": cascades,
        "memory": {
            "max_memory_bytes": mem.budget,
            "estimated_peak_bytes": mem.estimate,
            "spilled": mem.spill,
            "lean_sessions": mem.lean,
            "fits": mem.fits,
            "block_frames": mem.block,
        },
    }))
}

/// `--dry-run`: the members, every output path, and each cascade stage with its outputs.
#[allow(clippy::too_many_arguments)]
fn dry_run_plan(
    ctx: &mut OpCtx,
    manifest: &Manifest,
    cache: &Cache,
    members: &[Member],
    stems_n: &str,
    only: &[String],
    instrumental_only: bool,
    cascades_req: &[CascadeReq],
    overrides: &BTreeMap<String, String>,
    ext: &str,
    shifts: usize,
    overlap: f64,
) -> Result<Value> {
    let cached = |r: &ModelRef| match r {
        ModelRef::File(p) => p.exists(),
        ModelRef::Registry(n) => manifest.resolve(n).map(|e| cache.cached(e).is_some()).unwrap_or(false),
    };
    let plan: Vec<Value> = members
        .iter()
        .map(|m| json!({"model": label(&m.reference), "weight": m.weight, "stems": m.stems, "cached": cached(&m.reference)}))
        .collect();
    let mut names: Vec<&str> = match stems_n {
        "2" => vec!["vocals", "instrumental"],
        "6" => STEMS_6.to_vec(),
        _ => STEMS_4.to_vec(),
    };
    if instrumental_only {
        names = vec!["instrumental"];
    } else if !only.is_empty() {
        names.retain(|s| only.iter().any(|o| o == s));
    }
    let mut outputs = Vec::new();
    for s in names {
        let p = ctx.output_path(Some(s), ext)?;
        ctx.prepare_output(&p)?;
        outputs.push(json!({"stem": s, "path": p.display().to_string()}));
    }
    let mut cascades = Vec::new();
    for c in cascades_req {
        let members = match cascade_members(manifest, overrides, c.stage) {
            Ok(m) => m,
            Err(e) => {
                cascades.push(json!({
                    "stage": c.stage,
                    "source": c.source,
                    "available": false,
                    "error": e.message,
                    "hint": e.hint,
                }));
                continue;
            }
        };
        // the stems each member would produce (registry entries; file models via their spec)
        let mut produced: Vec<String> = Vec::new();
        let mut models = Vec::new();
        for m in &members {
            let stems: Vec<String> = match &m.reference {
                ModelRef::Registry(n) => manifest
                    .resolve(n)
                    .map(|e| adapter::stems_of(&ModelSpec::from_entry(e)))
                    .unwrap_or_default(),
                ModelRef::File(p) => {
                    let side = [p.with_extension("spec.json"), p.with_file_name("spec.json")];
                    side.iter().find(|s| s.exists()).and_then(|s| ModelSpec::load(s).ok()).map(|s| adapter::stems_of(&s)).unwrap_or_default()
                }
            };
            for s in stems {
                let s = canonical(&s);
                if (m.stems.is_empty() || m.stems.contains(&s)) && !produced.contains(&s) {
                    produced.push(s);
                }
            }
            models.push(json!({"model": label(&m.reference), "cached": cached(&m.reference)}));
        }
        let mut unavailable: Vec<&str> =
            c.wanted.iter().map(|s| s.as_str()).filter(|w| !produced.iter().any(|p| p == w)).collect();
        if c.stage == "drum-split" && unavailable == ["hh"] {
            unavailable.clear();
        }
        let mut outs = Vec::new();
        for w in c.wanted.iter().filter(|w| produced.contains(w)) {
            let p = ctx.output_path(Some(w), ext)?;
            ctx.prepare_output(&p)?;
            outs.push(json!({"stem": w, "path": p.display().to_string()}));
        }
        cascades.push(json!({
            "stage": c.stage,
            "source": c.source,
            "available": true,
            "models": models,
            "outputs": outs,
            "unavailable": unavailable,
            "best_effort": c.stage == "extended",
        }));
    }
    Ok(json!({"plan": plan, "outputs": outputs, "cascades": cascades, "shifts": shifts, "overlap": overlap}))
}

/// A second stage on one final stem. Writes the requested outputs and returns a JSON summary.
#[allow(clippy::too_many_arguments)]
fn run_cascade(
    ctx: &mut OpCtx,
    manifest: &Manifest,
    overrides: &BTreeMap<String, String>,
    req: &CascadeReq,
    source: &Track,
    sr: u32,
    grid: Option<f64>,
    run_opts: &RunOptions,
    writer: &mut Writer,
    accept: bool,
    mem: &MemPlan,
    spill: Option<&stream::SpillDir>,
) -> Result<Value> {
    let stage = req.stage;
    let members = cascade_members(manifest, overrides, stage)?;
    let resolved = resolve_members(ctx, &members, accept)?;
    let (n, ch) = (source.frames, source.channels);
    let wanted: Vec<&str> = req.wanted.iter().map(|s| s.as_str()).collect();
    let mut produced: Vec<String> = Vec::new();
    let mut keep: Vec<(String, Track)> = Vec::new();
    for r in &resolved {
        let sel: Vec<(usize, String)> = r
            .stems
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let c = canonical(s);
                ((r.member.stems.is_empty() || r.member.stems.contains(&c)) && !produced.contains(&c)).then_some((i, c))
            })
            .collect();
        produced.extend(sel.iter().map(|(_, c)| c.clone()));
        let store: Vec<(usize, String)> = sel.into_iter().filter(|(_, c)| wanted.contains(&c.as_str())).collect();
        let model = open_model(ctx, r, mem.lean)?;
        let mut tracks: Vec<Track> = store.iter().map(|_| Track::zeros(n, ch, spill)).collect::<Result<_>>()?;
        if !store.is_empty() {
            let cancel = ctx.cancel.clone();
            let mut tick = |_f: f64| cancel.check();
            let mut sink = |at: usize, blk: ferraus_ml::Block| -> Result<()> {
                for ((si, _), t) in store.iter().zip(tracks.iter_mut()) {
                    t.write(at, &blk[*si])?;
                }
                Ok(())
            };
            stream::run_member(model.as_ref(), source, sr, run_opts, mem.block, &mut tick, &mut sink)?;
        }
        release_model(model, r);
        keep.extend(store.into_iter().map(|(_, c)| c).zip(tracks));
    }
    let mut missing: Vec<&str> = wanted.iter().copied().filter(|w| !produced.iter().any(|s| s == w)).collect();
    if stage == "drum-split" && missing == ["hh"] {
        ctx.warn("drum_split_no_hihat", "the drum model has no separate hi-hat output; hi-hats are in cymbals");
        missing.clear();
    }
    if !missing.is_empty() {
        ctx.warn("cascade_stems_unavailable", format!("--{stage}: no available model produces {}", missing.join(", ")));
    }
    // keep the order the outputs were requested in the model order (first member wins)
    let target = req.residual_to.filter(|t| req.sum_to_mix && keep.iter().any(|(s, _)| s == t));
    let mut conf = ConfidenceAcc::new(keep.len(), n);
    let mut peak = 0f64;
    let margin = ConfidenceAcc::N_FFT / 2;
    let mut start = 0usize;
    while start < n {
        ctx.check_cancel()?;
        let end = (start + mem.block).min(n);
        let (ca, cb) = (start.saturating_sub(margin), (end + margin).min(n));
        let mut fin: Vec<(String, AudioBuffer)> = Vec::new();
        for (s, t) in &keep {
            fin.push((s.clone(), AudioBuffer { sample_rate: sr, data: t.read(ca, cb)? }));
        }
        if let Some(target) = target {
            // idempotent on samples already adjusted by the previous block
            let src = AudioBuffer { sample_rate: sr, data: source.read(ca, cb)? };
            peak = peak.max(assign_residual(&mut fin, target, &src, grid)?.peak);
        }
        let monos: Vec<Vec<f32>> = fin.iter().map(|(_, b)| stream::mono_of(&b.data)).collect();
        conf.feed(&monos, ca, start, end);
        for ((_, t), (_, b)) in keep.iter_mut().zip(&fin) {
            let part: Vec<Vec<f32>> = b.data.iter().map(|c| c[start - ca..end - ca].to_vec()).collect();
            t.write(start, &part)?;
        }
        start = end;
    }
    let residual = match target {
        Some(t) => json!({"target": t, "residual_peak_db": ferraus_core::lin_to_db(peak.max(1e-12))}),
        None => Value::Null,
    };
    let mut written = Vec::new();
    for ((s, t), c) in keep.iter().zip(conf.scores()) {
        let q =
            json!({"score": c, "method": "mask_confidence", "calibrated": false, "best_effort": stage == "extended"});
        writer.write_track(ctx, s, t, sr, q)?;
        written.push(s.clone());
    }
    Ok(json!({
        "stage": stage,
        "models": resolved.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
        "stems": written,
        "unavailable": missing,
        "best_effort": stage == "extended",
        "residual": residual,
    }))
}

fn add_into(acc: &mut AudioBuffer, b: &AudioBuffer) {
    for (ca, cb) in acc.data.iter_mut().zip(&b.data) {
        for (x, y) in ca.iter_mut().zip(cb) {
            *x += *y;
        }
    }
}

#[cfg(test)]
mod tests;
