//! Pipeline files: a graph of stages over named inputs, run with content-hash caching so
//! a re-run after editing one stage recomputes only what changed.

use crate::ctx::OpCtx;
use crate::envelope::{sha256_bytes, sha256_file};
use crate::error::{Error, Result};
use crate::op::{json_to_pairs, Args, OpSpec, Runner};
use crate::registry::Registry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PipelineFile {
    pub version: u32,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Stage {
    pub id: String,
    pub op: String,
    #[serde(rename = "in", default)]
    pub input: Option<StageInput>,
    #[serde(default)]
    pub args: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StageInput {
    One(String),
    Many(Vec<String>),
}

impl StageInput {
    pub fn refs(&self) -> Vec<String> {
        match self {
            StageInput::One(s) => vec![s.clone()],
            StageInput::Many(v) => v.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OutputSpec {
    pub from: String,
    pub path: String,
    #[serde(flatten)]
    pub encode: BTreeMap<String, Value>,
}

/// Parse YAML or JSON pipeline text, applying `--set key=value` overrides.
pub fn parse(text: &str, sets: &[String]) -> Result<PipelineFile> {
    let mut v: Value = if text.trim_start().starts_with('{') {
        serde_json::from_str(text).map_err(|e| Error::usage(format!("pipeline JSON: {e}")))?
    } else {
        serde_yaml::from_str(text).map_err(|e| Error::usage(format!("pipeline YAML: {e}")))?
    };
    for s in sets {
        let (k, val) = s.split_once('=').ok_or_else(|| Error::usage(format!("--set expects key=value, got '{s}'")))?;
        apply_set(&mut v, k, val)?;
    }
    let p: PipelineFile = serde_json::from_value(v).map_err(|e| Error::usage(format!("pipeline: {e}")))?;
    if p.version != 1 {
        return Err(Error::usage(format!("pipeline version {} is not supported (expected 1)", p.version)));
    }
    Ok(p)
}

/// `stages.tuned.args.strength=90%` (stages addressed by id), `inputs.vocal=x.wav`,
/// `outputs.0.path=out.wav`.
fn apply_set(v: &mut Value, key: &str, val: &str) -> Result<()> {
    // JSON arrays/objects in --set values are parsed (e.g. a replacement fx chain)
    let parsed = if val.trim_start().starts_with(['[', '{']) {
        serde_json::from_str::<Value>(val).unwrap_or_else(|_| Value::String(val.to_string()))
    } else {
        Value::String(val.to_string())
    };
    let parts: Vec<&str> = key.split('.').collect();
    let mut cur = v;
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        cur = match cur {
            Value::Array(arr) => {
                let idx = if let Ok(n) = part.parse::<usize>() {
                    n
                } else {
                    arr.iter()
                        .position(|s| s.get("id").and_then(|x| x.as_str()) == Some(part))
                        .ok_or_else(|| Error::usage(format!("--set: no stage with id '{part}'")))?
                };
                arr.get_mut(idx).ok_or_else(|| Error::usage(format!("--set: index {idx} out of range")))?
            }
            Value::Object(obj) => {
                if last {
                    obj.insert(part.to_string(), parsed);
                    return Ok(());
                }
                obj.entry(part.to_string()).or_insert_with(|| Value::Object(Map::new()))
            }
            _ => return Err(Error::usage(format!("--set: cannot descend into '{part}'"))),
        };
    }
    *cur = parsed;
    Ok(())
}

/// Order stages so every stage comes after the ones it reads from; error on cycles and
/// unknown references.
pub fn topo_order(p: &PipelineFile) -> Result<Vec<usize>> {
    let ids: HashMap<&str, usize> = p.stages.iter().enumerate().map(|(i, s)| (s.id.as_str(), i)).collect();
    if ids.len() != p.stages.len() {
        return Err(Error::usage("pipeline stage ids must be unique"));
    }
    let mut deps: Vec<Vec<usize>> = vec![vec![]; p.stages.len()];
    for (i, s) in p.stages.iter().enumerate() {
        for r in s.input.as_ref().map(|x| x.refs()).unwrap_or_default() {
            let base = r.split('.').next().unwrap_or(&r);
            if let Some(&j) = ids.get(base) {
                deps[i].push(j);
            } else if !p.inputs.contains_key(base) && !p.inputs.contains_key(&r) {
                return Err(Error::usage(format!(
                    "stage '{}' reads '{r}', which is neither an input nor a stage",
                    s.id
                )));
            }
        }
    }
    let mut order = Vec::new();
    let mut state = vec![0u8; p.stages.len()];
    fn visit(i: usize, deps: &[Vec<usize>], state: &mut [u8], order: &mut Vec<usize>, p: &PipelineFile) -> Result<()> {
        match state[i] {
            2 => return Ok(()),
            1 => return Err(Error::usage(format!("pipeline has a cycle through stage '{}'", p.stages[i].id))),
            _ => {}
        }
        state[i] = 1;
        for &d in &deps[i] {
            visit(d, deps, state, order, p)?;
        }
        state[i] = 2;
        order.push(i);
        Ok(())
    }
    for i in 0..p.stages.len() {
        visit(i, &deps, &mut state, &mut order, p)?;
    }
    for o in &p.outputs {
        let base = o.from.split('.').next().unwrap_or(&o.from);
        if !ids.contains_key(base) && !p.inputs.contains_key(base) {
            return Err(Error::usage(format!("output reads '{}', which is not a stage", o.from)));
        }
    }
    Ok(order)
}

/// Validate a pipeline against the registry: ops exist, args parse, graph is acyclic.
pub fn validate(p: &PipelineFile, registry: &Registry) -> Result<Vec<Value>> {
    topo_order(p)?;
    let mut resolved = Vec::new();
    for s in &p.stages {
        let spec = registry.require(&s.op).map_err(|e| Error::usage(format!("stage '{}': {}", s.id, e.message)))?;
        let pairs = stage_pairs(s)?;
        let args = Args::build(spec, pairs, &|_, _| None)
            .map_err(|e| Error::usage(format!("stage '{}': {}", s.id, e.message)))?;
        if let Runner::Chain = spec.runner {
            crate::exec::chain_stages(&args).map_err(|e| Error::usage(format!("stage '{}': {}", s.id, e.message)))?;
        }
        resolved.push(
            json!({"id": s.id, "op": spec.name, "in": s.input.as_ref().map(|i| i.refs()), "args": args.to_json()}),
        );
    }
    Ok(resolved)
}

/// Stage args → raw pairs. A `chain` array is passed as JSON text.
fn stage_pairs(s: &Stage) -> Result<Vec<(String, String)>> {
    let mut args = s.args.clone();
    let mut chain = None;
    if let Value::Object(o) = &mut args {
        if let Some(c) = o.remove("chain") {
            chain = Some(c);
        }
    }
    let mut pairs = json_to_pairs(&args)?;
    if let Some(c) = chain {
        pairs.push(("chain".into(), serde_json::to_string(&c)?));
    }
    Ok(pairs)
}

/// The result of running a pipeline.
#[derive(Debug, Serialize)]
pub struct RunReport {
    pub stages: Vec<Value>,
    pub outputs: Vec<Value>,
    pub computed: usize,
    pub cached: usize,
}

/// Run the pipeline. Intermediate results are float WAV files in the stage cache, keyed
/// by a hash of (op, resolved args, input content).
pub fn run(p: &PipelineFile, base_dir: &Path, ctx: &mut OpCtx, use_cache: bool) -> Result<RunReport> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("pipeline needs a registry"))?;
    validate(p, registry)?;
    let order = topo_order(p)?;
    // refuse output conflicts before computing anything
    for o in &p.outputs {
        ctx.prepare_output(&resolve_path(base_dir, &o.path))?;
    }
    let cache_root = crate::paths::pipeline_cache_dir();
    std::fs::create_dir_all(&cache_root)?;

    // name → (key, map role→path)
    let mut produced: HashMap<String, (String, BTreeMap<String, PathBuf>)> = HashMap::new();
    for (name, path) in &p.inputs {
        let pb = resolve_path(base_dir, path);
        let h = sha256_file(&pb)
            .map_err(|_| Error::input(format!("pipeline input '{name}' not found: {}", pb.display())))?;
        let mut m = BTreeMap::new();
        m.insert("main".to_string(), pb);
        produced.insert(name.clone(), (h, m));
    }

    let mut report = RunReport { stages: vec![], outputs: vec![], computed: 0, cached: 0 };
    // dependency levels: stages in the same level do not depend on each other and run in
    // parallel (each on its own thread with its own context)
    let ids: HashMap<&str, usize> = p.stages.iter().enumerate().map(|(i, s)| (s.id.as_str(), i)).collect();
    let mut level = vec![0usize; p.stages.len()];
    for &i in &order {
        for r in p.stages[i].input.as_ref().map(|x| x.refs()).unwrap_or_default() {
            let base = r.split('.').next().unwrap_or(&r);
            if let Some(&j) = ids.get(base) {
                level[i] = level[i].max(level[j] + 1);
            }
        }
    }
    let max_level = level.iter().copied().max().unwrap_or(0);
    let n = order.len();
    let mut done = 0usize;
    let mut stage_reports: HashMap<usize, Value> = HashMap::new();
    for lv in 0..=max_level {
        ctx.check_cancel()?;
        let members: Vec<usize> = order.iter().copied().filter(|&i| level[i] == lv).collect();
        // resolve inputs, cache keys, and cache hits sequentially
        struct Job<'r> {
            i: usize,
            spec: &'r OpSpec,
            args: Args,
            inputs: Vec<PathBuf>,
            key: String,
            dir: PathBuf,
        }
        let mut jobs: Vec<Job> = Vec::new();
        for &i in &members {
            let s = &p.stages[i];
            let spec = registry.require(&s.op)?;
            done += 1;
            ctx.progress.stage(&s.id, done, n);
            let args = Args::build(spec, stage_pairs(s)?, &|_, _| None)?;
            let mut input_paths = Vec::new();
            let mut input_keys = Vec::new();
            for r in s.input.as_ref().map(|x| x.refs()).unwrap_or_default() {
                let (base, role) = match r.split_once('.') {
                    Some((b, role)) if produced.contains_key(b) => (b.to_string(), role.to_string()),
                    _ => (r.clone(), "main".to_string()),
                };
                let (key, files) =
                    produced.get(&base).ok_or_else(|| Error::internal(format!("missing stage result {base}")))?;
                let path = files
                    .get(&role)
                    .or_else(|| if role == "main" { files.values().next() } else { None })
                    .ok_or_else(|| {
                        Error::usage(format!(
                            "stage '{}' reads '{r}', but '{base}' has no output named '{role}' (has: {})",
                            s.id,
                            files.keys().cloned().collect::<Vec<_>>().join(", ")
                        ))
                    })?;
                input_paths.push(path.clone());
                input_keys.push(format!("{key}:{role}"));
            }
            let key_src = json!({"op": spec.name, "args": args.to_json(), "inputs": input_keys, "v": crate::envelope::FERRAUS_VERSION});
            let key = sha256_bytes(serde_json::to_string(&key_src)?.as_bytes());
            let dir = cache_root.join(&key);
            let manifest = dir.join("manifest.json");
            if use_cache && manifest.exists() {
                if let Ok(m) = serde_json::from_slice::<BTreeMap<String, PathBuf>>(&std::fs::read(&manifest)?) {
                    if m.values().all(|p| p.exists()) {
                        report.cached += 1;
                        stage_reports.insert(
                            i,
                            json!({
                                "id": s.id, "op": spec.name, "args": args.to_json(), "cached": true, "timing_ms": 0,
                                "key": key, "result": Value::Null, "outputs": m.keys().cloned().collect::<Vec<_>>(),
                            }),
                        );
                        produced.insert(s.id.clone(), (key, m));
                        continue;
                    }
                }
            }
            jobs.push(Job { i, spec, args, inputs: input_paths, key, dir });
        }
        // compute the rest in parallel
        type Done<'c> = (usize, Result<(BTreeMap<String, PathBuf>, Value, OpCtx<'c>)>, u128);
        let mut children = Vec::new();
        for j in &jobs {
            let _ = std::fs::remove_dir_all(&j.dir);
            std::fs::create_dir_all(&j.dir)?;
            let mut child = ctx.child(j.spec.name, j.inputs.clone());
            child.globals.output = Some(j.dir.join("{stem}.wav").display().to_string());
            child.globals.outdir = None;
            child.globals.overwrite = true;
            child.globals.in_place = false;
            child.globals.bit_depth = Some(32);
            child.globals.format = None;
            child.globals.sample_rate = None;
            child.globals.channels = None;
            children.push(child);
        }
        let results: Vec<Done> = std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .iter()
                .zip(children)
                .map(|(j, mut child)| {
                    scope.spawn(move || {
                        let t0 = Instant::now();
                        let r =
                            compute_stage(registry, j.spec, &j.args, &j.dir, &mut child).map(|(f, v)| (f, v, child));
                        (j.i, r, t0.elapsed().as_millis())
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("pipeline stage thread panicked")).collect()
        });
        for (j, (i, r, ms)) in jobs.iter().zip(results) {
            let (files, result, child) = r?;
            ctx.absorb(child);
            std::fs::write(j.dir.join("manifest.json"), serde_json::to_vec_pretty(&files)?)?;
            report.computed += 1;
            let s = &p.stages[i];
            stage_reports.insert(
                i,
                json!({
                    "id": s.id, "op": j.spec.name, "args": j.args.to_json(), "cached": false, "timing_ms": ms as u64,
                    "key": j.key, "result": result, "outputs": files.keys().cloned().collect::<Vec<_>>(),
                }),
            );
            produced.insert(s.id.clone(), (j.key.clone(), files));
        }
    }
    for &i in &order {
        if let Some(r) = stage_reports.remove(&i) {
            report.stages.push(r);
        }
    }

    for o in &p.outputs {
        let (base, role) = match o.from.split_once('.') {
            Some((b, r)) => (b.to_string(), r.to_string()),
            None => (o.from.clone(), "main".to_string()),
        };
        let (_, files) =
            produced.get(&base).ok_or_else(|| Error::usage(format!("unknown output source {}", o.from)))?;
        let src = files
            .get(&role)
            .or_else(|| if role == "main" { files.values().next() } else { None })
            .ok_or_else(|| Error::usage(format!("'{}' has no output '{role}'", base)))?;
        let dest = resolve_path(base_dir, &o.path);
        let buf = ctx.read_audio_path(src)?;
        ctx.input_records.pop();
        let mut opts = ctx.encode_options(&dest);
        if let Some(b) = o.encode.get("bit_depth") {
            opts.bit_depth = b.as_u64().map(|v| v as u16).or_else(|| b.as_str().and_then(|s| s.parse().ok()));
        }
        if let Some(b) = o.encode.get("bitrate").and_then(|v| v.as_str()) {
            opts.bitrate = Some(crate::units::parse_bitrate(b)?);
        }
        if let Some(f) = o.encode.get("format").and_then(|v| v.as_str()) {
            opts.format = Some(f.to_string());
        }
        let mut buf = buf;
        let num =
            |k: &str| o.encode.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
        if let Some(sr) = num("sample_rate") {
            buf = crate::resample::resample(&buf, sr as u32, crate::resample::Quality::High);
        }
        if let Some(ch) = num("channels") {
            buf = buf.with_channels(ch as usize);
        }
        for k in o.encode.keys() {
            if !["bit_depth", "bitrate", "sample_rate", "channels", "format"].contains(&k.as_str()) {
                ctx.warn("unknown_output_key", format!("output '{}': '{k}' is not an output option (bit_depth, bitrate, sample_rate, channels, format)", o.path));
            }
        }
        ctx.write_audio_with(&dest, &buf, Some(&o.from), &opts)?;
        report.outputs.push(json!({"from": o.from, "path": dest.display().to_string()}));
    }
    Ok(report)
}

