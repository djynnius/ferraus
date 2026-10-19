//! Command-line parsing: the clap tree is generated from the registry, so every op's
//! flags, help text, defaults, and choices come from its `OpSpec`.

use crate::config::Config;
use crate::progress::StderrProgress;
use clap::{Arg, ArgAction, ArgMatches, Command};
use ferraus_core::ctx::Globals;
use ferraus_core::envelope::Envelope;
use ferraus_core::exec::{Invocation, Runtime};
use ferraus_core::op::{OpSpec, ParamKind, ParamSpec};
use ferraus_core::{units, Error, Registry};
use std::collections::BTreeSet;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

/// Names of global flags. Op params with the same name are filled from the global value.
pub const GLOBAL_FLAGS: &[&str] = &[
    "output",
    "outdir",
    "format",
    "input-format",
    "json",
    "progress",
    "quiet",
    "verbose",
    "threads",
    "device",
    "deterministic",
    "seed",
    "offline",
    "yes",
    "overwrite",
    "in-place",
    "sidecar",
    "no-sidecar",
    "dry-run",
    "explain",
    "force",
    "sample-rate",
    "bit-depth",
    "channels",
    "bitrate",
    "config",
    "sound-path",
    "bpm",
    "async",
    "timeout",
    "max-memory",
];

const ABOUT: &str = "ferraus: a complete music-editing toolkit for people and AI agents.\n\nEvery command accepts --json for a structured result, --dry-run to plan without processing, and writes a provenance sidecar next to each output. Run 'ferraus describe --agent' for a compact catalog or 'ferraus docs <command>' for full help.";

fn global_args() -> Vec<Arg> {
    let g = |a: Arg| a.global(true).help_heading("Global options");
    vec![
        g(Arg::new("output")
            .short('o')
            .long("output")
            .value_name("PATH")
            .allow_hyphen_values(true)
            .help("Output path; '-' for stdout; templates {name} {stem} {index} {ext}")),
        g(Arg::new("outdir").long("outdir").value_name("DIR").help("Output directory for batch runs")),
        g(Arg::new("format")
            .long("format")
            .value_name("FMT")
            .help("Output container/codec when the extension is ambiguous or output is stdout")),
        g(Arg::new("input-format").long("input-format").value_name("FMT").help("Input format when reading stdin")),
        g(Arg::new("json")
            .long("json")
            .action(ArgAction::SetTrue)
            .help("Structured result on stdout, nothing else on stdout")),
        g(Arg::new("progress")
            .long("progress")
            .value_name("MODE")
            .value_parser(["auto", "none", "bar", "json"])
            .help("Progress on stderr [default: auto]")),
        g(Arg::new("quiet").short('q').long("quiet").action(ArgAction::SetTrue).help("Only errors on stderr")),
        g(Arg::new("verbose")
            .short('v')
            .long("verbose")
            .action(ArgAction::Count)
            .help("More logging on stderr (-vv for debug)")),
        g(Arg::new("threads")
            .long("threads")
            .value_name("N")
            .help("CPU threads for DSP and inference [default: all cores]")),
        g(Arg::new("device")
            .long("device")
            .value_name("DEV")
            .value_parser(["auto", "cpu", "cuda", "coreml", "directml"])
            .help("Inference device [default: auto]")),
        g(Arg::new("deterministic")
            .long("deterministic")
            .action(ArgAction::SetTrue)
            .help("Bit-exact CPU results; implies a fixed thread count")),
        g(Arg::new("seed")
            .long("seed")
            .value_name("N")
            .help("Seed for any stochastic step (dither, humanize) [default: 0]")),
        g(Arg::new("offline").long("offline").action(ArgAction::SetTrue).help("Fail instead of downloading anything")),
        g(Arg::new("yes")
            .short('y')
            .long("yes")
            .action(ArgAction::SetTrue)
            .help("Allow model downloads without asking")),
        g(Arg::new("overwrite").long("overwrite").action(ArgAction::SetTrue).help("Replace an existing output")),
        g(Arg::new("in-place")
            .long("in-place")
            .action(ArgAction::SetTrue)
            .help("Write back to the input path, keeping <input>.bak")),
        g(Arg::new("sidecar")
            .long("sidecar")
            .action(ArgAction::SetTrue)
            .help("Write <output>.ferraus.json provenance (default on)")),
        g(Arg::new("no-sidecar")
            .long("no-sidecar")
            .action(ArgAction::SetTrue)
            .help("Do not write provenance sidecars")),
        g(Arg::new("dry-run")
            .long("dry-run")
            .action(ArgAction::SetTrue)
            .help("Resolve parameters and print the plan without processing")),
        g(Arg::new("explain")
            .long("explain")
            .action(ArgAction::SetTrue)
            .help("Explain what each resolved parameter does and why it was chosen")),
        g(Arg::new("force")
            .long("force")
            .action(ArgAction::SetTrue)
            .help("Recompute even if an identical output already exists")),
        g(Arg::new("sample-rate")
            .long("sample-rate")
            .value_name("HZ")
            .help("Output sample rate [default: match input]")),
        g(Arg::new("bit-depth").long("bit-depth").value_name("BITS").help("Output bit depth (16, 24, 32 = float)")),
        g(Arg::new("channels").long("channels").value_name("N").help("Output channel count [default: match input]")),
        g(Arg::new("bitrate").long("bitrate").value_name("RATE").help("Lossy bitrate, e.g. 192k [default: 192k]")),
        g(Arg::new("config").long("config").value_name("PATH").help("Alternate config file")),
        g(Arg::new("sound-path")
            .long("sound-path")
            .value_name("DIR")
            .action(ArgAction::Append)
            .help("Extra folder of instruments and sound libraries to scan (repeatable)")),
        g(Arg::new("bpm").long("bpm").value_name("BPM").help("Tempo for beat-based times (4b, 1/4)")),
        g(Arg::new("async")
            .long("async")
            .action(ArgAction::SetTrue)
            .help("Run as a background job and return its job_id immediately")),
        g(Arg::new("timeout").long("timeout").value_name("TIME").help("Cancel the job after this long (exit code 8)")),
        g(Arg::new("max-memory")
            .long("max-memory")
            .value_name("SIZE")
            .help("Memory budget before offline stages spill to disk [default: 2GB]")),
    ]
}

