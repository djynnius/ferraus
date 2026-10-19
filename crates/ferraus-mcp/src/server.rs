//! MCP protocol handling, tool generation, sandboxing, jobs, resources, and prompts.

use ferraus_core::ctx::{absolutize, AudioIo, CancelToken, Globals, Progress};
use ferraus_core::envelope::Envelope;
use ferraus_core::exec::{Invocation, Runtime};
use ferraus_core::op::{json_to_pairs, OpSpec, ParamKind};
use ferraus_core::{Error, Registry, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
pub const TOOL_PREFIX: &str = "ferraus_";

/// Commands that manage the CLI process itself and are not exposed as tools. MCP clients
/// get the in-server job tools instead of the CLI's process-based `jobs` commands.
pub const CLI_ONLY: &[&str] = &[
    "mcp serve",
    "self-update",
    "self-uninstall",
    "completions",
    "man",
    "docs build-site",
    "jobs list",
    "jobs status",
    "jobs logs",
    "jobs wait",
    "jobs cancel",
    "jobs result",
];

/// Global options an agent may pass inside tool arguments.
const GLOBAL_ARGS: &[(&str, &str)] = &[
    ("overwrite", "Replace an existing output"),
    ("dry_run", "Resolve parameters and return the plan without processing"),
    ("force", "Recompute even if an identical output exists"),
    ("bpm", "Tempo for beat-based times"),
    ("sample_rate", "Output sample rate in Hz"),
    ("bit_depth", "Output bit depth (16, 24, 32 = float)"),
    ("channels", "Output channel count"),
    ("bitrate", "Lossy bitrate, e.g. 192k"),
    ("format", "Output format when the extension is ambiguous"),
    ("outdir", "Output directory (inside an allowed directory)"),
    ("seed", "Seed for stochastic steps"),
    ("deterministic", "Bit-exact CPU results"),
];

#[derive(Clone)]
pub struct ServerConfig {
    pub allow: Vec<PathBuf>,
    pub base_globals: Globals,
}

#[derive(Clone)]
struct Job {
    state: String,
    percent: f64,
    result: Option<Value>,
    cancel: CancelToken,
    tool: String,
}

pub struct Server<'a> {
    registry: &'a Registry,
    io: &'a dyn AudioIo,
    cfg: ServerConfig,
    jobs: Mutex<HashMap<String, Job>>,
    cancels: Mutex<HashMap<String, CancelToken>>,
    next_job: Mutex<u64>,
}

type Writer<'w> = Arc<Mutex<Box<dyn Write + Send + 'w>>>;

/// Sends `notifications/progress` for a request that carried a progress token.
struct McpProgress<'w> {
    out: Option<Writer<'w>>,
    token: Option<Value>,
    job: Option<(&'w Mutex<HashMap<String, Job>>, String)>,
}

impl Progress for McpProgress<'_> {
    fn progress(&self, stage: &str, fraction: f64) {
        if let (Some(out), Some(token)) = (&self.out, &self.token) {
            let msg = json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {
                "progressToken": token, "progress": (fraction * 100.0).round(), "total": 100, "message": stage}});
            if let Ok(mut w) = out.lock() {
                let _ = writeln!(w, "{msg}");
                let _ = w.flush();
            }
        }
        if let Some((jobs, id)) = &self.job {
            if let Some(j) = jobs.lock().unwrap().get_mut(id) {
                j.percent = (fraction * 100.0).round();
            }
        }
    }
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn strip_meta(mut v: Value) -> Value {
    if let Value::Object(o) = &mut v {
        o.remove("$schema");
        o.remove("$id");
    }
    v
}

