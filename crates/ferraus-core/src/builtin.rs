//! Ops implemented by the core itself: `fx`, `pipeline`, `presets`, `schema`, `describe`,
//! `replay`, and `verify`.

use crate::ctx::OpCtx;
use crate::envelope::{sha256_file, Sidecar};
use crate::error::{Error, Result};
use crate::exec::{Invocation, Runtime};
use crate::op::{param, Args, Category, Inputs, OpSpec, ParamKind, Runner};
use crate::registry::Registry;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::new("fx", Category::Chains, "Apply a chain of effects in order in one pass", Runner::Chain)
            .about("Every effect command is also an fx stage with the same parameter names. Stages are written as 'name:key=value,key=value'; a bare key means true (\"denoise:auto\").")
            .param(param("fx", ParamKind::Str).repeatable().help("One stage, e.g. \"fade:in=1s,out=2s\"; repeat for more stages"))
            .param(param("chain", ParamKind::Str).repeatable().advanced().help("Stages as a JSON array of {op, ...args} objects (used by pipeline files)"))
            .no_mix()
            .example("ferraus fx take.wav -o clean.wav --fx \"denoise:auto\" --fx \"fade:in=1s,out=2s\" --fx \"normalize:lufs=-14\"", "Denoise, fade, and normalize in one run")
            .example("ferraus fx vox.wav -o vox_fx.wav --fx \"compress:preset=vocal\" --fx deess --fx \"reverb:type=plate,mix=18%\"", "A vocal chain")
            .example("ferraus fx drums.wav -o drums_fx.wav --fx \"highpass:freq=40Hz\" --fx \"compress:ratio=4:1,attack=5ms\" --fx \"limit:ceiling=-1dBTP\" --dry-run --json", "Plan a drum-bus chain and print the resolved stages without processing"),
        OpSpec::custom("pipeline run", Category::Chains, "Run a pipeline file (YAML or JSON) with stage caching", pipeline_run)
            .param(param("set", ParamKind::Str).repeatable().help("Override any field, e.g. stages.tuned.args.strength=90%"))
            .param(param("no-cache", ParamKind::Flag).help("Recompute every stage instead of reusing cached results"))
            .inputs(Inputs::exactly(1, crate::op::InputKind::Any))
            .example("ferraus pipeline run job.yaml", "Run every stage and write the outputs listed in the file")
            .example("ferraus pipeline run job.yaml --set stages.tuned.args.strength=90% --json", "Override one stage parameter for this run and get a JSON result")
            .example("ferraus pipeline run job.yaml --no-cache --overwrite", "Recompute every stage from scratch and replace existing outputs"),
        OpSpec::custom("pipeline validate", Category::Chains, "Check a pipeline file without running it", pipeline_validate)
            .param(param("set", ParamKind::Str).repeatable().help("Override any field before validating"))
            .inputs(Inputs::exactly(1, crate::op::InputKind::Any))
            .example("ferraus pipeline validate job.yaml --json", "Validate stage names, arguments, and references")
            .example("ferraus pipeline validate job.json", "Check a JSON pipeline file; exit code 2 means a stage or argument is wrong")
            .example("ferraus pipeline validate job.yaml --set stages.master.args.lufs=-16LUFS", "Check that an override is valid before running it"),
        OpSpec::custom("pipeline explain", Category::Chains, "Print the resolved plan with every default filled in", pipeline_validate)
            .param(param("set", ParamKind::Str).repeatable().help("Override any field before explaining"))
            .inputs(Inputs::exactly(1, crate::op::InputKind::Any))
            .example("ferraus pipeline explain job.yaml", "Show each stage with all parameters resolved")
            .example("ferraus pipeline explain job.yaml --json", "The resolved plan as JSON, for an agent to inspect")
            .example("ferraus pipeline explain job.yaml --set stages.clean.args.amount=12dB", "See how an override changes the plan"),
        OpSpec::custom("presets list", Category::Chains, "List built-in and user presets", presets_list)
            .param(param("effect", ParamKind::Str).help("Only presets for this effect"))
            .positional("effect")
            .example("ferraus presets list compress", "Presets for the compressor")
            .example("ferraus presets list", "Every built-in and user preset, grouped by effect")
            .example("ferraus presets list reverb --json", "Reverb presets as JSON"),
        OpSpec::custom("presets show", Category::Chains, "Show a preset's values, e.g. compress/tighten", presets_show)
            .param(param("name", ParamKind::Str).required().help("effect/preset, e.g. compress/vocal"))
            .positional("name")
            .example("ferraus presets show compress/tighten", "Print the tighten preset")
            .example("ferraus presets show compress/vocal --json", "The vocal compressor preset's values as JSON")
            .example("ferraus presets show compress/mine", "Show a user preset saved earlier with presets save"),
        OpSpec::custom("presets save", Category::Chains, "Save parameter values as a user preset", presets_save)
            .param(param("name", ParamKind::Str).required().help("effect/preset, e.g. compress/mine"))
            .param(param("set", ParamKind::Str).repeatable().help("key=value pairs to store"))
            .positional("name")
            .example("ferraus presets save compress/mine --set threshold=-16dB --set ratio=3:1", "Save a compressor preset")
            .example("ferraus presets save reverb/booth --set type=room --set mix=15%", "Save a small-room reverb as a user preset")
            .example("ferraus presets save compress/mine --set ratio=2:1 --overwrite", "Replace an existing user preset"),
        OpSpec::custom("schema", Category::Agents, "List commands, or print the JSON Schema of one command's arguments and result", schema)
            .param(param("command", ParamKind::Str).help("Command to describe (omit to list every command)"))
            .positional("command")
            .example("ferraus schema separate", "Arguments and result schema for separate")
            .example("ferraus schema", "List every command that has a schema")
            .example("ferraus schema \"denoise profile\" --json", "The schema of a multi-word command (quote it)"),
        OpSpec::custom("describe", Category::Agents, "Compact catalog of every command for agents", describe)
            .inputs(Inputs::NONE)
            .param(param("agent", ParamKind::Flag).help("One line per command sized to fit in a model's context"))
            .param(param("format", ParamKind::Choice).choices(&["text", "markdown", "json"]).default("text").help("Output format"))
            .example("ferraus describe --agent", "The whole surface in under 4,000 tokens")
            .example("ferraus describe --format markdown", "The catalog as a Markdown table, e.g. for AGENTS.md")
            .example("ferraus describe --format json", "The catalog as JSON for programmatic tool discovery"),
        OpSpec::custom("replay", Category::Agents, "Re-run the command recorded in a sidecar", replay)
            .inputs(Inputs::exactly(1, crate::op::InputKind::Any))
            .example("ferraus replay take.tuned.wav.ferraus.json --overwrite", "Reproduce an output from its sidecar")
            .example("ferraus replay take.tuned.wav.ferraus.json --dry-run --json", "Show the recorded command and parameters without running it")
            .example("ferraus replay take.tuned.wav.ferraus.json -o retake.wav", "Re-run the recorded command but write to a new file"),
        OpSpec::custom("verify", Category::Agents, "Check that a sidecar's inputs and outputs still match their hashes", verify)
            .inputs(Inputs::exactly(1, crate::op::InputKind::Any))
            .example("ferraus verify take.tuned.wav.ferraus.json --json", "Confirm nothing changed since the run")
            .example("ferraus verify master.wav.ferraus.json", "Check a master against its provenance; exit code 9 if a hash differs")
            .example("ferraus verify stems/vocals.wav.ferraus.json --quiet", "Silent check for scripts: only the exit code matters"),
    ]
}