fn param_help(p: &ParamSpec) -> String {
    let mut h = p.help.to_string();
    if !p.choices.is_empty() && p.kind == ParamKind::Choice {
        h.push_str(&format!(" [{}]", p.choices.join("|")));
    }
    if let Some(r) = p.range {
        h.push_str(&format!(" (range {r})"));
    }
    if let Some(d) = p.default {
        h.push_str(&format!(" [default: {d}]"));
    }
    if p.repeatable {
        h.push_str(" (repeatable)");
    }
    h
}

fn op_args(op: &OpSpec) -> Vec<Arg> {
    let mut args = Vec::new();
    for p in op.effective_params() {
        if GLOBAL_FLAGS.contains(&p.name) {
            continue;
        }
        if op.positional == Some(p.name) {
            continue;
        }
        let mut a = Arg::new(p.name).long(p.name).help(param_help(&p));
        a = match p.kind {
            ParamKind::Flag => a.action(ArgAction::SetTrue),
            ParamKind::Switch => {
                a.num_args(0..=1).default_missing_value("on").value_name("on|off").action(ArgAction::Set)
            }
            _ => {
                let a = a.num_args(1).allow_hyphen_values(true).value_name(p.kind.type_name().to_uppercase());
                if p.repeatable {
                    a.action(ArgAction::Append)
                } else {
                    a.action(ArgAction::Set)
                }
            }
        };
        if p.advanced {
            a = a.hide_short_help(true);
        }
        args.push(a);
    }
    if let Some(pos) = op.positional {
        let p = op.find_param(pos);
        let mut a = Arg::new("__positional")
            .num_args(0..)
            .action(ArgAction::Append)
            .value_name(pos.to_uppercase())
            .help(p.map(|p| p.help).unwrap_or(""));
        if p.map(|p| p.kind == ParamKind::List).unwrap_or(false) {
            a = a.trailing_var_arg(true).allow_hyphen_values(true);
        }
        args.push(a);
    } else if op.inputs.max != Some(0) {
        args.push(
            Arg::new("__inputs")
                .num_args(0..)
                .action(ArgAction::Append)
                .value_name("INPUT")
                .help("Input files (globs allowed; '-' for stdin)"),
        );
    }
    args
}