impl<'a> Server<'a> {
    pub fn new(registry: &'a Registry, io: &'a dyn AudioIo, cfg: ServerConfig) -> Server<'a> {
        Server {
            registry,
            io,
            cfg,
            jobs: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            next_job: Mutex::new(1),
        }
    }

    /// Ops exposed as tools.
    pub fn tool_ops(&self) -> Vec<&OpSpec> {
        self.registry.ops().iter().filter(|o| !CLI_ONLY.contains(&o.name)).collect()
    }

    pub fn tool_name(op: &OpSpec) -> String {
        format!("{TOOL_PREFIX}{}", op.id())
    }

    fn op_for_tool(&self, name: &str) -> Option<&'a OpSpec> {
        let id = name.strip_prefix(TOOL_PREFIX)?;
        self.registry.ops().iter().find(|o| o.id() == id && !CLI_ONLY.contains(&o.name))
    }

    /// The MCP tool definition for an op.
    pub fn tool_def(op: &OpSpec) -> Value {
        let mut schema = strip_meta(ferraus_core::schema::args_schema(op));
        if let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) {
            for (k, help) in GLOBAL_ARGS {
                if !props.contains_key(*k) {
                    let ty = match *k {
                        "overwrite" | "dry_run" | "force" | "deterministic" => {
                            json!({"type": "boolean", "description": help})
                        }
                        _ => json!({"type": ["string", "number"], "description": help}),
                    };
                    props.insert(k.to_string(), ty);
                }
            }
        }
        let mut desc = op.summary.to_string();
        if !op.about.is_empty() {
            desc.push_str("\n\n");
            desc.push_str(op.about);
        }
        if op.best_effort {
            desc.push_str("\n\nQuality tier: best-effort; check the reported quality/confidence.");
        }
        if let Some(e) = op.examples.first() {
            desc.push_str(&format!("\n\nCLI equivalent: {}", e.cmd));
        }
        let read_only = matches!(op.category, ferraus_core::Category::Inspect | ferraus_core::Category::Docs)
            || op.name.starts_with("schema")
            || op.name.starts_with("describe")
            || op.name.ends_with(" list")
            || op.name.ends_with(" info")
            || op.name.ends_with(" validate")
            || op.name.ends_with(" explain");
        let network = op.name.starts_with("models pull")
            || op.name.starts_with("sounds install")
            || op.name.starts_with("accelerate")
            || op.name.starts_with("doctor");
        json!({
            "name": Self::tool_name(op),
            "title": format!("ferraus {}", op.name),
            "description": desc,
            "inputSchema": schema,
            "outputSchema": strip_meta(ferraus_core::schema::result_schema(op)),
            "annotations": {
                "readOnlyHint": read_only,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": network,
            }
        })
    }

    fn job_tools() -> Vec<Value> {
        vec![
            json!({"name": "ferraus_job_start", "title": "Start a background job", "description": "Run any ferraus tool in the background and return a job_id immediately. Use for long operations such as separate, master, or transcribe.",
                "inputSchema": {"type": "object", "properties": {"tool": {"type": "string", "description": "Tool name, e.g. ferraus_separate"}, "arguments": {"type": "object", "description": "That tool's arguments"}}, "required": ["tool"]},
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}}),
            json!({"name": "ferraus_job_status", "title": "Job status", "description": "State (running, succeeded, failed, cancelled) and percent complete of a background job.",
                "inputSchema": {"type": "object", "properties": {"job_id": {"type": "string"}}, "required": ["job_id"]},
                "annotations": {"readOnlyHint": true}}),
            json!({"name": "ferraus_job_result", "title": "Job result", "description": "The result envelope of a finished background job.",
                "inputSchema": {"type": "object", "properties": {"job_id": {"type": "string"}}, "required": ["job_id"]},
                "annotations": {"readOnlyHint": true}}),
            json!({"name": "ferraus_job_cancel", "title": "Cancel a job", "description": "Cancel a running background job.",
                "inputSchema": {"type": "object", "properties": {"job_id": {"type": "string"}}, "required": ["job_id"]},
                "annotations": {"readOnlyHint": false, "destructiveHint": false}}),
        ]
    }

    pub fn list_tools(&self) -> Vec<Value> {
        let mut tools: Vec<Value> = self.tool_ops().into_iter().map(Self::tool_def).collect();
        tools.extend(Self::job_tools());
        tools
    }

    // ---------------------------------------------------------------------------
    // Sandbox
    // ---------------------------------------------------------------------------

    fn base_dir(&self) -> &Path {
        &self.cfg.allow[0]
    }

    fn resolve(&self, p: &str) -> PathBuf {
        let pb = PathBuf::from(p);
        let joined = if p.starts_with('~') || pb.is_absolute() { pb } else { self.base_dir().join(pb) };
        absolutize(&joined)
    }

    fn check(&self, p: &Path) -> Result<()> {
        if self.cfg.allow.iter().any(|a| p.starts_with(a)) {
            Ok(())
        } else {
            Err(Error::new(
                ferraus_core::ErrorKind::Usage,
                "path_not_allowed",
                format!("{} is outside the allowed directories", p.display()),
            )
            .with_hint(format!(
                "allowed: {}",
                self.cfg.allow.iter().map(|a| a.display().to_string()).collect::<Vec<_>>().join(", ")
            )))
        }
    }

    /// Turn tool arguments into an invocation, enforcing the sandbox.
    pub fn build_invocation(&self, op: &OpSpec, args: &Value) -> Result<Invocation> {
        let obj = match args {
            Value::Object(o) => o.clone(),
            Value::Null => Map::new(),
            _ => return Err(Error::usage("arguments must be an object")),
        };
        let mut g = self.cfg.base_globals.clone();
        g.allow = self.cfg.allow.clone();
        g.sidecar = true;
        let mut inputs = Vec::new();
        let mut rest = Map::new();
        for (k, v) in obj {
            let has_param = op.find_param(&k).is_some();
            match k.as_str() {
                "inputs" | "input" => {
                    let list: Vec<String> = match v {
                        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
                        Value::String(s) => vec![s],
                        _ => return Err(Error::usage("inputs must be a list of paths")),
                    };
                    for i in list {
                        if i == "-" {
                            return Err(Error::usage("stdin is not available over MCP; pass a file path"));
                        }
                        let p = self.resolve(&i);
                        self.check(&p)?;
                        inputs.push(p.display().to_string());
                    }
                }
                "output" => {
                    let s = v.as_str().ok_or_else(|| Error::usage("output must be a string"))?;
                    if s == "-" {
                        return Err(Error::usage("stdout output is not available over MCP; pass a file path"));
                    }
                    let p = self.resolve(s);
                    self.check(&p)?;
                    g.output = Some(p.display().to_string());
                }
                "outdir" if !has_param => {
                    let p = self.resolve(v.as_str().unwrap_or(""));
                    self.check(&p)?;
                    g.outdir = Some(p);
                }
                "overwrite" if !has_param => g.overwrite = v.as_bool().unwrap_or(false),
                "dry_run" if !has_param => g.dry_run = v.as_bool().unwrap_or(false),
                "force" if !has_param => g.force = v.as_bool().unwrap_or(false),
                "deterministic" if !has_param => g.deterministic = v.as_bool().unwrap_or(false),
                "bpm" if !has_param => g.bpm = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())),
                "sample_rate" if !has_param => {
                    g.sample_rate = v.as_u64().map(|x| x as u32).or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                }
                "bit_depth" if !has_param => {
                    g.bit_depth = v.as_u64().map(|x| x as u16).or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                }
                "channels" if !has_param => {
                    g.channels = v.as_u64().map(|x| x as usize).or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                }
                "bitrate" if !has_param => {
                    g.bitrate = v
                        .as_str()
                        .and_then(|s| ferraus_core::units::parse_bitrate(s).ok())
                        .or(v.as_u64().map(|x| x as u32))
                }
                "format" if !has_param => g.format = v.as_str().map(|s| s.to_string()),
                "seed" if !has_param => g.seed = v.as_u64().unwrap_or(0),
                _ => {
                    // path-valued params must stay inside the sandbox too
                    if let Some(p) = op.find_param(&k) {
                        if p.kind == ParamKind::Path {
                            if let Some(s) = v.as_str() {
                                let resolved = self.resolve(s);
                                self.check(&resolved)?;
                                rest.insert(k, Value::String(resolved.display().to_string()));
                                continue;
                            }
                        }
                    }
                    rest.insert(k, v);
                }
            }
        }
        if g.output.is_none() && g.outdir.is_none() && op.inputs.max != Some(0) {
            g.outdir = Some(self.base_dir().join("ferraus-out"));
        }
        let pairs = json_to_pairs(&Value::Object(rest))?;
        Ok(Invocation {
            command: op.name.to_string(),
            args: pairs,
            inputs,
            globals: g,
            argv: vec!["(mcp)".into(), op.name.to_string()],
            stdin: None,
        })
    }

    fn run_invocation(&self, inv: Invocation, progress: &dyn Progress, cancel: CancelToken) -> Envelope {
        let presets = |e: &str, n: &str| ferraus_core::builtin::load_user_preset(e, n);
        let rt = Runtime { registry: self.registry, io: self.io, progress, presets: &presets, cancel };
        rt.invoke(inv).envelope
    }

    fn tool_result(env: &Envelope) -> Value {
        let v = serde_json::to_value(env).unwrap_or(Value::Null);
        json!({
            "content": [{"type": "text", "text": serde_json::to_string(&v).unwrap_or_default()}],
            "structuredContent": v,
            "isError": !env.ok,
        })
    }

    fn call_tool(
        &self,
        name: &str,
        args: &Value,
        progress: &dyn Progress,
        cancel: CancelToken,
        scope_jobs: Option<&dyn Fn(String, Value)>,
    ) -> Result<Value> {
        match name {
            "ferraus_job_start" => {
                let tool = args
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| Error::usage("job_start needs 'tool'"))?
                    .to_string();
                if self.op_for_tool(&tool).is_none() {
                    return Err(Error::usage(format!("unknown tool '{tool}'")));
                }
                let id = {
                    let mut n = self.next_job.lock().unwrap();
                    *n += 1;
                    format!("job-{}", *n - 1)
                };
                let spawn = scope_jobs.ok_or_else(|| Error::internal("jobs unavailable"))?;
                self.jobs.lock().unwrap().insert(
                    id.clone(),
                    Job {
                        state: "running".into(),
                        percent: 0.0,
                        result: None,
                        cancel: CancelToken::default(),
                        tool: tool.clone(),
                    },
                );
                spawn(
                    id.clone(),
                    json!({"tool": tool, "arguments": args.get("arguments").cloned().unwrap_or(json!({}))}),
                );
                let v = json!({"job_id": id, "state": "running"});
                Ok(
                    json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v, "isError": false}),
                )
            }
            "ferraus_job_status" | "ferraus_job_result" | "ferraus_job_cancel" => {
                let id = args.get("job_id").and_then(|t| t.as_str()).ok_or_else(|| Error::usage("needs 'job_id'"))?;
                let mut jobs = self.jobs.lock().unwrap();
                let job = jobs.get_mut(id).ok_or_else(|| Error::usage(format!("no job '{id}'")))?;
                let v = match name {
                    "ferraus_job_status" => {
                        json!({"job_id": id, "tool": job.tool, "state": job.state, "percent": job.percent})
                    }
                    "ferraus_job_cancel" => {
                        job.cancel.cancel();
                        if job.state == "running" {
                            job.state = "cancelling".into();
                        }
                        json!({"job_id": id, "state": job.state})
                    }
                    _ => match &job.result {
                        Some(r) => r.clone(),
                        None => return Err(Error::usage(format!("job {id} is still {}", job.state))),
                    },
                };
                let is_err = name == "ferraus_job_result" && v.get("ok") == Some(&json!(false));
                Ok(
                    json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v, "isError": is_err}),
                )
            }
            _ => {
                let op = self.op_for_tool(name).ok_or_else(|| Error::usage(format!("unknown tool '{name}'")))?;
                let env = match self.build_invocation(op, args) {
                    Ok(inv) => self.run_invocation(inv, progress, cancel),
                    Err(e) => Envelope::error(op.name, e, 0),
                };
                Ok(Self::tool_result(&env))
            }
        }
    }

    fn run_job(&self, id: &str, spec: &Value) {
        let tool = spec["tool"].as_str().unwrap_or("");
        let cancel = self.jobs.lock().unwrap().get(id).map(|j| j.cancel.clone()).unwrap_or_default();
        let progress = McpProgress { out: None, token: None, job: Some((&self.jobs, id.to_string())) };
        let result = match self.op_for_tool(tool) {
            Some(op) => {
                let env = match self.build_invocation(op, &spec["arguments"]) {
                    Ok(inv) => self.run_invocation(inv, &progress, cancel.clone()),
                    Err(e) => Envelope::error(op.name, e, 0),
                };
                serde_json::to_value(&env).unwrap_or(Value::Null)
            }
            None => json!({"ok": false, "error": {"message": "unknown tool"}}),
        };
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(j) = jobs.get_mut(id) {
            j.state = if cancel.is_cancelled() {
                "cancelled".into()
            } else if result.get("ok") == Some(&json!(true)) {
                "succeeded".into()
            } else {
                "failed".into()
            };
            j.percent = 100.0;
            j.result = Some(result);
        }
    }

    // ---------------------------------------------------------------------------
    // Resources and prompts
    // ---------------------------------------------------------------------------

    fn list_resources(&self) -> Value {
        let mut r = vec![
            json!({"uri": "ferraus://catalog", "name": "catalog", "title": "Command catalog", "description": "One line per command (describe --agent)", "mimeType": "text/plain"}),
            json!({"uri": "ferraus://presets", "name": "presets", "title": "Presets", "description": "Built-in and user presets", "mimeType": "application/json"}),
        ];
        for (uri, op, title) in [
            ("ferraus://models", "models list", "Model registry"),
            ("ferraus://plugins", "plugin list", "Scanned plugins"),
            ("ferraus://sounds", "sounds list", "Installed sounds"),
        ] {
            if self.registry.get(op).is_some() {
                r.push(json!({"uri": uri, "name": uri.trim_start_matches("ferraus://"), "title": title, "mimeType": "application/json"}));
            }
        }
        json!({ "resources": r })
    }

    fn list_resource_templates(&self) -> Value {
        json!({"resourceTemplates": [
            {"uriTemplate": "ferraus://presets/{effect}", "name": "effect-presets", "title": "Presets for one effect", "mimeType": "application/json"},
            {"uriTemplate": "ferraus://jobs/{id}", "name": "job", "title": "Background job state and result", "mimeType": "application/json"},
            {"uriTemplate": "ferraus://docs/{command}", "name": "docs", "title": "Reference page for a command", "mimeType": "text/markdown"},
            {"uriTemplate": "ferraus://schema/{command}", "name": "schema", "title": "JSON Schema for a command", "mimeType": "application/json"},
        ]})
    }

    fn run_quiet(&self, op: &str, pairs: Vec<(String, String)>) -> Result<Value> {
        let mut g = self.cfg.base_globals.clone();
        g.sidecar = false;
        let inv = Invocation { command: op.into(), args: pairs, inputs: vec![], globals: g, argv: vec![], stdin: None };
        let env = self.run_invocation(inv, &ferraus_core::ctx::NoProgress, CancelToken::default());
        match env.error {
            Some(e) => Err(e),
            None => Ok(env.result),
        }
    }

    fn read_resource(&self, uri: &str) -> Result<Value> {
        let (mime, text) = if uri == "ferraus://catalog" {
            let v = self.run_quiet("describe", vec![("agent".into(), "true".into())])?;
            ("text/plain", v["text"].as_str().unwrap_or("").to_string())
        } else if uri == "ferraus://presets" {
            ("application/json", self.run_quiet("presets list", vec![])?.to_string())
        } else if let Some(effect) = uri.strip_prefix("ferraus://presets/") {
            ("application/json", self.run_quiet("presets list", vec![("effect".into(), effect.into())])?.to_string())
        } else if uri == "ferraus://models" {
            ("application/json", self.run_quiet("models list", vec![])?.to_string())
        } else if uri == "ferraus://plugins" {
            ("application/json", self.run_quiet("plugin list", vec![])?.to_string())
        } else if uri == "ferraus://sounds" {
            ("application/json", self.run_quiet("sounds list", vec![])?.to_string())
        } else if let Some(id) = uri.strip_prefix("ferraus://jobs/") {
            let jobs = self.jobs.lock().unwrap();
            let j = jobs.get(id).ok_or_else(|| Error::usage(format!("no job '{id}'")))?;
            (
                "application/json",
                json!({"job_id": id, "tool": j.tool, "state": j.state, "percent": j.percent, "result": j.result})
                    .to_string(),
            )
        } else if let Some(cmd) = uri.strip_prefix("ferraus://docs/") {
            let cmd = cmd.replace("%20", " ").replace('_', " ");
            let v = self.run_quiet("docs", vec![("command".into(), cmd)])?;
            ("text/markdown", v["text"].as_str().unwrap_or("").to_string())
        } else if let Some(cmd) = uri.strip_prefix("ferraus://schema/") {
            let cmd = cmd.replace("%20", " ").replace('_', " ");
            ("application/json", self.run_quiet("schema", vec![("command".into(), cmd)])?.to_string())
        } else {
            return Err(Error::usage(format!("unknown resource '{uri}'")));
        };
        Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]}))
    }

    fn prompts() -> Vec<(&'static str, &'static str, Vec<(&'static str, &'static str, bool)>, &'static str)> {
        vec![
            ("clean-and-tune-vocal", "Clean a vocal take, tune it to its key, and add a vocal chain",
                vec![("vocal", "Path to the vocal recording", true), ("key", "Key to tune to (default: detect)", false)],
                "Process the vocal at {vocal}:\n1. ferraus_analyze_problems on it and read the suggested fixes.\n2. ferraus_denoise with auto=true (add reduction=12dB) to remove steady background noise.\n3. ferraus_tune with key={key} (auto detects it), strength=85%, retune_speed=30ms; read params.key in the result.\n4. ferraus_fx with chain stages compress:preset=vocal, deess, reverb:type=plate,mix=18%.\n5. ferraus_compare the original and the result to confirm nothing was damaged.\nReport the detected key, the noise region chosen, and every output path."),
            ("extract-stems-for-remix", "Split a song into stems for remixing",
                vec![("song", "Path to the mixed song", true), ("stems", "2, 4, or 6 (default 4)", false)],
                "Split {song} into {stems} stems:\n1. ferraus_info to check duration and format.\n2. ferraus_separate with stems={stems} (the default tier picks the best verified models; use ferraus_job_start for long songs and poll ferraus_job_status).\n3. For each stem, ferraus_analyze_loudness; report the per-stem quality estimate from metrics.\n4. Optionally ferraus_analyze_tempo and ferraus_analyze_key on the song so the remix can match.\nReturn the stem paths with their roles."),
            ("hum-to-instrument", "Turn a hummed melody into an instrument part",
                vec![("recording", "Path to the humming recording", true), ("instrument", "Instrument name, e.g. piano or strings (default piano)", false)],
                "Turn the humming at {recording} into a {instrument} part:\n1. ferraus_denoise with auto=true if the recording is noisy (check ferraus_analyze_problems first).\n2. ferraus_transcribe to MIDI (mode=mono), then ferraus_midi_quantize with grid=1/16 strength=80%.\n3. ferraus_render the MIDI with instrument={instrument}.\nOr do all of it with ferraus_hum to={instrument}. Report the MIDI path, detected tempo and key, and the audio path."),
            ("master-song", "Master a mix to a delivery target with an analyze, plan, verify loop",
                vec![("mix", "Path to the final mix", true), ("target", "streaming, apple, youtube, club, cd, broadcast, podcast (default streaming)", false), ("reference", "Optional reference track", false)],
                "Master {mix} for {target}:\n1. ferraus_master_analyze (reference={reference} if given); read the loudness, tonal balance, stereo, and problem list.\n2. ferraus_master_plan with target={target}; inspect the chain and edit values if the analysis suggests it.\n3. ferraus_master with plan=<edited plan file> and report=<json path>.\n4. ferraus_master_verify against {target}. If it fails, adjust the plan and repeat from step 3.\n5. ferraus_compare the mix and the master to confirm nothing was damaged.\nStop when verify passes and the deviations are within 1 dB per band; report the final loudness and true peak."),
        ]
    }

    fn list_prompts(&self) -> Value {
        let prompts: Vec<Value> = Self::prompts()
            .into_iter()
            .map(|(name, desc, args, _)| json!({"name": name, "title": name.replace('-', " "), "description": desc,
                "arguments": args.iter().map(|(n, d, r)| json!({"name": n, "description": d, "required": r})).collect::<Vec<_>>()}))
            .collect();
        json!({ "prompts": prompts })
    }

    fn get_prompt(&self, name: &str, args: &Value) -> Result<Value> {
        let (_, desc, params, template) = Self::prompts()
            .into_iter()
            .find(|p| p.0 == name)
            .ok_or_else(|| Error::usage(format!("unknown prompt '{name}'")))?;
        let mut text = template.to_string();
        for (p, _, required) in params {
            let v = args.get(p).and_then(|v| v.as_str()).map(|s| s.to_string());
            if required && v.is_none() {
                return Err(Error::usage(format!("prompt '{name}' needs argument '{p}'")));
            }
            let default = match p {
                "key" => "auto",
                "stems" => "4",
                "instrument" => "piano",
                "target" => "streaming",
                "reference" => "(none)",
                _ => "",
            };
            text = text.replace(&format!("{{{p}}}"), &v.unwrap_or_else(|| default.to_string()));
        }
        Ok(json!({"description": desc, "messages": [{"role": "user", "content": {"type": "text", "text": text}}]}))
    }

    // ---------------------------------------------------------------------------
    // Dispatch
    // ---------------------------------------------------------------------------

    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL_VERSIONS[0]);
        let version = if PROTOCOL_VERSIONS.contains(&requested) { requested } else { PROTOCOL_VERSIONS[0] };
        json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": {"listChanged": false},
                "resources": {"subscribe": false, "listChanged": false},
                "prompts": {"listChanged": false},
                "logging": {},
            },
            "serverInfo": {"name": "ferraus", "title": "ferraus audio toolkit", "version": ferraus_core::FERRAUS_VERSION},
            "instructions": format!(
                "ferraus edits, repairs, separates, tunes, transcribes, and masters audio. Every tool returns the ferraus JSON envelope (ok, outputs, params, metrics, warnings, error with a runnable hint). Paths are confined to: {}. Outputs default to ferraus-out/ under the first allowed directory. For long jobs use ferraus_job_start. Read ferraus://catalog for a one-line-per-tool overview.",
                self.cfg.allow.iter().map(|a| a.display().to_string()).collect::<Vec<_>>().join(", ")
            ),
        })
    }

    /// Handle one JSON-RPC message. Returns a response for requests, None for notifications.
    /// Tool calls run on the calling thread.
    pub fn handle(
        &self,
        msg: &Value,
        out: Option<Writer<'_>>,
        jobs_spawn: Option<&dyn Fn(String, Value)>,
    ) -> Option<Value> {
        let method = msg.get("method").and_then(|m| m.as_str());
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(method) = method else {
            // a response from the client (we never send requests) — ignore
            return id.map(|i| rpc_error(&i, -32600, "invalid request: missing method"));
        };
        let Some(id) = id else {
            if method == "notifications/cancelled" {
                if let Some(rid) = params.get("requestId") {
                    if let Some(c) = self.cancels.lock().unwrap().get(&rid.to_string()) {
                        c.cancel();
                    }
                }
            }
            return None;
        };
        let result: std::result::Result<Value, (i64, String)> = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "logging/setLevel" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.list_tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let token = params.get("_meta").and_then(|m| m.get("progressToken")).cloned();
                let cancel = CancelToken::default();
                self.cancels.lock().unwrap().insert(id.to_string(), cancel.clone());
                let progress = McpProgress { out: out.clone(), token, job: None };
                let r = self.call_tool(name, &args, &progress, cancel, jobs_spawn);
                self.cancels.lock().unwrap().remove(&id.to_string());
                match r {
                    Ok(v) => Ok(v),
                    Err(e) if e.message.starts_with("unknown tool") => Err((-32602, e.message)),
                    Err(e) => {
                        let env = Envelope::error(name.trim_start_matches(TOOL_PREFIX), e, 0);
                        Ok(Self::tool_result(&env))
                    }
                }
            }
            "resources/list" => Ok(self.list_resources()),
            "resources/templates/list" => Ok(self.list_resource_templates()),
            "resources/read" => {
                let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                self.read_resource(uri).map_err(|e| (-32002, e.message))
            }
            "prompts/list" => Ok(self.list_prompts()),
            "prompts/get" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                self.get_prompt(name, params.get("arguments").unwrap_or(&json!({}))).map_err(|e| (-32602, e.message))
            }
            "completion/complete" => Ok(json!({"completion": {"values": [], "hasMore": false}})),
            other => Err((-32601, format!("method not found: {other}"))),
        };
        Some(match result {
            Ok(v) => rpc_ok(&id, v),
            Err((code, msg)) => rpc_error(&id, code, &msg),
        })
    }

    /// Serve newline-delimited JSON-RPC on stdin/stdout. Tool calls and jobs run on their
    /// own threads so long operations don't block pings, cancellations, or other calls.
    pub fn serve_stdio(&self) -> Result<()> {
        let out: Writer<'static> = Arc::new(Mutex::new(Box::new(std::io::stdout())));
        self.serve_lines(std::io::stdin().lock(), out)
    }

    pub fn serve_lines<R: BufRead>(&self, input: R, out: Writer<'_>) -> Result<()> {
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel::<(String, Value)>();
            // job runner: spawns one scoped thread per background job
            scope.spawn(move || {
                for (id, spec) in rx {
                    scope.spawn(move || self.run_job(&id, &spec));
                }
            });
            for line in input.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let parsed: std::result::Result<Value, _> = serde_json::from_str(&line);
                let msg = match parsed {
                    Ok(v) => v,
                    Err(e) => {
                        write_line(&out, &rpc_error(&Value::Null, -32700, &format!("parse error: {e}")));
                        continue;
                    }
                };
                let batch: Vec<Value> = match msg {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                for m in batch {
                    let is_call = m.get("method").and_then(|x| x.as_str()) == Some("tools/call");
                    let tx2 = tx.clone();
                    if is_call {
                        let out2 = out.clone();
                        scope.spawn(move || {
                            let spawn = |id: String, spec: Value| {
                                let _ = tx2.send((id, spec));
                            };
                            if let Some(resp) = self.handle(&m, Some(out2.clone()), Some(&spawn)) {
                                write_line(&out2, &resp);
                            }
                        });
                    } else {
                        let spawn = |id: String, spec: Value| {
                            let _ = tx2.send((id, spec));
                        };
                        if let Some(resp) = self.handle(&m, Some(out.clone()), Some(&spawn)) {
                            write_line(&out, &resp);
                        }
                    }
                }
            }
            drop(tx);
        });
        Ok(())
    }

    /// Serve the streamable-HTTP transport on `addr` (POST /mcp with JSON responses).
    pub fn serve_http(&self, addr: &str) -> Result<()> {
        let server =
            tiny_http::Server::http(addr).map_err(|e| Error::processing(format!("cannot listen on {addr}: {e}")))?;
        let loopback = addr.starts_with("127.") || addr.starts_with("localhost") || addr.starts_with("[::1]");
        if !loopback {
            eprintln!("warning: serving MCP on a non-loopback address ({addr}); anyone who can reach it can run ferraus within the allowed directories");
        }
        eprintln!("ferraus MCP server listening on http://{addr}/mcp");
        let session = format!("ferraus-{}", std::process::id());
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel::<(String, Value)>();
            scope.spawn(move || {
                for (id, spec) in rx {
                    scope.spawn(move || self.run_job(&id, &spec));
                }
            });
            for mut req in server.incoming_requests() {
                let session = session.clone();
                let tx2 = tx.clone();
                scope.spawn(move || {
                    let spawn = |id: String, spec: Value| {
                        let _ = tx2.send((id, spec));
                    };
                    let sj: &dyn Fn(String, Value) = &spawn;
                    let origin =
                        req.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str().to_string());
                    if let Some(origin) = origin {
                        let ok = origin.starts_with("http://localhost")
                            || origin.starts_with("http://127.0.0.1")
                            || origin.starts_with("https://localhost");
                        if !ok {
                            let _ =
                                req.respond(tiny_http::Response::from_string("forbidden origin").with_status_code(403));
                            return;
                        }
                    }
                    let path = req.url().split('?').next().unwrap_or("").to_string();
                    if path != "/mcp" {
                        let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
                        return;
                    }
                    match req.method() {
                        tiny_http::Method::Post => {}
                        tiny_http::Method::Delete => {
                            let _ = req.respond(tiny_http::Response::empty(200));
                            return;
                        }
                        _ => {
                            let _ = req.respond(tiny_http::Response::from_string("use POST").with_status_code(405));
                            return;
                        }
                    }
                    let mut body = String::new();
                    if req.as_reader().read_to_string(&mut body).is_err() {
                        let _ = req.respond(tiny_http::Response::from_string("bad body").with_status_code(400));
                        return;
                    }
                    let parsed: std::result::Result<Value, _> = serde_json::from_str(&body);
                    let responses: Vec<Value> = match parsed {
                        Err(e) => vec![rpc_error(&Value::Null, -32700, &format!("parse error: {e}"))],
                        Ok(Value::Array(batch)) => {
                            batch.iter().filter_map(|m| self.handle(m, None, Some(sj))).collect()
                        }
                        Ok(m) => self.handle(&m, None, Some(sj)).into_iter().collect(),
                    };
                    let json_header = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
                    let sess_header = tiny_http::Header::from_bytes("Mcp-Session-Id", session.as_bytes()).unwrap();
                    let resp = if responses.is_empty() {
                        tiny_http::Response::from_string("").with_status_code(202)
                    } else {
                        let text = if responses.len() == 1 {
                            responses[0].to_string()
                        } else {
                            Value::Array(responses).to_string()
                        };
                        tiny_http::Response::from_string(text).with_header(json_header)
                    };
                    let _ = req.respond(resp.with_header(sess_header));
                });
            }
        });
        Ok(())
    }
}