/// Run one stage into its cache directory; returns its output files (role → path) and result.
fn compute_stage(
    registry: &Registry,
    spec: &OpSpec,
    args: &Args,
    dir: &Path,
    child: &mut OpCtx,
) -> Result<(BTreeMap<String, PathBuf>, Value)> {
    let v = match spec.runner {
        Runner::Custom(f) => f(args, child)?,
        Runner::Transform(_) => {
            let input = child.read_audio(0)?;
            let out = crate::exec::apply_spec(spec, args, input, child)?;
            child.write_audio(&dir.join("main.wav"), &out, Some("main"))?;
            Value::Null
        }
        Runner::Chain => {
            let mut buf = child.read_audio(0)?;
            for (name, pairs) in crate::exec::chain_stages(args)? {
                buf = crate::exec::run_transform_pairs(registry, &|_, _| None, &name, pairs, buf, child)?;
            }
            child.write_audio(&dir.join("main.wav"), &buf, Some("main"))?;
            Value::Null
        }
    };
    let mut files = BTreeMap::new();
    for o in &child.outputs {
        files.insert(o.role.clone().unwrap_or_else(|| "main".to_string()), PathBuf::from(&o.path));
    }
    let mut result = v;
    for (k, val) in &child.notes {
        if result.is_null() {
            result = json!({});
        }
        if let Value::Object(m) = &mut result {
            m.insert(k.clone(), val.clone());
        }
    }
    Ok((files, result))
}

