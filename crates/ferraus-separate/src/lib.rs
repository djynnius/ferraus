//! ferraus-separate: the `separate` command.
//!
//! Tiers map to registry models (`fast` = Demucs v4 `htdemucs`, ...); `--model` pins one
//! model (a registry name or an ONNX file path), `--ensemble` builds a custom ensemble. The
//! pipeline (see [`pipeline`]) resamples to the model rate, runs windowed inference with a
//! raised-cosine crossfade, averages test-time-augmentation passes, combines ensemble members
//! in the magnitude domain, assigns the residual to `other` so the stems sum back to the mix
//! on the output sample grid, runs cascades, and writes each stem as soon as it is final.

pub mod adapter;
pub mod pipeline;
pub mod stft;

use adapter::SeparationModel;
use ferraus_core::prelude::*;
use ferraus_core::EncodeOptions;
use ferraus_models::{active_registry, ensure_model_with, not_cached_error, Cache, EnsureOptions, Manifest, ModelSpec};
use pipeline::{assign_residual, ensemble, separate_buffer, RunOptions, Segment};
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
    .about("Separates a song into stems with open models run through ONNX Runtime. Tiers trade speed for quality: fast (Demucs v4), balanced (Demucs v4 fine-tuned bag), quality (RoFormer vocals + Demucs), max (ensemble with test-time augmentation). Models download on first use only with -y or models.auto_download = true; otherwise the command fails with exit code 5 and the exact `ferraus models pull` command. With --sum-to-mix on (default) the residual goes to `other`, so the written stems add back to the input within one LSB. Each stem's JSON result carries a quality estimate (ensemble agreement, or mask confidence for a single model).")
    .param(param("stems", ParamKind::Choice).choices(&["2", "4", "6"]).default("4").help("2 = vocals + instrumental, 4 = vocals/drums/bass/other, 6 = adds guitar and piano"))
    .param(param("tier", ParamKind::Choice).choices(&["fast", "balanced", "quality", "max"]).default("quality").help("Model tier from the registry"))
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
    .example("ferraus separate song.wav", "Four stems with the default quality tier: song.vocals.wav, song.drums.wav, ...")
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
    let tier = a.choice("tier");
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

struct Loaded {
    member: Member,
    model: Box<dyn SeparationModel>,
}