fn op_long_about(op: &OpSpec) -> String {
    let mut s = op.summary.to_string();
    if !op.about.is_empty() {
        s.push_str("\n\n");
        s.push_str(op.about);
    }
    if op.best_effort {
        s.push_str("\n\nQuality tier: best-effort.");
    }
    if !op.presets.is_empty() {
        s.push_str("\n\nPresets: ");
        s.push_str(&op.presets.iter().map(|p| p.name).collect::<Vec<_>>().join(", "));
    }
    if !op.examples.is_empty() {
        s.push_str("\n\nExamples:");
        for e in &op.examples {
            s.push_str(&format!("\n  {}\n      {}", e.cmd, e.explain));
        }
    }
    s
}

/// Build the full clap tree from the registry.
pub fn build_command(reg: &Registry) -> Command {
    let paths: BTreeSet<Vec<&'static str>> = reg.ops().iter().map(|o| o.name.split(' ').collect()).collect();
    let mut prefixes: BTreeSet<Vec<&'static str>> = BTreeSet::new();
    for p in &paths {
        for i in 1..=p.len() {
            prefixes.insert(p[..i].to_vec());
        }
    }
    fn node(prefix: Vec<&'static str>, reg: &Registry, prefixes: &BTreeSet<Vec<&'static str>>) -> Command {
        let name = *prefix.last().unwrap();
        let joined = prefix.join(" ");
        let mut cmd = Command::new(name);
        let children: Vec<Vec<&'static str>> = prefixes
            .iter()
            .filter(|p| p.len() == prefix.len() + 1 && p[..prefix.len()] == prefix[..])
            .cloned()
            .collect();
        if let Some(op) = reg.get(&joined).filter(|o| o.name == joined) {
            cmd = cmd.about(op.summary).long_about(op_long_about(op)).args(op_args(op));
            for a in &op.aliases {
                if !a.contains(' ') {
                    cmd = cmd.visible_alias(*a);
                }
            }
            if !children.is_empty() {
                cmd = cmd.args_conflicts_with_subcommands(true).subcommand_negates_reqs(true);
            }
        } else {
            let kids: Vec<String> = children.iter().map(|c| c.last().unwrap().to_string()).collect();
            cmd = cmd
                .about(format!("{} commands: {}", name, kids.join(", ")))
                .subcommand_required(true)
                .arg_required_else_help(true);
        }
        for c in children {
            cmd = cmd.subcommand(node(c, reg, prefixes));
        }
        cmd
    }
    let mut root = Command::new("ferraus")
        .version(ferraus_core::FERRAUS_VERSION)
        .about(ABOUT)
        .arg_required_else_help(true)
        .subcommand_required(true)
        .max_term_width(100)
        .args(global_args());
    for p in prefixes.iter().filter(|p| p.len() == 1) {
        root = root.subcommand(node(p.clone(), reg, &prefixes));
    }
    root
}

/// Walk matches to the leaf subcommand; returns the command path and its matches.
fn leaf(m: &ArgMatches) -> (Vec<String>, &ArgMatches) {
    let mut path = Vec::new();
    let mut cur = m;
    while let Some((name, sub)) = cur.subcommand() {
        path.push(name.to_string());
        cur = sub;
    }
    (path, cur)
}

fn get_str(m: &ArgMatches, id: &str) -> Option<String> {
    m.try_get_one::<String>(id).ok().flatten().cloned()
}

fn get_flag(m: &ArgMatches, id: &str) -> bool {
    m.try_get_one::<bool>(id).ok().flatten().copied().unwrap_or(false)
}

