//! The executor: runs one op with resolved args, handling batches, `--mix`, `fx` chains,
//! dry runs, idempotent outputs, output encoding overrides, and sidecars. The CLI, the
//! pipeline runner, and the MCP server all go through `Runtime::invoke`.

use crate::buffer::AudioBuffer;
use crate::ctx::{AudioIo, CancelToken, Globals, OpCtx, OutputRecord, Progress};
use crate::envelope::{sha256_file, Envelope, HashedFile, HostInfo, Sidecar, FERRAUS_VERSION, SCHEMA_VERSION};
use crate::error::{Error, Result};
use crate::op::{parse_stage_string, Args, OpSpec, Runner};
use crate::registry::Registry;
use crate::resample::{resample, Quality};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Looks up user presets: (command, preset name) → values.
pub type PresetLookup = dyn Fn(&str, &str) -> Option<Vec<(String, String)>> + Send + Sync;

pub struct Runtime<'a> {
    pub registry: &'a Registry,
    pub io: &'a dyn AudioIo,
    pub progress: &'a dyn Progress,
    pub presets: &'a PresetLookup,
    pub cancel: CancelToken,
}

/// One request to run a command.
#[derive(Clone, Debug, Default)]
pub struct Invocation {
    pub command: String,
    pub args: Vec<(String, String)>,
    pub inputs: Vec<String>,
    pub globals: Globals,
    /// The original command line, recorded in sidecars.
    pub argv: Vec<String>,
    pub stdin: Option<Vec<u8>>,
}

/// What `invoke` returns: the envelope plus any bytes destined for stdout (`-o -`).
pub struct Outcome {
    pub envelope: Envelope,
    pub stdout: Option<Vec<u8>>,
}

fn expand_inputs(inputs: &[String]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for i in inputs {
        if i == "-" {
            out.push(PathBuf::from("-"));
            continue;
        }
        if i.contains(['*', '?', '[']) {
            let mut matched: Vec<PathBuf> = glob::glob(i)
                .map_err(|e| Error::usage(format!("bad glob '{i}': {e}")))?
                .filter_map(|r| r.ok())
                .collect();
            matched.sort();
            if matched.is_empty() {
                return Err(Error::input(format!("no files match '{i}'")));
            }
            out.extend(matched);
        } else {
            let p = PathBuf::from(i);
            out.push(p);
        }
    }
    Ok(out)
}

impl<'a> Runtime<'a> {
    pub fn invoke(&self, inv: Invocation) -> Outcome {
        let start = Instant::now();
        match self.invoke_inner(inv.clone(), start) {
            Ok(o) => o,
            Err(e) => {
                Outcome { envelope: Envelope::error(&inv.command, e, start.elapsed().as_millis() as u64), stdout: None }
            }
        }
    }

