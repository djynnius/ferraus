//! Pipeline files: a graph of stages over named inputs, run with content-hash caching so
//! a re-run after editing one stage recomputes only what changed.

use crate::ctx::OpCtx;
use crate::envelope::{sha256_bytes, sha256_file};
use crate::error::{Error, Result};
use crate::op::{json_to_pairs, Args, Runner};
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
        let (k, val) = s
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("--set expects key=value, got '{s}'")))?;
        apply_set(&mut v, k, val)?;
    }
    let p: PipelineFile =
        serde_json::from_value(v).map_err(|e| Error::usage(format!("pipeline: {e}")))?;
    if p.version != 1 {
        return Err(Error::usage(format!(
            "pipeline version {} is not supported (expected 1)",
            p.version
        )));
    }
    Ok(p)
}

/// `stages.tuned.args.strength=90%` (stages addressed by id), `inputs.vocal=x.wav`,
/// `outputs.0.path=out.wav`.
fn apply_set(v: &mut Value, key: &str, val: &str) -> Result<()> {
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
                arr.get_mut(idx)
                    .ok_or_else(|| Error::usage(format!("--set: index {idx} out of range")))?
            }
            Value::Object(obj) => {
                if last {
                    obj.insert(part.to_string(), Value::String(val.to_string()));
                    return Ok(());
                }
                obj.entry(part.to_string())
                    .or_insert_with(|| Value::Object(Map::new()))
            }
            _ => return Err(Error::usage(format!("--set: cannot descend into '{part}'"))),
        };
    }
    *cur = Value::String(val.to_string());
    Ok(())
}

/// Order stages so every stage comes after the ones it reads from; error on cycles and
/// unknown references.
pub fn topo_order(p: &PipelineFile) -> Result<Vec<usize>> {
    let ids: HashMap<&str, usize> = p
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
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
    fn visit(
        i: usize,
        deps: &[Vec<usize>],
        state: &mut [u8],
        order: &mut Vec<usize>,
        p: &PipelineFile,
    ) -> Result<()> {
        match state[i] {
            2 => return Ok(()),
            1 => {
                return Err(Error::usage(format!(
                    "pipeline has a cycle through stage '{}'",
                    p.stages[i].id
                )))
            }
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
            return Err(Error::usage(format!(
                "output reads '{}', which is not a stage",
                o.from
            )));
        }
    }
    Ok(order)
}