#[allow(clippy::field_reassign_with_default)]
fn parse_globals(m: &ArgMatches, cfg: &Config) -> ferraus_core::Result<(Globals, OutputPrefs)> {
    let mut g = Globals::default();
    g.output = get_str(m, "output");
    g.outdir = get_str(m, "outdir").map(PathBuf::from);
    g.format = get_str(m, "format");
    g.input_format = get_str(m, "input-format");
    g.overwrite = get_flag(m, "overwrite");
    g.in_place = get_flag(m, "in-place");
    g.sidecar = !get_flag(m, "no-sidecar") && cfg.output.sidecar.unwrap_or(true) || get_flag(m, "sidecar");
    g.dry_run = get_flag(m, "dry-run");
    g.explain = get_flag(m, "explain");
    g.force = get_flag(m, "force");
    g.deterministic = get_flag(m, "deterministic") || cfg.compute.deterministic.unwrap_or(false);
    g.offline = get_flag(m, "offline");
    g.yes = get_flag(m, "yes");
    let num = |id: &str| -> ferraus_core::Result<Option<u64>> {
        match get_str(m, id) {
            None => Ok(None),
            Some(v) => v
                .trim()
                .parse::<u64>()
                .map(Some)
                .map_err(|_| Error::usage(format!("--{id}: '{v}' is not a whole number"))),
        }
    };
    g.sample_rate = match get_str(m, "sample-rate") {
        None => None,
        Some(v) => {
            Some(units::parse_freq(&v).map_err(|e| Error::usage(format!("--sample-rate: {}", e.message)))? as u32)
        }
    };
    g.bit_depth = num("bit-depth")?.map(|v| v as u16).or(cfg.output.bit_depth);
    if let Some(b) = g.bit_depth {
        if ![8, 16, 24, 32].contains(&b) {
            return Err(Error::usage(format!("--bit-depth must be 8, 16, 24, or 32 (float), got {b}")));
        }
    }
    g.channels = num("channels")?.map(|v| v as usize);
    g.bitrate = match get_str(m, "bitrate") {
        None => None,
        Some(v) => Some(units::parse_bitrate(&v)?),
    };
    g.seed = num("seed")?.unwrap_or(0);
    g.threads = num("threads")?.map(|v| v as usize).or(cfg.compute.threads).unwrap_or(0);
    if g.deterministic && g.threads == 0 {
        g.threads = 4;
    }
    g.device = get_str(m, "device").or_else(|| cfg.compute.device.clone()).unwrap_or_else(|| "auto".into());
    g.bpm = match get_str(m, "bpm") {
        None => None,
        Some(v) => Some(v.trim().parse::<f64>().map_err(|_| Error::usage(format!("--bpm: '{v}' is not a number")))?),
    };
    g.sound_paths = m
        .try_get_many::<String>("sound-path")
        .ok()
        .flatten()
        .map(|v| v.map(PathBuf::from).collect())
        .unwrap_or_default();
    if let Ok(env) = std::env::var("FERRAUS_SOUND_PATH") {
        g.sound_paths.extend(std::env::split_paths(&env));
    }
    g.sound_paths.extend(cfg.sounds.paths.iter().cloned());
    g.max_memory = match get_str(m, "max-memory").or_else(|| cfg.compute.max_memory.clone()) {
        None => g.max_memory,
        Some(v) => units::parse_bytes(&v)?,
    };
    g.config_path = get_str(m, "config").map(PathBuf::from);
    if g.offline {
        std::env::set_var("FERRAUS_OFFLINE", "1");
    }
    // -y wins over the config; the config opts in or out otherwise (ADR 0004)
    if g.yes {
        std::env::set_var("FERRAUS_AUTO_DOWNLOAD", "1");
    } else if let Some(auto) = cfg.models.auto_download {
        std::env::set_var("FERRAUS_AUTO_DOWNLOAD", if auto { "1" } else { "0" });
    }
    if let Some(gb) = cfg.models.max_cache_gb {
        std::env::set_var("FERRAUS_MAX_CACHE_GB", gb.to_string());
    }
    if let Some(url) = &cfg.models.registry_url {
        std::env::set_var("FERRAUS_REGISTRY_URL", url);
    }
    if let Some(p) = &cfg.ffmpeg.path {
        std::env::set_var("FERRAUS_FFMPEG", p);
    }
    if !cfg.plugins.extra_paths.is_empty() {
        g.sound_paths.extend(cfg.plugins.extra_paths.iter().cloned());
    }
    if g.threads > 0 {
        let _ = rayon_init(g.threads);
    }
    let prefs = OutputPrefs {
        json: get_flag(m, "json"),
        progress: get_str(m, "progress").unwrap_or_else(|| "auto".into()),
        verbosity: if get_flag(m, "quiet") {
            -1
        } else {
            m.try_get_one::<u8>("verbose").ok().flatten().copied().unwrap_or(0) as i32
        },
        is_async: get_flag(m, "async"),
        timeout: match get_str(m, "timeout") {
            None => None,
            Some(v) => Some(units::parse_duration_s(&v)?),
        },
    };
    if prefs.json && g.output.as_deref() == Some("-") {
        return Err(Error::usage("--json and -o - both want stdout; write the audio to a file or drop --json"));
    }
    Ok((g, prefs))
}