fn read_pipeline(args: &Args, ctx: &OpCtx) -> Result<(crate::pipeline::PipelineFile, PathBuf)> {
    let path = ctx.input(0)?.to_path_buf();
    let text =
        std::fs::read_to_string(&path).map_err(|e| Error::input(format!("cannot read {}: {e}", path.display())))?;
    let p = crate::pipeline::parse(&text, &args.all("set"))?;
    let base = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    Ok((p, base))
}

fn pipeline_run(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let (p, base) = read_pipeline(args, ctx)?;
    let started = std::time::Instant::now();
    let report = crate::pipeline::run(&p, &base, ctx, !args.flag("no-cache"))?;
    let job = json!({
        "ferraus_version": crate::envelope::FERRAUS_VERSION,
        "pipeline": ctx.input(0)?.display().to_string(),
        "plan": report.stages,
        "outputs": report.outputs,
        "computed": report.computed,
        "cached": report.cached,
        "timing_ms": started.elapsed().as_millis() as u64,
    });
    if let Some(first) = report.outputs.first().and_then(|o| o["path"].as_str()) {
        let dir = Path::new(first).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let _ = std::fs::write(dir.join("job.ferraus.json"), serde_json::to_vec_pretty(&job)?);
    }
    Ok(job)
}

fn pipeline_validate(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let (p, _) = read_pipeline(args, ctx)?;
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let resolved = crate::pipeline::validate(&p, registry)?;
    let order: Vec<String> = crate::pipeline::topo_order(&p)?.into_iter().map(|i| p.stages[i].id.clone()).collect();
    Ok(json!({"valid": true, "order": order, "stages": resolved, "outputs": p.outputs}))
}