/// Ensure and open every member. Missing registry models are collected into one exit-5 error.
fn load_members(ctx: &mut OpCtx, members: &[Member], accept: bool) -> Result<Vec<Loaded>> {
    let cache = Cache::default_root();
    let opts = EnsureOptions { accept_license: accept, cache: Some(cache), ..Default::default() };
    let mut missing = Vec::new();
    let mut resolved: Vec<(Member, PathBuf, ModelSpec, String)> = Vec::new();
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
                resolved.push((m.clone(), p.clone(), spec, p.display().to_string()));
            }
            ModelRef::Registry(name) => match ensure_model_with(ctx, name, &opts) {
                Ok(c) => {
                    let label = c.entry.id();
                    resolved.push((m.clone(), c.path, c.spec, label));
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
    let mut out = Vec::new();
    for (member, path, spec, label) in resolved {
        let session = ferraus_ml::session::load_for_hinted(ctx, &path, &spec.provider_hints)?;
        let model = adapter::open(&label, spec, session)?;
        out.push(Loaded { member, model });
    }
    Ok(out)
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
}

fn separate(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    ctx.input(0)?;
    let cache = Cache::default_root();
    let manifest = active_registry(&cache).manifest;
    let stems_explicit = a.is_explicit("stems");
    let mut stems_n = a.choice("stems");
    let (members, tier_spec) = plan_members(a, &manifest, &stems_n)?;
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
    let extended: Vec<String> = a.list("extended").iter().map(|s| canonical(s)).collect();
    for e in &extended {
        if !EXTENDED.contains(&e.as_str()) {
            return Err(Error::usage(format!("--extended: '{e}' is not one of {}", EXTENDED.join(", "))));
        }
    }
    ctx.note("shifts", shifts);
    ctx.note("ensemble_mode", &mode);
    if let Some(t) = &tier_spec {
        let _ = t;
        ctx.note("tier", a.choice("tier"));
    }
    let ext = ctx.globals.format.clone().unwrap_or_else(|| "wav".into());

    if ctx.globals.dry_run {
        let plan: Vec<Value> = members
            .iter()
            .map(|m| {
                let cached = match &m.reference {
                    ModelRef::File(p) => p.exists(),
                    ModelRef::Registry(n) => manifest.resolve(n).map(|e| cache.cached(e).is_some()).unwrap_or(false),
                };
                json!({"model": label(&m.reference), "weight": m.weight, "stems": m.stems, "cached": cached})
            })
            .collect();
        let names: Vec<&str> = match stems_n.as_str() {
            "2" => vec!["vocals", "instrumental"],
            "6" => STEMS_6.to_vec(),
            _ => STEMS_4.to_vec(),
        };
        let mut outputs = Vec::new();
        for s in names {
            let p = ctx.output_path(Some(s), &ext)?;
            ctx.prepare_output(&p)?;
            outputs.push(json!({"stem": s, "path": p.display().to_string()}));
        }
        return Ok(json!({"plan": plan, "outputs": outputs, "shifts": shifts, "overlap": overlap}));
    }

    let mix = ctx.read_audio(0)?;
    let loaded = load_members(ctx, &members, a.flag("accept-license"))?;

    // which stems the members provide
    let mut provided: Vec<String> = Vec::new();
    for l in &loaded {
        for s in l.model.stems() {
            let c = canonical(s);
            if (l.member.stems.is_empty() || l.member.stems.contains(&c)) && !provided.contains(&c) {
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

    // run every member
    let n_members = loaded.len();
    let mut estimates: BTreeMap<String, Vec<(f64, AudioBuffer)>> = BTreeMap::new();
    let mut single_model_stems: Vec<(String, AudioBuffer)> = Vec::new();
    for (mi, l) in loaded.iter().enumerate() {
        ctx.progress.stage(l.model.name(), mi + 1, n_members);
        if let (_, Some(note)) = pipeline::plan_for(l.model.as_ref(), &run_opts)? {
            ctx.warn("segment_adjusted", note);
        }
        let cancel = ctx.cancel.clone();
        let progress = ctx.progress;
        let command = ctx.command.clone();
        let mut tick = |f: f64| {
            cancel.check()?;
            progress.progress(&command, ((mi as f64 + f) / n_members as f64) * 0.9);
            Ok(())
        };
        let out = separate_buffer(l.model.as_ref(), &mix, &run_opts, &mut tick)?;
        for (stem, buf) in out {
            let c = canonical(&stem);
            if !l.member.stems.is_empty() && !l.member.stems.contains(&c) {
                continue;
            }
            if n_members == 1 {
                single_model_stems.push((c.clone(), buf.clone()));
            }
            estimates.entry(c).or_default().push((l.member.weight, buf));
        }
    }

    // ensemble per stem + quality
    let mut combined: BTreeMap<String, (AudioBuffer, Value)> = BTreeMap::new();
    let confidences: BTreeMap<String, f64> = if n_members == 1 {
        let bufs: Vec<&AudioBuffer> = single_model_stems.iter().map(|(_, b)| b).collect();
        single_model_stems.iter().map(|(s, _)| s.clone()).zip(pipeline::mask_confidence(&bufs)).collect()
    } else {
        BTreeMap::new()
    };
    for (stem, ests) in &estimates {
        let bufs: Vec<&AudioBuffer> = ests.iter().map(|(_, b)| b).collect();
        let weights: Vec<f64> = ests.iter().map(|(w, _)| *w).collect();
        let out = ensemble(&bufs, &weights, &mode);
        let quality = match pipeline::agreement(&bufs) {
            Some(q) => json!({"score": q, "method": "ensemble_agreement", "members": bufs.len()}),
            None => json!({
                "score": confidences.get(stem).copied().unwrap_or(1.0),
                "method": "mask_confidence",
                "calibrated": false,
            }),
        };
        combined.insert(stem.clone(), (out, quality));
    }

    // assemble the requested stem set
    let mut order: Vec<String> = match stems_n.as_str() {
        "2" => vec!["vocals".into(), "instrumental".into()],
        "6" => STEMS_6.iter().map(|s| s.to_string()).collect(),
        _ => STEMS_4.iter().map(|s| s.to_string()).collect(),
    };
    let zeros = || AudioBuffer::new(mix.sample_rate, mix.channels(), mix.frames());
    let mut final_stems: Vec<(String, AudioBuffer)> = Vec::new();
    let mut qualities: BTreeMap<String, Value> = BTreeMap::new();
    if stems_n == "2" {
        let vocals =
            combined.get("vocals").cloned().ok_or_else(|| Error::processing("the model produced no vocals stem"))?;
        let inst = match combined.get("instrumental").or_else(|| combined.get("other")).filter(|_| two_stem_model) {
            Some((b, q)) => (b.clone(), q.clone()),
            None => {
                let mut acc = zeros();
                for (s, (b, _)) in &combined {
                    if s != "vocals" {
                        add_into(&mut acc, b);
                    }
                }
                (acc, json!({"method": "sum_of_stems"}))
            }
        };
        qualities.insert("vocals".into(), vocals.1.clone());
        qualities.insert("instrumental".into(), inst.1.clone());
        final_stems.push(("vocals".into(), vocals.0));
        final_stems.push(("instrumental".into(), inst.0));
    } else {
        for s in &order {
            match combined.get(s) {
                Some((b, q)) => {
                    final_stems.push((s.clone(), b.clone()));
                    qualities.insert(s.clone(), q.clone());
                }
                None if s == "other" && sum_to_mix => {
                    final_stems.push((s.clone(), zeros()));
                    qualities.insert(s.clone(), json!({"method": "residual"}));
                }
                None => return Err(Error::processing(format!("no selected model provides the '{s}' stem"))),
            }
        }
    }

    // residual assignment on the output grid
    let probe_path = ctx.output_path(Some("vocals"), &ext)?;
    let mut writer = Writer { ext: ext.clone(), results: Vec::new(), sum_to_mix_grid: false };
    let grid = output_grid(&writer.opts(ctx, &probe_path));
    let residual_target = if stems_n == "2" { "instrumental" } else { "other" };
    let mut residual_info = json!({"applied": false});
    if sum_to_mix {
        let rep = assign_residual(&mut final_stems, residual_target, &mix, grid)?;
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

    // which stems to write
    let only: Vec<String> = a.list("only").iter().map(|s| canonical(s)).collect();
    let instrumental_only = a.flag("instrumental");
    if instrumental_only {
        let v = final_stems
            .iter()
            .find(|(s, _)| s == "vocals")
            .map(|(_, b)| b.clone())
            .ok_or_else(|| Error::processing("no vocals stem"))?;
        let mut inst = mix.clone();
        for (ci, c) in inst.data.iter_mut().enumerate() {
            for (i, x) in c.iter_mut().enumerate() {
                *x -= v.data[ci][i];
            }
        }
        if let Some(g) = grid {
            for c in inst.data.iter_mut() {
                for x in c.iter_mut() {
                    *x = pipeline::quantize(*x, g);
                }
            }
        }
        final_stems = vec![("instrumental".into(), inst)];
        qualities.insert("instrumental".into(), qualities.get("vocals").cloned().unwrap_or(Value::Null));
        order = vec!["instrumental".into()];
    } else if !only.is_empty() {
        for o in &only {
            if !final_stems.iter().any(|(s, _)| s == o) {
                return Err(Error::usage(format!(
                    "--only: '{o}' is not one of {}",
                    final_stems.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join(", ")
                )));
            }
        }
        order.retain(|s| only.contains(s));
    }

    // write each stem as soon as it is final (residual target last)
    let mut write_order: Vec<String> = order.iter().filter(|s| s.as_str() != residual_target).cloned().collect();
    if order.iter().any(|s| s == residual_target) {
        write_order.push(residual_target.to_string());
    }
    for s in &write_order {
        if let Some((_, b)) = final_stems.iter().find(|(n, _)| n == s) {
            let q = qualities.get(s).cloned().unwrap_or(Value::Null);
            writer.write(ctx, s, b, q)?;
        }
    }
    ctx.set_progress(0.9);

    // cascades
    let overrides: BTreeMap<String, String> = a
        .list("cascade-models")
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    let mut cascades = Vec::new();
    let stem_buf = |name: &str| final_stems.iter().find(|(s, _)| s == name).map(|(_, b)| b.clone());
    if a.flag("vocal-split") {
        let src = stem_buf("vocals").ok_or_else(|| Error::usage("--vocal-split needs the vocals stem"))?;
        cascades.push(run_cascade(
            ctx,
            &manifest,
            &overrides,
            "vocal-split",
            &src,
            &["lead", "backing"],
            Some("backing"),
            sum_to_mix,
            grid,
            &run_opts,
            &mut writer,
            a.flag("accept-license"),
        )?);
    }
    if a.flag("drum-split") {
        let src = stem_buf("drums").ok_or_else(|| Error::usage("--drum-split needs the drums stem (use --stems 4)"))?;
        cascades.push(run_cascade(
            ctx,
            &manifest,
            &overrides,
            "drum-split",
            &src,
            DRUM_PARTS,
            Some("cymbals"),
            sum_to_mix,
            grid,
            &run_opts,
            &mut writer,
            a.flag("accept-license"),
        )?);
    }
    if !extended.is_empty() {
        let src = stem_buf("other").ok_or_else(|| Error::usage("--extended needs the other stem (use --stems 4)"))?;
        let want: Vec<&str> = extended.iter().map(|s| s.as_str()).collect();
        cascades.push(run_cascade(
            ctx,
            &manifest,
            &overrides,
            "extended",
            &src,
            &want,
            None,
            false,
            None,
            &run_opts,
            &mut writer,
            a.flag("accept-license"),
        )?);
    }

    let models: Vec<Value> = loaded
        .iter()
        .map(|l| json!({"model": l.model.name(), "weight": l.member.weight, "stems": if l.member.stems.is_empty() { l.model.stems().iter().map(|s| canonical(s)).collect() } else { l.member.stems.clone() }}))
        .collect();
    Ok(json!({
        "stems": writer.results,
        "models": models,
        "tier": tier_spec.as_ref().map(|_| a.choice("tier")),
        "ensemble_mode": if n_members > 1 { Some(mode) } else { None },
        "shifts": shifts,
        "sum_to_mix": residual_info,
        "cascades": cascades,
    }))
}

/// A second stage on one stem. Writes the requested outputs and returns a JSON summary.
#[allow(clippy::too_many_arguments)]
fn run_cascade(
    ctx: &mut OpCtx,
    manifest: &Manifest,
    overrides: &BTreeMap<String, String>,
    stage: &str,
    source: &AudioBuffer,
    wanted: &[&str],
    residual_to: Option<&str>,
    sum_to_mix: bool,
    grid: Option<f64>,
    run_opts: &RunOptions,
    writer: &mut Writer,
    accept: bool,
) -> Result<Value> {
    let members: Vec<Member> = match overrides.get(stage) {
        Some(m) => vec![Member { reference: parse_ref(m), weight: 1.0, stems: vec![] }],
        None => match manifest.tier(&format!("separate.{stage}"), "default") {
            Some(t) => t
                .members
                .iter()
                .map(|m| Member {
                    reference: ModelRef::Registry(m.model.clone()),
                    weight: m.weight,
                    stems: m.stems.iter().map(|s| canonical(s)).collect(),
                })
                .collect(),
            None => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "no_cascade_model",
                    format!("the registry has no license-clean model for --{stage}"),
                )
                .with_hint(format!(
                    "pass --cascade-models {stage}=<model> --accept-license (see ferraus models list --task separate)"
                )))
            }
        },
    };
    let loaded = load_members(ctx, &members, accept)?;
    let mut outs: Vec<(String, AudioBuffer)> = Vec::new();
    for l in &loaded {
        let cancel = ctx.cancel.clone();
        let mut tick = |_f: f64| cancel.check();
        for (stem, buf) in separate_buffer(l.model.as_ref(), source, run_opts, &mut tick)? {
            let c = canonical(&stem);
            if (l.member.stems.is_empty() || l.member.stems.contains(&c)) && !outs.iter().any(|(s, _)| *s == c) {
                outs.push((c, buf));
            }
        }
    }
    let mut missing: Vec<&str> = wanted.iter().copied().filter(|w| !outs.iter().any(|(s, _)| s == w)).collect();
    if stage == "drum-split" && missing == ["hh"] {
        ctx.warn("drum_split_no_hihat", "the drum model has no separate hi-hat output; hi-hats are in cymbals");
        missing.clear();
    }
    if !missing.is_empty() {
        ctx.warn("cascade_stems_unavailable", format!("--{stage}: no available model produces {}", missing.join(", ")));
    }
    let mut keep: Vec<(String, AudioBuffer)> = outs.into_iter().filter(|(s, _)| wanted.contains(&s.as_str())).collect();
    let mut residual = Value::Null;
    if let (true, Some(target)) = (sum_to_mix, residual_to) {
        if keep.iter().any(|(s, _)| s == target) {
            let rep = assign_residual(&mut keep, target, source, grid)?;
            residual = json!({"target": target, "residual_peak_db": ferraus_core::lin_to_db(rep.peak.max(1e-12))});
        }
    }
    let bufs: Vec<&AudioBuffer> = keep.iter().map(|(_, b)| b).collect();
    let conf = pipeline::mask_confidence(&bufs);
    let mut written = Vec::new();
    for ((s, b), c) in keep.iter().zip(conf) {
        let q =
            json!({"score": c, "method": "mask_confidence", "calibrated": false, "best_effort": stage == "extended"});
        writer.write(ctx, s, b, q)?;
        written.push(s.clone());
    }
    Ok(json!({
        "stage": stage,
        "models": loaded.iter().map(|l| l.model.name().to_string()).collect::<Vec<_>>(),
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