fn rayon_init(threads: usize) -> Result<(), String> {
    std::env::set_var("RAYON_NUM_THREADS", threads.to_string());
    Ok(())
}

pub struct OutputPrefs {
    pub json: bool,
    pub progress: String,
    pub verbosity: i32,
    pub is_async: bool,
    pub timeout: Option<f64>,
}

fn print_envelope(env: &Envelope, prefs: &OutputPrefs) {
    let out = std::io::stdout();
    let mut out = out.lock();
    if prefs.json {
        let text = if out.is_terminal() { serde_json::to_string_pretty(env) } else { serde_json::to_string(env) };
        let _ = writeln!(out, "{}", text.unwrap_or_default());
        return;
    }
    if let Some(e) = &env.error {
        eprintln!("error: {}", e.message);
        if let Some(h) = &e.hint {
            eprintln!("hint: {h}");
        }
        return;
    }
    if prefs.verbosity < 0 {
        return;
    }
    if let Some(t) = env.result.get("text").and_then(|t| t.as_str()) {
        let _ = write!(out, "{t}");
        if !t.ends_with('\n') {
            let _ = writeln!(out);
        }
        return;
    }
    for o in &env.outputs {
        match &o.role {
            Some(r) => {
                let _ = writeln!(out, "{} ({r})", o.path);
            }
            None => {
                let _ = writeln!(out, "{}", o.path);
            }
        }
    }
    let has_result = !env.result.is_null() && env.result != serde_json::json!({"reused": true});
    if has_result {
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&env.result).unwrap_or_default());
    }
    if !env.metrics.is_empty() && (env.outputs.is_empty() || prefs.verbosity > 0) && !has_result {
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&env.metrics).unwrap_or_default());
    }
}

fn error_exit(command: &str, e: Error, json: bool) -> i32 {
    let code = e.exit_code;
    let env = Envelope::error(command, e, 0);
    print_envelope(&env, &OutputPrefs { json, progress: "none".into(), verbosity: 0, is_async: false, timeout: None });
    code
}

/// If argv has `--help --json`, print the command's schema instead of text help.
fn help_json(reg: &Registry, argv: &[String]) -> Option<i32> {
    let has_help = argv.iter().any(|a| a == "--help" || a == "-h");
    let has_json = argv.iter().any(|a| a == "--json");
    if !(has_help && has_json) {
        return None;
    }
    let words: Vec<&str> = argv.iter().skip(1).filter(|a| !a.starts_with('-')).map(|s| s.as_str()).collect();
    for n in (1..=words.len().min(3)).rev() {
        let name = words[..n].join(" ");
        if let Some(op) = reg.get(&name).filter(|o| o.name == name) {
            println!("{}", serde_json::to_string_pretty(&ferraus_core::schema::command_schema(op)).unwrap());
            return Some(0);
        }
    }
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "commands": reg.ops().iter().map(|o| serde_json::json!({"command": o.name, "summary": o.summary})).collect::<Vec<_>>()
    })).unwrap());
    Some(0)
}