    fn invoke_inner(&self, inv: Invocation, start: Instant) -> Result<Outcome> {
        let spec = self.registry.require(&inv.command)?;
        let args = Args::build(spec, inv.args.clone(), self.presets)?;
        let inputs = expand_inputs(&inv.inputs)?;
        if inputs.len() < spec.inputs.min {
            return Err(Error::usage(format!(
                "'{}' needs {} input file(s), got {}",
                spec.name,
                spec.inputs.min,
                inputs.len()
            ))
            .with_hint(format!("ferraus docs {}", spec.name)));
        }
        let batch = matches!(spec.inputs.max, Some(1)) && inputs.len() > 1;
        if let Some(max) = spec.inputs.max {
            if !batch && inputs.len() > max {
                return Err(Error::usage(format!("'{}' takes at most {max} input(s)", spec.name)));
            }
        }
        if batch && inv.globals.output.as_deref().map(|o| !o.contains('{')).unwrap_or(false) {
            return Err(Error::usage("several inputs need --outdir or an -o template such as '{name}.out.wav'"));
        }

        let groups: Vec<Vec<PathBuf>> =
            if batch { inputs.iter().map(|i| vec![i.clone()]).collect() } else { vec![inputs.clone()] };
        let mut envelope = Envelope {
            ok: true,
            command: spec.name.to_string(),
            ferraus_version: FERRAUS_VERSION.to_string(),
            schema_version: SCHEMA_VERSION,
            inputs: vec![],
            outputs: vec![],
            params: args.to_json(),
            models: vec![],
            metrics: Map::new(),
            warnings: vec![],
            result: Value::Null,
            error: None,
            timing_ms: 0,
            dry_run: if inv.globals.dry_run { Some(true) } else { None },
        };
        let mut results = Vec::new();
        let mut stdout: Option<Vec<u8>> = None;
        let mut deterministic = !matches!(spec.runner, Runner::Custom(_)) || inv.globals.deterministic;
        for (index, group) in groups.into_iter().enumerate() {
            let item_start = Instant::now();
            let mut ctx = OpCtx::new(spec.name, group, inv.globals.clone(), self.io, self.progress);
            ctx.registry = Some(self.registry);
            ctx.cancel = self.cancel.clone();
            ctx.index = index;
            ctx.stdin = inv.stdin.clone();
            if batch {
                self.progress.stage(spec.name, index + 1, inputs.len());
            }
            let res = self.run_one(spec, &args, &mut ctx);
            match res {
                Ok(v) => {
                    if !inv.globals.dry_run && inv.globals.sidecar {
                        self.write_sidecars(spec, &args, &ctx, &inv.argv, item_start.elapsed().as_millis() as u64)?;
                    }
                    if ctx.deterministic == Some(false) {
                        deterministic = false;
                    }
                    envelope.inputs.append(&mut ctx.input_records);
                    envelope.outputs.append(&mut ctx.outputs);
                    envelope.warnings.append(&mut ctx.warnings);
                    for m in ctx.models.drain(..) {
                        if !envelope.models.contains(&m) {
                            envelope.models.push(m);
                        }
                    }
                    for (k, val) in ctx.notes {
                        envelope.params.insert(k, val);
                    }
                    if batch {
                        let mut item = Map::new();
                        item.insert("input".into(), json!(inputs[index].display().to_string()));
                        if !ctx.metrics.is_empty() {
                            item.insert("metrics".into(), Value::Object(ctx.metrics));
                        }
                        if !v.is_null() {
                            item.insert("result".into(), v);
                        }
                        results.push(Value::Object(item));
                    } else {
                        envelope.metrics = ctx.metrics;
                        envelope.result = v;
                    }
                    if let Some(b) = ctx.stdout.take() {
                        stdout.get_or_insert_with(Vec::new).extend(b);
                    }
                }
                Err(e) => {
                    for t in &ctx.temp_files {
                        let _ = std::fs::remove_file(t);
                    }
                    if batch {
                        results.push(json!({"input": inputs[index].display().to_string(), "ok": false, "error": e}));
                        envelope.ok = false;
                        if envelope.error.is_none() {
                            envelope.error = Some(e);
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        if batch {
            envelope.result = Value::Array(results);
        }
        let _ = deterministic;
        if inv.globals.explain {
            envelope.metrics.insert("explain".into(), explain(spec, &args));
        }
        envelope.timing_ms = start.elapsed().as_millis() as u64;
        self.progress.log(&format!("done in {} ms", envelope.timing_ms));
        Ok(Outcome { envelope, stdout })
    }

    /// Run a single (non-batch) item.
    fn run_one(&self, spec: &OpSpec, args: &Args, ctx: &mut OpCtx) -> Result<Value> {
        match spec.runner {
            Runner::Transform(_) | Runner::Chain => {
                let out_ext = default_ext(ctx);
                let out_path = ctx.output_path(None, &out_ext)?;
                if ctx.globals.dry_run {
                    ctx.prepare_output(&out_path)?;
                    ctx.outputs.push(OutputRecord {
                        path: out_path.display().to_string(),
                        role: None,
                        duration_s: None,
                        sample_rate: None,
                        channels: None,
                        extra: Map::new(),
                    });
                    let plan = if matches!(spec.runner, Runner::Chain) {
                        chain_plan(self.registry, args, self.presets)?
                    } else {
                        Value::Null
                    };
                    return Ok(plan);
                }
                // idempotent outputs: same command, params, and inputs → reuse
                if !ctx.globals.force && !ctx.globals.overwrite && out_path.as_os_str() != "-" && out_path.exists() {
                    if let Some(v) = self.reuse_existing(spec, args, ctx, &out_path) {
                        return Ok(v);
                    }
                }
                ctx.prepare_output(&out_path)?;
                let input = ctx.read_audio(0)?;
                ctx.set_progress(0.0);
                let out = match spec.runner {
                    Runner::Transform(_) => apply_spec(spec, args, input, ctx)?,
                    Runner::Chain => self.run_chain(args, input, ctx)?,
                    Runner::Custom(_) => unreachable!(),
                };
                let out = conform_output(out, &ctx.globals);
                ctx.write_audio(&out_path, &out, None)?;
                ctx.set_progress(1.0);
                Ok(Value::Null)
            }
            Runner::Custom(f) => {
                ctx.set_progress(0.0);
                let v = f(args, ctx)?;
                ctx.set_progress(1.0);
                Ok(v)
            }
        }
    }

    fn reuse_existing(&self, spec: &OpSpec, args: &Args, ctx: &mut OpCtx, out: &Path) -> Option<Value> {
        let sc_path = Sidecar::path_for(out);
        let sc: Sidecar = serde_json::from_slice(&std::fs::read(&sc_path).ok()?).ok()?;
        if sc.command != spec.name || sc.ferraus_version != FERRAUS_VERSION {
            return None;
        }
        let params = args.to_json();
        for (k, v) in &params {
            if sc.params.get(k) != Some(v) {
                return None;
            }
        }
        if sc.inputs.len() != ctx.inputs.len() {
            return None;
        }
        for (rec, path) in sc.inputs.iter().zip(&ctx.inputs) {
            let h = sha256_file(path).ok()?;
            if rec.sha256.as_deref() != Some(h.as_str()) {
                return None;
            }
        }
        for o in &sc.outputs {
            let h = sha256_file(Path::new(&o.path)).ok()?;
            if o.sha256.as_deref() != Some(h.as_str()) {
                return None;
            }
        }
        for o in &sc.outputs {
            ctx.outputs.push(OutputRecord {
                path: o.path.clone(),
                role: o.role.clone(),
                duration_s: o.duration_s,
                sample_rate: None,
                channels: None,
                extra: Map::new(),
            });
        }
        ctx.warn("reused_output", format!("{} already matches this command; pass --force to recompute", out.display()));
        // keep the existing sidecar untouched
        ctx.globals.sidecar = false;
        Some(json!({"reused": true}))
    }

    fn run_chain(&self, args: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
        let stages = chain_stages(args)?;
        let n = stages.len();
        for (i, (name, pairs)) in stages.into_iter().enumerate() {
            ctx.check_cancel()?;
            self.progress.stage(&name, i + 1, n);
            buf = run_transform_pairs(self.registry, self.presets, &name, pairs, buf, ctx)?;
        }
        Ok(buf)
    }

    fn write_sidecars(&self, spec: &OpSpec, args: &Args, ctx: &OpCtx, argv: &[String], timing_ms: u64) -> Result<()> {
        if ctx.outputs.is_empty() || !ctx.globals.sidecar {
            return Ok(());
        }
        let inputs: Vec<HashedFile> = ctx
            .input_records
            .iter()
            .map(|r| HashedFile {
                path: r.path.clone(),
                sha256: if r.path == "-" { None } else { sha256_file(Path::new(&r.path)).ok() },
                duration_s: r.duration_s,
                role: None,
            })
            .collect();
        let outputs: Vec<HashedFile> = ctx
            .outputs
            .iter()
            .filter(|o| o.path != "-")
            .map(|o| HashedFile {
                path: o.path.clone(),
                sha256: sha256_file(Path::new(&o.path)).ok(),
                duration_s: o.duration_s,
                role: o.role.clone(),
            })
            .collect();
        let mut params = args.to_json();
        for (k, v) in &ctx.notes {
            params.insert(k.clone(), v.clone());
        }
        let deterministic = match ctx.deterministic {
            Some(false) if !ctx.plugins.is_empty() => json!("unknown"),
            Some(false) => json!(false),
            _ => json!(true),
        };
        let sc = Sidecar {
            ferraus_version: FERRAUS_VERSION.into(),
            schema_version: SCHEMA_VERSION,
            command: spec.name.into(),
            argv: argv.to_vec(),
            params,
            inputs,
            outputs: outputs.clone(),
            models: ctx.models.iter().map(|m| serde_json::to_value(m).unwrap()).collect(),
            plugins: ctx.plugins.clone(),
            host: HostInfo::current(&ctx.globals.device, ctx.threads()),
            deterministic,
            warnings: ctx.warnings.iter().map(|w| serde_json::to_value(w).unwrap()).collect(),
            timing_ms,
        };
        let text = serde_json::to_vec_pretty(&sc)?;
        for o in &outputs {
            let p = Sidecar::path_for(Path::new(&o.path));
            // never write a sidecar for a sidecar
            if o.path.ends_with(".ferraus.json") {
                continue;
            }
            std::fs::write(p, &text)?;
        }
        Ok(())
    }
}

/// Default output extension: same as the input when this build can encode it, else wav.
pub fn default_ext(ctx: &OpCtx) -> String {
    if let Some(f) = &ctx.globals.format {
        return f.clone();
    }
    let ext = ctx
        .inputs
        .first()
        .and_then(|p| p.extension())
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "wav".into());
    if ctx.io.can_encode(&ext) {
        ext
    } else {
        "wav".into()
    }
}

/// Apply `--sample-rate` and `--channels` overrides.
pub fn conform_output(mut buf: AudioBuffer, g: &Globals) -> AudioBuffer {
    if let Some(sr) = g.sample_rate {
        if sr != buf.sample_rate {
            buf = resample(&buf, sr, Quality::High);
        }
    }
    if let Some(ch) = g.channels {
        if ch != buf.channels() {
            buf = buf.with_channels(ch);
        }
    }
    buf
}

/// Run a Transform spec with generic dry/wet handling.
pub fn apply_spec(spec: &OpSpec, args: &Args, input: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let f = match spec.runner {
        Runner::Transform(f) => f,
        _ => return Err(Error::usage(format!("'{}' cannot be used as an effect stage", spec.name))),
    };
    let generic_mix = spec.auto_mix && spec.find_param("mix").is_none();
    if generic_mix {
        let m = args.percent("mix").unwrap_or(1.0);
        if m <= 0.0 {
            return Ok(input);
        }
        if m >= 1.0 {
            return f(args, input, ctx);
        }
        let dry = input.clone();
        let wet = f(args, input, ctx)?;
        return Ok(blend(&dry, &wet, m as f32));
    }
    f(args, input, ctx)
}

/// `dry + m·(wet − dry)`, matching lengths and channels to the wet signal.
pub fn blend(dry: &AudioBuffer, wet: &AudioBuffer, m: f32) -> AudioBuffer {
    let dry = if dry.channels() != wet.channels() { dry.with_channels(wet.channels()) } else { dry.clone() };
    let mut out = wet.clone();
    for (oc, dc) in out.data.iter_mut().zip(&dry.data) {
        for (i, o) in oc.iter_mut().enumerate() {
            let d = dc.get(i).copied().unwrap_or(0.0);
            *o = d + m * (*o - d);
        }
    }
    out
}

/// Parse `fx` stages from `--fx` strings (and/or `chain` JSON from pipelines).
pub fn chain_stages(args: &Args) -> Result<Vec<(String, Vec<(String, String)>)>> {
    let mut stages = Vec::new();
    for s in args.all("fx") {
        stages.push(parse_stage_string(&s)?);
    }
    for c in args.all("chain") {
        let v: Value = serde_json::from_str(&c).map_err(|e| Error::usage(format!("chain: {e}")))?;
        let items = match v {
            Value::Array(a) => a,
            other => vec![other],
        };
        for it in items {
            stages.push(stage_from_json(&it)?);
        }
    }
    if stages.is_empty() {
        return Err(Error::usage("fx needs at least one --fx stage, e.g. --fx \"fade:in=1s\""));
    }
    Ok(stages)
}

/// `{ "op": "compress", "preset": "vocal", "ratio": "4:1" }` or `{op, args: {...}}`.
pub fn stage_from_json(v: &Value) -> Result<(String, Vec<(String, String)>)> {
    match v {
        Value::String(s) => parse_stage_string(s),
        Value::Object(o) => {
            let op =
                o.get("op").and_then(|x| x.as_str()).ok_or_else(|| Error::usage("each chain stage needs an 'op'"))?;
            let mut pairs = Vec::new();
            for (k, val) in o {
                if k == "op" || k == "id" {
                    continue;
                }
                if k == "args" {
                    pairs.extend(crate::op::json_to_pairs(val)?);
                } else {
                    pairs.extend(crate::op::json_to_pairs(&json!({ k: val }))?);
                }
            }
            Ok((op.to_string(), pairs))
        }
        _ => Err(Error::usage("chain stages must be strings or objects")),
    }
}

fn chain_plan(registry: &Registry, args: &Args, presets: &PresetLookup) -> Result<Value> {
    let mut plan = Vec::new();
    for (name, pairs) in chain_stages(args)? {
        let spec = registry.require(&name)?;
        if !spec.is_transform() {
            return Err(Error::usage(format!(
                "'{name}' cannot be used as an fx stage (it is not a single-input audio effect)"
            )));
        }
        let a = Args::build(spec, pairs, presets)?;
        plan.push(json!({"op": spec.name, "args": a.to_json()}));
    }
    Ok(json!({ "stages": plan }))
}

/// Run a named transform with raw pairs (used by fx chains, pipelines, and composite ops).
pub fn run_transform_pairs(
    registry: &Registry,
    presets: &PresetLookup,
    name: &str,
    pairs: Vec<(String, String)>,
    buf: AudioBuffer,
    ctx: &mut OpCtx,
) -> Result<AudioBuffer> {
    let spec = registry.require(name)?;
    if !spec.is_transform() {
        return Err(Error::usage(format!(
            "'{name}' cannot be used as an fx stage (it is not a single-input audio effect)"
        )));
    }
    let a = Args::build(spec, pairs, presets)?;
    apply_spec(spec, &a, buf, ctx)
}

/// Convenience for composite ops: `run_transform(ctx, "compress", &[("ratio", "1.5:1")], buf)`.
pub fn run_transform(ctx: &mut OpCtx, name: &str, pairs: &[(&str, &str)], buf: AudioBuffer) -> Result<AudioBuffer> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry in context"))?;
    let pairs = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    run_transform_pairs(registry, &|_, _| None, name, pairs, buf, ctx)
}

/// Build Args for a named op from pairs (defaults applied), for composite ops.
pub fn build_args(ctx: &OpCtx, name: &str, pairs: &[(&str, &str)]) -> Result<Args> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry in context"))?;
    let spec = registry.require(name)?;
    Args::build(spec, pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), &|_, _| None)
}

/// Run any registered op as a nested step, with its own inputs and output path. Returns
/// the op's result value and the outputs it wrote. Warnings and models are merged.
pub fn run_nested(
    ctx: &mut OpCtx,
    name: &str,
    pairs: &[(&str, &str)],
    inputs: Vec<PathBuf>,
    output: Option<&Path>,
) -> Result<(Value, Vec<OutputRecord>)> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry in context"))?;
    let spec = registry.require(name)?;
    let args = Args::build(spec, pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), &|_, _| None)?;
    let mut child = ctx.child(spec.name, inputs);
    child.globals.output = output.map(|p| p.display().to_string());
    child.globals.outdir = None;
    child.globals.in_place = false;
    let v = match spec.runner {
        Runner::Custom(f) => f(&args, &mut child)?,
        Runner::Transform(_) | Runner::Chain => {
            let out = child.output_path(None, "wav")?;
            let input = child.read_audio(0)?;
            let buf = apply_spec(spec, &args, input, &mut child)?;
            child.write_audio(&out, &buf, None)?;
            Value::Null
        }
    };
    let outs = child.outputs.clone();
    ctx.absorb(child);
    Ok((v, outs))
}

fn explain(spec: &OpSpec, args: &Args) -> Value {
    let mut lines = Vec::new();
    for p in spec.effective_params() {
        if let Some(v) = args.raw(p.name) {
            let why = if args.is_explicit(p.name) {
                "given on the command line"
            } else if p.default == Some(v) {
                "the default"
            } else {
                "from the preset"
            };
            lines.push(json!({"param": p.key(), "value": v, "meaning": p.help, "why": why}));
        }
    }
    Value::Array(lines)
}