/// User presets live in `<config>/presets/<effect>/<name>.toml` as flat key = "value" tables.
pub fn load_user_preset(effect: &str, name: &str) -> Option<Vec<(String, String)>> {
    let path = crate::paths::presets_dir().join(effect.replace(' ', "-")).join(format!("{name}.toml"));
    let text = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    Some(table.into_iter().map(|(k, v)| (k, toml_scalar(&v))).collect())
}

fn toml_scalar(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn user_presets(effect: Option<&str>) -> Vec<Value> {
    let mut out = Vec::new();
    let root = crate::paths::presets_dir();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return out;
    };
    for d in dirs.flatten() {
        let eff = d.file_name().to_string_lossy().to_string();
        if effect.map(|e| e.replace(' ', "-") != eff).unwrap_or(false) {
            continue;
        }
        if let Ok(files) = std::fs::read_dir(d.path()) {
            for f in files.flatten() {
                let p = f.path();
                if p.extension().map(|e| e == "toml").unwrap_or(false) {
                    let name = p.file_stem().unwrap().to_string_lossy().to_string();
                    out.push(json!({"effect": eff, "name": name, "source": "user", "path": p.display().to_string()}));
                }
            }
        }
    }
    out
}

fn presets_list(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let effect = args.str("effect");
    let mut out = Vec::new();
    for op in registry.ops() {
        if effect.as_deref().map(|e| e != op.name).unwrap_or(false) {
            continue;
        }
        for p in &op.presets {
            out.push(json!({"effect": op.name, "name": p.name, "source": "built-in", "help": p.help}));
        }
    }
    out.extend(user_presets(effect.as_deref()));
    Ok(json!({ "presets": out }))
}

fn split_preset_name(s: &str) -> Result<(String, String)> {
    s.split_once('/')
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .ok_or_else(|| Error::usage("preset names are effect/name, e.g. compress/vocal"))
}

fn presets_show(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let (effect, name) = split_preset_name(&args.str_or("name", ""))?;
    let spec = registry.require(&effect)?;
    if let Some(p) = spec.presets.iter().find(|p| p.name == name) {
        let values: serde_json::Map<String, Value> =
            p.values.iter().map(|(k, v)| (k.replace('-', "_"), json!(v))).collect();
        return Ok(json!({"effect": spec.name, "name": name, "source": "built-in", "help": p.help, "values": values}));
    }
    if let Some(vals) = load_user_preset(spec.name, &name) {
        let values: serde_json::Map<String, Value> = vals.into_iter().map(|(k, v)| (k, json!(v))).collect();
        return Ok(json!({"effect": spec.name, "name": name, "source": "user", "values": values}));
    }
    Err(Error::usage(format!("no preset '{name}' for '{effect}'"))
        .with_hint(format!("ferraus presets list --effect {effect}")))
}