fn write_line(out: &Writer<'_>, v: &Value) {
    if let Ok(mut w) = out.lock() {
        let _ = writeln!(w, "{v}");
        let _ = w.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::op::{param, Category, OpSpec};
    use ferraus_core::testing;

    fn gain(
        a: &ferraus_core::Args,
        mut b: ferraus_core::AudioBuffer,
        _: &mut ferraus_core::OpCtx,
    ) -> Result<ferraus_core::AudioBuffer> {
        b.scale(a.gain("by")?.linear() as f32);
        Ok(b)
    }

    fn registry() -> &'static Registry {
        let mut ops = ferraus_core::builtin::ops();
        ops.push(
            OpSpec::transform("gain", Category::Edit, "Apply gain", gain)
                .param(param("by", ParamKind::Gain).default("0dB").help("gain"))
                .example("ferraus gain a.wav --by -6dB", "x"),
        );
        ops.extend(crate::ops());
        testing::leak_registry(ops)
    }

    fn server(dir: &Path) -> Server<'static> {
        let cfg = ServerConfig { allow: vec![absolutize(dir)], base_globals: Globals::default() };
        Server::new(registry(), &testing::SIMPLE_IO, cfg)
    }

    #[test]
    fn initialize_and_list() {
        let d = testing::TempDir::new("mcp-init");
        let s = server(&d.0);
        let r = s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}), None, None).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        let r = s.handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), None, None).unwrap();
        let names: Vec<&str> =
            r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ferraus_gain"));
        assert!(names.contains(&"ferraus_pipeline_run"));
        assert!(!names.contains(&"ferraus_mcp_serve"));
        assert!(names.contains(&"ferraus_job_start"));
        // unknown version falls back to latest
        let r = s
            .handle(
                &json!({"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
                None,
                None,
            )
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn one_to_one_mapping() {
        let d = testing::TempDir::new("mcp-map");
        let s = server(&d.0);
        let tools = s.list_tools();
        for op in s.registry.ops() {
            let present = tools.iter().any(|t| t["name"] == Server::tool_name(op));
            assert_eq!(present, !CLI_ONLY.contains(&op.name), "{}", op.name);
        }
    }

    #[test]
    fn call_tool_in_sandbox() {
        let d = testing::TempDir::new("mcp-call");
        testing::write_wav(&d.path("in.wav"), &testing::sine(8000, 440.0, 0.1, 0.5));
        let s = server(&d.0);
        let r = s.handle(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ferraus_gain","arguments":{"inputs":["in.wav"],"by":"-6dB"}}}), None, None).unwrap();
        let env = &r["result"]["structuredContent"];
        assert_eq!(env["ok"], true, "{r}");
        let out = env["outputs"][0]["path"].as_str().unwrap();
        assert!(out.contains("ferraus-out"), "{out}");
        assert!(Path::new(out).exists());
        // escaping the sandbox is refused
        let r = s.handle(&json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"ferraus_gain","arguments":{"inputs":["/etc/hosts"]}}}), None, None).unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["result"]["structuredContent"]["error"]["code"], "path_not_allowed");
        let r = s.handle(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"ferraus_gain","arguments":{"inputs":["in.wav"],"output":"../escape.wav"}}}), None, None).unwrap();
        assert_eq!(r["result"]["isError"], true);
    }

    #[test]
    fn stdio_loop_with_jobs_resources_prompts() {
        let d = testing::TempDir::new("mcp-stdio");
        testing::write_wav(&d.path("in.wav"), &testing::sine(8000, 440.0, 0.1, 0.5));
        let s = server(&d.0);
        let input = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ferraus_job_start","arguments":{"tool":"ferraus_gain","arguments":{"inputs":["in.wav"],"by":"-3dB"}}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}),
            json!({"jsonrpc":"2.0","id":4,"method":"prompts/get","params":{"name":"master-song","arguments":{"mix":"mix.wav"}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"ferraus://catalog"}}),
            json!({"jsonrpc":"2.0","id":6,"method":"nope"}),
        ]
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        struct W(Arc<Mutex<Vec<u8>>>);
        impl Write for W {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let out: Writer<'_> = Arc::new(Mutex::new(Box::new(W(buf.clone()))));
        s.serve_lines(std::io::Cursor::new(input), out).unwrap();
        let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let msgs: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let by_id = |i: i64| msgs.iter().find(|m| m["id"] == json!(i)).unwrap().clone();
        assert_eq!(by_id(1)["result"]["serverInfo"]["name"], "ferraus");
        let job = by_id(2)["result"]["structuredContent"]["job_id"].as_str().unwrap().to_string();
        assert!(by_id(3)["result"]["resources"].as_array().unwrap().len() >= 2);
        assert!(by_id(4)["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("mix.wav"));
        assert!(by_id(5)["result"]["contents"][0]["text"].as_str().unwrap().contains("gain"));
        assert_eq!(by_id(6)["error"]["code"], -32601);
        // the job finished before serve_lines returned (scoped threads are joined)
        let status = s
            .call_tool(
                "ferraus_job_result",
                &json!({"job_id": job}),
                &ferraus_core::ctx::NoProgress,
                CancelToken::default(),
                None,
            )
            .unwrap();
        assert_eq!(status["structuredContent"]["ok"], true, "{status}");
    }
}