/// Validate a pipeline against the registry: ops exist, args parse, graph is acyclic.
pub fn validate(p: &PipelineFile, registry: &Registry) -> Result<Vec<Value>> {
    topo_order(p)?;
    let mut resolved = Vec::new();
    for s in &p.stages {
        let spec = registry
            .require(&s.op)
            .map_err(|e| Error::usage(format!("stage '{}': {}", s.id, e.message)))?;
        let pairs = stage_pairs(s)?;
        let args = Args::build(spec, pairs, &|_, _| None)
            .map_err(|e| Error::usage(format!("stage '{}': {}", s.id, e.message)))?;
        if let Runner::Chain = spec.runner {
            crate::exec::chain_stages(&args)
                .map_err(|e| Error::usage(format!("stage '{}': {}", s.id, e.message)))?;
        }
        resolved.push(json!({"id": s.id, "op": spec.name, "in": s.input.as_ref().map(|i| i.refs()), "args": args.to_json()}));
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
pub fn run(
    p: &PipelineFile,
    base_dir: &Path,
    ctx: &mut OpCtx,
    use_cache: bool,
) -> Result<RunReport> {
    let registry = ctx
        .registry
        .ok_or_else(|| Error::internal("pipeline needs a registry"))?;
    validate(p, registry)?;
    let order = topo_order(p)?;
    let cache_root = crate::paths::pipeline_cache_dir();
    std::fs::create_dir_all(&cache_root)?;

    // name → (key, map role→path)
    let mut produced: HashMap<String, (String, BTreeMap<String, PathBuf>)> = HashMap::new();
    for (name, path) in &p.inputs {
        let pb = resolve_path(base_dir, path);
        let h = sha256_file(&pb).map_err(|_| {
            Error::input(format!(
                "pipeline input '{name}' not found: {}",
                pb.display()
            ))
        })?;
        let mut m = BTreeMap::new();
        m.insert("main".to_string(), pb);
        produced.insert(name.clone(), (h, m));
    }

    let mut report = RunReport {
        stages: vec![],
        outputs: vec![],
        computed: 0,
        cached: 0,
    };
    let n = order.len();
    for (k, &i) in order.iter().enumerate() {
        ctx.check_cancel()?;
        let s = &p.stages[i];
        let spec = registry.require(&s.op)?;
        ctx.progress.stage(&s.id, k + 1, n);
        let pairs = stage_pairs(s)?;
        let args = Args::build(spec, pairs.clone(), &|_, _| None)?;
        let mut input_paths = Vec::new();
        let mut input_keys = Vec::new();
        for r in s.input.as_ref().map(|x| x.refs()).unwrap_or_default() {
            let (base, role) = match r.split_once('.') {
                Some((b, role)) if produced.contains_key(b) => (b.to_string(), role.to_string()),
                _ => (r.clone(), "main".to_string()),
            };
            let (key, files) = produced
                .get(&base)
                .ok_or_else(|| Error::internal(format!("missing stage result {base}")))?;
            let path = files
                .get(&role)
                .or_else(|| if role == "main" { files.values().next() } else { None })
                .ok_or_else(|| Error::usage(format!("stage '{}' reads '{r}', but '{base}' has no output named '{role}' (has: {})", s.id, files.keys().cloned().collect::<Vec<_>>().join(", "))))?;
            input_paths.push(path.clone());
            input_keys.push(format!("{key}:{role}"));
        }
        let key_src = json!({"op": spec.name, "args": args.to_json(), "inputs": input_keys, "v": crate::envelope::FERRAUS_VERSION});
        let key = sha256_bytes(serde_json::to_string(&key_src)?.as_bytes());
        let dir = cache_root.join(&key);
        let manifest = dir.join("manifest.json");
        let t0 = Instant::now();
        let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut was_cached = false;
        if use_cache && manifest.exists() {
            if let Ok(m) =
                serde_json::from_slice::<BTreeMap<String, PathBuf>>(&std::fs::read(&manifest)?)
            {
                if m.values().all(|p| p.exists()) {
                    files = m;
                    was_cached = true;
                }
            }
        }
        let mut result = Value::Null;
        if !was_cached {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            let out_template = dir.join("{stem}.wav");
            let mut child = ctx.child(spec.name, input_paths.clone());
            child.globals.output = Some(out_template.display().to_string());
            child.globals.outdir = None;
            child.globals.overwrite = true;
            child.globals.in_place = false;
            child.globals.bit_depth = Some(32);
            child.globals.format = None;
            child.globals.sample_rate = None;
            child.globals.channels = None;
            let v = match spec.runner {
                Runner::Custom(f) => f(&args, &mut child)?,
                Runner::Transform(_) => {
                    let input = child.read_audio(0)?;
                    let out = crate::exec::apply_spec(spec, &args, input, &mut child)?;
                    let path = dir.join("main.wav");
                    child.write_audio(&path, &out, Some("main"))?;
                    Value::Null
                }
                Runner::Chain => {
                    let input = child.read_audio(0)?;
                    let mut buf = input;
                    for (name, pairs) in crate::exec::chain_stages(&args)? {
                        buf = crate::exec::run_transform_pairs(
                            registry,
                            &|_, _| None,
                            &name,
                            pairs,
                            buf,
                            &mut child,
                        )?;
                    }
                    let path = dir.join("main.wav");
                    child.write_audio(&path, &buf, Some("main"))?;
                    Value::Null
                }
            };
            result = v;
            for o in &child.outputs {
                let role = o.role.clone().unwrap_or_else(|| "main".to_string());
                files.insert(role, PathBuf::from(&o.path));
            }
            for (k2, v2) in &child.notes {
                result = if result.is_null() { json!({}) } else { result };
                if let Value::Object(m) = &mut result {
                    m.insert(k2.clone(), v2.clone());
                }
            }
            ctx.absorb(child);
            std::fs::write(&manifest, serde_json::to_vec_pretty(&files)?)?;
            report.computed += 1;
        } else {
            report.cached += 1;
        }
        report.stages.push(json!({
            "id": s.id, "op": spec.name, "args": args.to_json(), "cached": was_cached,
            "timing_ms": t0.elapsed().as_millis() as u64, "key": key, "result": result,
            "outputs": files.keys().cloned().collect::<Vec<_>>(),
        }));
        produced.insert(s.id.clone(), (key, files));
    }

    for o in &p.outputs {
        let (base, role) = match o.from.split_once('.') {
            Some((b, r)) => (b.to_string(), r.to_string()),
            None => (o.from.clone(), "main".to_string()),
        };
        let (_, files) = produced
            .get(&base)
            .ok_or_else(|| Error::usage(format!("unknown output source {}", o.from)))?;
        let src = files
            .get(&role)
            .or_else(|| {
                if role == "main" {
                    files.values().next()
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::usage(format!("'{}' has no output '{role}'", base)))?;
        let dest = resolve_path(base_dir, &o.path);
        let buf = ctx.read_audio_path(src)?;
        ctx.input_records.pop();
        let mut opts = ctx.encode_options(&dest);
        if let Some(b) = o.encode.get("bit_depth") {
            opts.bit_depth = b
                .as_u64()
                .map(|v| v as u16)
                .or_else(|| b.as_str().and_then(|s| s.parse().ok()));
        }
        if let Some(b) = o.encode.get("bitrate").and_then(|v| v.as_str()) {
            opts.bitrate = Some(crate::units::parse_bitrate(b)?);
        }
        let mut buf = buf;
        if let Some(sr) = o.encode.get("sample_rate").and_then(|v| v.as_u64()) {
            buf = crate::resample::resample(&buf, sr as u32, crate::resample::Quality::High);
        }
        ctx.write_audio_with(&dest, &buf, Some(&o.from), &opts)?;
        report
            .outputs
            .push(json!({"from": o.from, "path": dest.display().to_string()}));
    }
    Ok(report)
}

fn resolve_path(base: &Path, p: &str) -> PathBuf {
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
        assert_eq!(
            p.outputs[0]
                .encode
                .get("bit_depth")
                .and_then(|v| v.as_u64()),
            Some(24)
        );
    }

    #[test]
    fn set_overrides() {
        let p = parse(
            EX,
            &[
                "stages.tuned.args.strength=90%".into(),
                "inputs.vocal=x.wav".into(),
            ],
        )
        .unwrap();
        assert_eq!(p.stages[1].args["strength"], "90%");
        assert_eq!(p.inputs["vocal"], "x.wav");
    }

    #[test]
    fn detects_cycles_and_bad_refs() {
        let bad =
            "version: 1\nstages:\n  - {id: a, op: gain, in: b}\n  - {id: b, op: gain, in: a}\n";
        assert!(topo_order(&parse(bad, &[]).unwrap()).is_err());
        let bad2 = "version: 1\nstages:\n  - {id: a, op: gain, in: nope}\n";
        assert!(topo_order(&parse(bad2, &[]).unwrap()).is_err());
    }
}