fn presets_save(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let (effect, name) = split_preset_name(&args.str_or("name", ""))?;
    let spec = registry.require(&effect)?;
    let mut table = toml::Table::new();
    let mut pairs = Vec::new();
    for s in args.all("set") {
        let (k, v) = s.split_once('=').ok_or_else(|| Error::usage(format!("--set expects key=value, got '{s}'")))?;
        pairs.push((k.to_string(), v.to_string()));
        table.insert(k.replace('-', "_"), toml::Value::String(v.to_string()));
    }
    // validate against the effect
    Args::build(spec, pairs, &|_, _| None)?;
    let dir = crate::paths::presets_dir().join(spec.name.replace(' ', "-"));
    let path = dir.join(format!("{name}.toml"));
    ctx.prepare_output(&path)?;
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string(&table).map_err(|e| Error::internal(e.to_string()))?;
    ctx.write_file(&path, text.as_bytes(), Some("preset"))?;
    Ok(json!({"saved": path.display().to_string()}))
}

fn schema(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    match args.str("command") {
        Some(c) => Ok(crate::schema::command_schema(registry.require(&c)?)),
        None => Ok(json!({
            "schema_version": crate::envelope::SCHEMA_VERSION,
            "commands": registry.ops().iter().map(|o| json!({"command": o.name, "summary": o.summary, "category": o.category})).collect::<Vec<_>>()
        })),
    }
}

/// One line per command: purpose, required inputs, and the three most useful flags.
pub fn agent_catalog(registry: &Registry) -> Vec<(String, String)> {
    let mut lines = Vec::new();
    let mut ops: Vec<&OpSpec> = registry.ops().iter().collect();
    ops.sort_by_key(|o| (o.category, o.name));
    for o in ops {
        let inputs = if let Some(pos) = o.positional {
            format!(" [{pos}]")
        } else {
            match (o.inputs.kind, o.inputs.min, o.inputs.max) {
                (crate::op::InputKind::None, _, _) => String::new(),
                (k, min, max) => {
                    let kind = match k {
                        crate::op::InputKind::Audio => "audio",
                        crate::op::InputKind::Midi => "midi",
                        crate::op::InputKind::Score => "score",
                        _ => "file",
                    };
                    match max {
                        // one audio file is the default and is not marked (see the header)
                        Some(1) if kind == "audio" => String::new(),
                        Some(1) => format!(" <{kind}>"),
                        Some(m) if m == min => format!(" <{kind}>x{m}"),
                        _ if min == 0 => format!(" [{kind}...]"),
                        _ => format!(" <{kind}...>"),
                    }
                }
            }
        };
        let flags: Vec<String> = o
            .params
            .iter()
            .filter(|p| !p.advanced && Some(p.name) != o.positional)
            .take(3)
            .map(|p| format!("--{}", p.name))
            .collect();
        let mut line = format!("{}{}: {}", o.name, inputs, o.summary);
        if !flags.is_empty() {
            line.push_str(&format!(" [{}]", flags.join(" ")));
        }
        lines.push((o.category.title().to_string(), line));
    }
    lines
}