pub fn run(argv: Vec<String>) -> i32 {
    let reg = crate::registry();
    if let Some(code) = help_json(&reg, &argv) {
        return code;
    }
    let wants_json = argv.iter().any(|a| a == "--json");
    let matches = match build_command(&reg).try_get_matches_from(&argv) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind as K;
            return match e.kind() {
                K::DisplayHelp | K::DisplayVersion | K::DisplayHelpOnMissingArgumentOrSubcommand => {
                    let _ = e.print();
                    0
                }
                _ if wants_json => error_exit(
                    "ferraus",
                    Error::usage(e.to_string().trim().trim_start_matches("error: ").to_string()),
                    true,
                ),
                _ => {
                    let _ = e.print();
                    2
                }
            };
        }
    };
    let (path, leaf_m) = leaf(&matches);
    let command = path.join(" ");
    let op = match reg.get(&command) {
        Some(o) => o,
        None => return error_exit(&command, Error::usage(format!("unknown command '{command}'")), wants_json),
    };
    let cfg = match Config::load(get_str(&matches, "config").map(PathBuf::from).as_deref()) {
        Ok(c) => c,
        Err(e) => return error_exit(&command, e, wants_json),
    };
    let (globals, prefs) = match parse_globals(leaf_m, &cfg) {
        Ok(g) => g,
        Err(e) => return error_exit(&command, e, wants_json),
    };

    // op args: config overrides first, then explicit flags (later values win)
    let mut raw: Vec<(String, String)> = Vec::new();
    for (k, v) in cfg.preset_overrides(op.name) {
        if op.find_param(&k).is_some() {
            raw.push((k, v));
        }
    }
    let mut inputs: Vec<String> = Vec::new();
    for p in op.effective_params() {
        if GLOBAL_FLAGS.contains(&p.name) {
            if let Some(v) = get_str(leaf_m, p.name) {
                raw.push((p.name.to_string(), v));
            } else if p.kind == ParamKind::Flag && get_flag(leaf_m, p.name) {
                raw.push((p.name.to_string(), "true".into()));
            }
            continue;
        }
        if op.positional == Some(p.name) {
            continue;
        }
        match p.kind {
            ParamKind::Flag => {
                if get_flag(leaf_m, p.name) {
                    raw.push((p.name.to_string(), "true".into()));
                }
            }
            _ if p.repeatable => {
                if let Ok(Some(vals)) = leaf_m.try_get_many::<String>(p.name) {
                    for v in vals {
                        raw.push((p.name.to_string(), v.clone()));
                    }
                }
            }
            _ => {
                if let Some(v) = get_str(leaf_m, p.name) {
                    raw.push((p.name.to_string(), v));
                }
            }
        }
    }
    if let Some(pos) = op.positional {
        if let Ok(Some(vals)) = leaf_m.try_get_many::<String>("__positional") {
            let vals: Vec<String> = vals.cloned().collect();
            let repeat = op.find_param(pos).map(|p| p.repeatable || p.kind == ParamKind::List).unwrap_or(false);
            if repeat {
                for v in vals {
                    raw.push((pos.to_string(), v));
                }
            } else if !vals.is_empty() {
                raw.push((pos.to_string(), vals.join(" ")));
            }
        }
    } else if let Ok(Some(vals)) = leaf_m.try_get_many::<String>("__inputs") {
        inputs = vals.cloned().collect();
    }

    if prefs.is_async {
        return crate::system::spawn_async(&argv, prefs.json);
    }

    let stdin = if inputs.iter().any(|i| i == "-") {
        let mut b = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut b) {
            return error_exit(&command, Error::input(format!("cannot read stdin: {e}")), prefs.json);
        }
        Some(b)
    } else {
        None
    };

    let progress = StderrProgress::new(&prefs.progress, prefs.verbosity);
    let io = ferraus_codec::CodecIo;
    let cancel = ferraus_core::ctx::CancelToken::default();
    install_cancel_handlers(&cancel, &command, prefs.json, prefs.timeout);
    let user_presets = |effect: &str, name: &str| ferraus_core::builtin::load_user_preset(effect, name);
    let rt = Runtime { registry: &reg, io: &io, progress: &progress, presets: &user_presets, cancel };
    let inv = Invocation {
        command: op.name.to_string(),
        args: raw,
        inputs,
        globals,
        argv: argv.iter().skip(1).cloned().collect(),
        stdin,
    };
    let outcome = rt.invoke(inv);
    progress.finish();
    if let Some(bytes) = &outcome.stdout {
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(bytes);
        let _ = so.flush();
    }
    print_envelope(&outcome.envelope, &prefs);
    outcome.envelope.exit_code()
}

fn install_cancel_handlers(cancel: &ferraus_core::ctx::CancelToken, command: &str, json: bool, timeout: Option<f64>) {
    if let Some(secs) = timeout {
        let c = cancel.clone();
        let command = command.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(secs));
            c.cancel();
            // give cooperative stages a moment to stop, then exit hard
            std::thread::sleep(Duration::from_secs(2));
            let e = Error::new(
                ferraus_core::ErrorKind::Cancelled,
                "timeout",
                format!("cancelled after the {secs}s timeout"),
            );
            std::process::exit(error_exit(&command, e, json));
        });
    }
}