pub(crate) fn resolve_path(base: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        base.join(pb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EX: &str = r#"
version: 1
inputs:
  vocal: takes/vocal_take3.wav
  beat: beats/instrumental.wav
stages:
  - id: clean
    op: denoise
    in: vocal
    args: { auto: true, reduction: 12dB }
  - id: tuned
    op: tune
    in: clean
    args: { key: auto, strength: 85%, retune_speed: 30ms }
  - id: mix
    op: mix
    in: [tuned, beat]
    args: { gains: [0dB, -3dB] }
outputs:
  - { from: mix, path: out/song.wav, bit_depth: 24 }
"#;

    #[test]
    fn parses_and_orders() {
        let p = parse(EX, &[]).unwrap();
        assert_eq!(p.stages.len(), 3);
        let o = topo_order(&p).unwrap();
        assert_eq!(o, vec![0, 1, 2]);
        assert_eq!(p.outputs[0].encode.get("bit_depth").and_then(|v| v.as_u64()), Some(24));
    }

    #[test]
    fn set_overrides() {
        let p = parse(EX, &["stages.tuned.args.strength=90%".into(), "inputs.vocal=x.wav".into()]).unwrap();
        assert_eq!(p.stages[1].args["strength"], "90%");
        assert_eq!(p.inputs["vocal"], "x.wav");
    }

    #[test]
    fn detects_cycles_and_bad_refs() {
        let bad = "version: 1\nstages:\n  - {id: a, op: gain, in: b}\n  - {id: b, op: gain, in: a}\n";
        assert!(topo_order(&parse(bad, &[]).unwrap()).is_err());
        let bad2 = "version: 1\nstages:\n  - {id: a, op: gain, in: nope}\n";
        assert!(topo_order(&parse(bad2, &[]).unwrap()).is_err());
    }
}