fn describe(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let catalog = agent_catalog(registry);
    let fmt = args.str_or("format", "text");
    let header = "ferraus: audio editing CLI. Grammar: ferraus <command> [inputs] [flags] [-o out]. Commands take one audio file unless marked (<midi>, <file>, x2, ...). Add --json for a structured result, --dry-run to plan. Units: 1.5s 4b -6dB 2kHz C4 4:1 80% 1/16. Effects chain: ferraus fx in.wav -o out.wav --fx \"name:key=val\".";
    let text = match fmt.as_str() {
        "markdown" => {
            let mut s = format!("# ferraus agent guide\n\n{header}\n");
            let mut cur = String::new();
            for (cat, line) in &catalog {
                if *cat != cur {
                    s.push_str(&format!("\n## {cat}\n\n"));
                    cur = cat.clone();
                }
                s.push_str(&format!("- `{}`\n", line));
            }
            s
        }
        _ => {
            let mut s = format!("{header}\n");
            for (_, line) in &catalog {
                s.push_str(line);
                s.push('\n');
            }
            s
        }
    };
    let approx_tokens = text.len() / 4;
    ctx.metric("approx_tokens", approx_tokens);
    if fmt == "json" {
        return Ok(
            json!({"header": header, "commands": catalog.iter().map(|(c, l)| json!({"category": c, "line": l})).collect::<Vec<_>>()}),
        );
    }
    Ok(json!({ "text": text }))
}

fn replay(_args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let path = ctx.input(0)?.to_path_buf();
    let sc: Sidecar = serde_json::from_slice(&std::fs::read(&path)?)
        .map_err(|e| Error::usage(format!("not a ferraus sidecar: {e}")))?;
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let spec = registry.require(&sc.command)?;
    // Rebuild the invocation from resolved params (not argv) so defaults are pinned.
    let mut pairs = Vec::new();
    for (k, v) in &sc.params {
        if spec.find_param(k).is_none() && !(k == "mix" && spec.auto_mix) && k != "preset" {
            continue; // run-time notes such as detected values
        }
        match v {
            Value::Array(a) => pairs.extend(a.iter().map(|x| (k.clone(), x.as_str().unwrap_or("").to_string()))),
            Value::String(s) => pairs.push((k.clone(), s.clone())),
            other => pairs.push((k.clone(), other.to_string())),
        }
    }
    let mut globals = ctx.globals.clone();
    if sc.outputs.len() == 1 && sc.outputs[0].role.is_none() {
        globals.output = Some(sc.outputs[0].path.clone());
    } else if let Some(o) = sc.outputs.first() {
        globals.outdir = Path::new(&o.path).parent().map(|p| p.to_path_buf());
    }
    globals.force = true;
    let inv = Invocation {
        command: sc.command.clone(),
        args: pairs,
        inputs: sc.inputs.iter().map(|i| i.path.clone()).collect(),
        globals,
        argv: sc.argv.clone(),
        stdin: None,
    };
    let rt =
        Runtime { registry, io: ctx.io, progress: ctx.progress, presets: &|_, _| None, cancel: ctx.cancel.clone() };
    let out = rt.invoke(inv);
    let env = out.envelope;
    if let Some(e) = env.error.clone() {
        return Err(e);
    }
    let mut matches = Vec::new();
    for (o, rec) in env.outputs.iter().zip(&sc.outputs) {
        let h = sha256_file(Path::new(&o.path)).ok();
        matches.push(json!({"path": o.path, "identical": h.is_some() && h == rec.sha256}));
    }
    ctx.outputs.extend(env.outputs.clone());
    ctx.globals.sidecar = false;
    Ok(json!({"replayed": sc.command, "outputs": matches}))
}

fn verify(_args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let path = ctx.input(0)?.to_path_buf();
    let sc: Sidecar = serde_json::from_slice(&std::fs::read(&path)?)
        .map_err(|e| Error::usage(format!("not a ferraus sidecar: {e}")))?;
    let mut files = Vec::new();
    let mut ok = true;
    for (kind, list) in [("input", &sc.inputs), ("output", &sc.outputs)] {
        for f in list {
            let actual = sha256_file(Path::new(&f.path)).ok();
            let good = f.sha256.is_none() || actual.as_deref() == f.sha256.as_deref();
            ok &= good;
            files.push(json!({"kind": kind, "path": f.path, "expected": f.sha256, "actual": actual, "match": good}));
        }
    }
    if !ok {
        let e = Error::verification(format!("{} no longer matches its recorded hashes", path.display()));
        ctx.metric("files", &files);
        return Err(e);
    }
    Ok(json!({"verified": true, "files": files}))
}
