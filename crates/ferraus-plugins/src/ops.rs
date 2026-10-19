//! `plugin ...` commands. Every plugin is loaded in its own host process.

use crate::api::{self, PluginOptions, PluginRef, PluginSession, Tail};
use crate::client::{default_watchdog, PluginFormat};
use crate::scan::{self, ScanCache, ScanOptions, ScannedPlugin};
use base64::Engine;
use ferraus_core::exec::{blend, conform_output};
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

pub fn ops() -> Vec<OpSpec> {
    let mut v = plugin_ops();
    v.extend(crate::sounds::ops());
    v
}

fn watchdog_param() -> ferraus_core::ParamSpec {
    param("watchdog", ParamKind::Time)
        .help("Stop the plugin host after this long without progress (default 60s, or FERRAUS_PLUGIN_WATCHDOG_S)")
        .advanced()
}

fn plugin_name_param() -> ferraus_core::ParamSpec {
    param("name", ParamKind::Str)
        .required()
        .help("Plugin name, id, or path to a .clap/.vst3 bundle (path#plugin.id picks one plugin in a bundle)")
}

fn param_param() -> ferraus_core::ParamSpec {
    param("param", ParamKind::Str).repeatable().help(
        "Set a parameter: name=value, by display name or stable id; value in display units (cutoff=2.5kHz, mix=30%, gain=-6dB) or normalized (norm:0.3)",
    )
}

fn preset_param() -> ferraus_core::ParamSpec {
    param("preset", ParamKind::Str)
        .help("Load one of the plugin's presets by name (see 'plugin presets list') before applying --param values")
}

fn state_param() -> ferraus_core::ParamSpec {
    param("state", ParamKind::Path).help(
        "Restore native plugin state from a file written by 'plugin state save' (or a sidecar) before the preset and --param values",
    )
}

fn plugin_ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("plugin scan", Category::Plugins, "Find CLAP and VST3 plugins and cache their metadata", plugin_scan)
            .about("Searches the standard plugin folders for this OS, the folders given with --paths, and every --sound-path folder. Each CLAP bundle is probed in a separate ferraus-plughost process, so a plugin that crashes on load cannot stop the scan; VST3 metadata comes from the bundle's moduleinfo.json. Results are cached in <cache>/plugins/scan.json keyed by path and modification time, so later scans only probe new or changed bundles.")
            .inputs(Inputs::NONE)
            .param(param("paths", ParamKind::List).repeatable().help("Extra folders (or bundles) to scan, comma-separated or repeated"))
            .param(param("format", ParamKind::List).default("clap,vst3").help("Plugin formats to look for: clap, vst3"))
            .param(param("rescan", ParamKind::Flag).help("Probe every bundle again instead of trusting the cache"))
            .param(watchdog_param())
            .example("ferraus plugin scan", "Scan the standard plugin folders")
            .example("ferraus plugin scan --paths ~/plugins,/opt/clap --format clap --rescan --json", "Re-probe CLAP plugins in two extra folders and print the result as JSON"),
        OpSpec::custom("plugin list", Category::Plugins, "List cached plugins", plugin_list)
            .about("Lists the plugins found by the last scan (running one first if nothing is cached), plus any in --sound-path folders. Filter by kind or format.")
            .inputs(Inputs::NONE)
            .param(param("instruments", ParamKind::Flag).help("Only instruments"))
            .param(param("effects", ParamKind::Flag).help("Only audio and note effects"))
            .param(param("format", ParamKind::List).help("Only these formats: clap, vst3"))
            .example("ferraus plugin list --instruments", "Show instrument plugins")
            .example("ferraus plugin list --effects --format clap --json", "CLAP effects as JSON"),
        OpSpec::custom("plugin info", Category::Plugins, "Show a plugin's parameters, presets, ports, latency, and MIDI support", plugin_info)
            .about("Loads the plugin in a host process (never opening its GUI) and reports every parameter with its stable id, name, range, default, and unit, the presets it exposes, its audio and note ports, the latency it reports at 48 kHz, and whether it accepts MIDI.")
            .positional("name")
            .param(plugin_name_param())
            .param(watchdog_param())
            .example("ferraus plugin info \"Surge XT\"", "Parameters and presets of Surge XT")
            .example("ferraus plugin info ./MyPlugin.clap#com.acme.reverb --json", "One plugin from a bundle, as JSON"),
        OpSpec::custom("plugin run", Category::Plugins, "Run an effect plugin over audio", plugin_run)
            .about("Processes the input through an effect plugin in a separate host process, offline in fixed blocks. The plugin's reported latency is compensated so every sample stays at its original position; --tail keeps processing after the input ends so reverbs and delays can ring out. The resolved parameters, preset, and native state are written to the sidecar so the run can be reproduced.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("plugin", ParamKind::Str).help("Plugin name, id, or path to a .clap/.vst3 bundle (required)"))
            .param(param_param())
            .param(preset_param())
            .param(state_param())
            .param(param("tail", ParamKind::Time).help("Extra time to process after the input ends (e.g. 2s); default auto: the plugin's reported tail, or 2s when it reports none"))
            .param(param("block-size", ParamKind::Int).default("512").range("16..16384").help("Processing block size in samples"))
            .param(param("mix", ParamKind::Percent).default("100%").help("Dry/wet balance: 0% returns the input unchanged, 100% is fully processed"))
            .param(watchdog_param())
            .no_mix()
            .example("ferraus plugin run vocal.wav --plugin \"Dragonfly Room Reverb\" --tail 3s --mix 30% -o vocal.verb.wav", "Add 30% room reverb and let it ring for 3 s")
            .example("ferraus plugin run in.wav --plugin com.acme.eq --param \"Low Gain=-3dB\" --param cutoff=2.5kHz -o out.wav", "Set parameters in display units"),
        OpSpec::custom("plugin presets list", Category::Plugins, "List a plugin's presets", plugin_presets_list)
            .about("Discovers presets through the plugin's own preset-discovery interface (CLAP) or program lists (VST3). Use a name with --preset.")
            .positional("name")
            .param(plugin_name_param())
            .param(watchdog_param())
            .example("ferraus plugin presets list \"Surge XT\"", "List Surge XT's presets")
            .example("ferraus plugin presets list Dexed --json", "Presets as JSON"),
        OpSpec::custom("plugin state save", Category::Plugins, "Save a plugin's native state as base64 JSON", plugin_state_save)
            .about("Loads the plugin, applies --state, --preset, and --param values, and writes the plugin's native state blob (base64) with the resolved parameters to a JSON file. Pass that file to --state on 'plugin run' or 'render' to reproduce a sound exactly.")
            .positional("name")
            .param(plugin_name_param())
            .param(param_param())
            .param(preset_param())
            .param(state_param())
            .param(watchdog_param())
            .example("ferraus plugin state save \"Surge XT\" --preset \"Warm Pad\" -o pad.state.json", "Save a preset as a reusable state file")
            .example("ferraus plugin state save com.acme.eq --param cutoff=2.5kHz -o eq.json", "Save state after setting a parameter"),
        OpSpec::custom("plugin state load", Category::Plugins, "Load a saved state into a plugin and show the resulting parameters", plugin_state_load)
            .about("Checks that a state file loads into the plugin and reports every parameter value afterwards. Useful to inspect a state before rendering with it.")
            .positional("name")
            .param(plugin_name_param())
            .param(param("state", ParamKind::Path).required().help("State file from 'plugin state save' (or a sidecar with a plugin stage)"))
            .param(watchdog_param())
            .example("ferraus plugin state load \"Surge XT\" --state pad.state.json", "Show the parameters stored in a state file")
            .example("ferraus plugin state load Dexed --state song.wav.json --json", "Read the plugin state recorded in a sidecar"),
    ]
}

fn watchdog(a: &Args) -> Result<Duration> {
    if a.has("watchdog") {
        let s = a.seconds("watchdog")?;
        if s <= 0.0 {
            return Err(Error::usage("--watchdog must be positive"));
        }
        Ok(Duration::from_secs_f64(s))
    } else {
        Ok(default_watchdog())
    }
}

fn formats(a: &Args) -> Result<Vec<PluginFormat>> {
    let mut out = Vec::new();
    for f in a.list("format") {
        match PluginFormat::parse(&f) {
            Some(x) => {
                if !out.contains(&x) {
                    out.push(x)
                }
            }
            None => return Err(Error::usage(format!("--format: '{f}' is not clap or vst3"))),
        }
    }
    if out.is_empty() {
        out = vec![PluginFormat::Clap, PluginFormat::Vst3];
    }
    Ok(out)
}

fn plugin_json(p: &ScannedPlugin) -> Value {
    json!({
        "name": p.name, "id": p.id, "vendor": p.vendor, "version": p.version,
        "format": p.format, "kind": p.kind, "category": p.category,
        "features": p.features, "path": p.path.display().to_string(),
        "description": p.description,
    })
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(s)
}

fn plugin_scan(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut extra: Vec<PathBuf> = a.list("paths").iter().map(|s| expand_tilde(s)).collect();
    extra.extend(ctx.globals.sound_paths.iter().cloned());
    let opts = ScanOptions {
        extra,
        formats: formats(a)?,
        rescan: a.flag("rescan"),
        standard: true,
        watchdog: watchdog(a)?,
        cache_path: scan::default_cache_path(),
    };
    if ctx.globals.dry_run {
        let dirs = scan::scan_dirs(&opts);
        let bundles: Vec<Value> = dirs
            .iter()
            .flat_map(|d| scan::find_bundles(d, &opts.formats))
            .map(|(f, p)| json!({"format": f, "path": p.display().to_string()}))
            .collect();
        return Ok(json!({"dirs": dirs, "bundles": bundles}));
    }
    ctx.progress.log("scanning plugin folders");
    let r = scan::scan(&opts)?;
    for e in &r.errors {
        ctx.warn("plugin_probe_failed", format!("{} ({}): {}", e.path.display(), e.format, e.error));
    }
    ctx.metric("plugins", r.plugins.len());
    ctx.metric("bundles", r.bundles);
    ctx.metric("probed", r.probed);
    Ok(json!({
        "dirs": r.dirs,
        "bundles": r.bundles,
        "probed": r.probed,
        "cached": r.cached,
        "plugins": r.plugins.iter().map(plugin_json).collect::<Vec<_>>(),
        "errors": r.errors,
        "cache": opts.cache_path.display().to_string(),
    }))
}

fn plugin_list(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let cache_path = scan::default_cache_path();
    let mut cache = ScanCache::load(&cache_path);
    let fmts = formats(a)?;
    if cache.entries.is_empty() && !ctx.globals.dry_run {
        let opts = ScanOptions {
            extra: ctx.globals.sound_paths.clone(),
            formats: vec![PluginFormat::Clap, PluginFormat::Vst3],
            watchdog: default_watchdog(),
            cache_path: cache_path.clone(),
            ..Default::default()
        };
        scan::scan(&opts)?;
        cache = ScanCache::load(&cache_path);
    } else if !ctx.globals.sound_paths.is_empty() && !ctx.globals.dry_run {
        let opts = ScanOptions {
            extra: ctx.globals.sound_paths.clone(),
            standard: false,
            watchdog: default_watchdog(),
            cache_path: cache_path.clone(),
            ..Default::default()
        };
        scan::scan(&opts)?;
        cache = ScanCache::load(&cache_path);
    }
    let (inst, fx) = (a.flag("instruments"), a.flag("effects"));
    let mut list: Vec<ScannedPlugin> = cache
        .plugins()
        .into_iter()
        .filter(|p| fmts.contains(&p.format))
        .filter(|p| match (inst, fx) {
            (true, false) => p.is_instrument(),
            (false, true) => p.is_effect(),
            _ => true,
        })
        .collect();
    list.sort_by(|x, y| x.name.to_lowercase().cmp(&y.name.to_lowercase()).then(x.format.cmp(&y.format)));
    if list.is_empty() {
        ctx.warn("no_plugins", "no plugins matched; run 'ferraus plugin scan --paths <dir>' to add folders");
    }
    Ok(json!({"count": list.len(), "plugins": list.iter().map(plugin_json).collect::<Vec<_>>()}))
}

fn resolve(a: &Args, key: &str, ctx: &OpCtx) -> Result<PluginRef> {
    let name = a.str(key).ok_or_else(|| {
        Error::usage(format!("missing --{key}: name the plugin to use")).with_hint("ferraus plugin list")
    })?;
    scan::resolve_plugin(&name, &ctx.globals.sound_paths, watchdog(a)?)
}

fn ref_json(p: &PluginRef) -> Value {
    json!({"name": p.name, "id": p.plugin_id, "format": p.format, "path": p.path.display().to_string(), "vendor": p.vendor, "version": p.version})
}

fn plugin_info(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = resolve(a, "name", ctx)?;
    let mut s = PluginSession::open(&p, watchdog(a)?)?;
    let info = s.info.clone();
    let presets =
        if info.has_presets || info.format == "vst3" { s.host.list_presets().unwrap_or_default() } else { Vec::new() };
    let sr = ctx.globals.sample_rate.unwrap_or(48000);
    let act = s.host.activate(sr, 512, info.main_in_channels, info.main_out_channels, 1);
    let (latency, tail) = match act {
        Ok(a) => (Some(a.latency), a.tail),
        Err(e) => {
            ctx.warn("plugin_activate_failed", e.message.clone());
            (None, None)
        }
    };
    s.host.shutdown();
    let params: Vec<Value> = info
        .params
        .iter()
        .map(|q| {
            json!({
                "id": q.id, "name": q.name, "module": q.module, "min": q.min, "max": q.max,
                "default": q.default, "default_text": q.default_text, "unit": q.unit,
                "stepped": q.stepped, "hidden": q.hidden, "readonly": q.readonly,
                "automatable": q.automatable, "bypass": q.bypass,
            })
        })
        .collect();
    Ok(json!({
        "plugin": ref_json(&p),
        "descriptor": info.descriptor,
        "kind": info.descriptor.kind,
        "params": params,
        "presets": presets,
        "audio_inputs": info.audio_inputs,
        "audio_outputs": info.audio_outputs,
        "note_inputs": info.note_inputs,
        "note_outputs": info.note_outputs,
        "midi": info.accepts_notes(),
        "latency_samples": latency,
        "latency_sample_rate": sr,
        "tail_samples": tail,
        "has_state": info.has_state,
    }))
}

fn param_pairs(a: &Args) -> Result<Vec<(String, String)>> {
    a.all("param")
        .iter()
        .map(|kv| {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| Error::usage(format!("--param '{kv}' must be name=value (e.g. cutoff=2.5kHz)")))?;
            if k.trim().is_empty() {
                return Err(Error::usage(format!("--param '{kv}' has an empty name")));
            }
            Ok((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn options(a: &Args, ctx: &mut OpCtx) -> Result<PluginOptions> {
    let state = match a.path("state") {
        Some(p) => {
            ctx.check_allowed(&p)?;
            ctx.note_input(&p);
            Some(api::read_state_file(&p)?)
        }
        None => None,
    };
    let tail = if a.has("tail") { Tail::Seconds(a.seconds("tail")?) } else { Tail::Auto };
    Ok(PluginOptions {
        params: param_pairs(a)?,
        preset: a.str("preset"),
        state,
        block_size: if a.has("block-size") { a.int("block-size")? as u32 } else { 512 },
        tail,
        watchdog: watchdog(a)?,
        ..Default::default()
    })
}

fn plugin_run(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = resolve(a, "plugin", ctx)?;
    let opts = options(a, ctx)?;
    let mix = a.percent("mix")?;
    if !(0.0..=1.0).contains(&mix) {
        return Err(Error::usage("--mix must be between 0% and 100%"));
    }
    let input = ctx.read_audio(0)?;
    let out_path = ctx.output_path(None, "")?;
    if ctx.globals.dry_run {
        // resolve the plugin and the parameter set without processing
        let mut s = PluginSession::open(&p, opts.watchdog)?;
        let params = s.configure(&opts)?;
        s.host.shutdown();
        ctx.prepare_output(&out_path)?;
        return Ok(json!({
            "plugin": ref_json(&p),
            "params": params,
            "preset": opts.preset,
            "output": out_path.display().to_string(),
        }));
    }
    let run = {
        let progress = ctx.progress;
        let cancel = ctx.cancel.clone();
        let command = ctx.command.clone();
        api::process_effect_inner(&p, &input, &opts, &mut |f| {
            progress.progress(&command, f);
            cancel.check()
        })?
    };
    let wet = &run.audio;
    let out = if mix >= 1.0 { wet.clone() } else { blend(&input, wet, mix as f32) };
    let out = conform_output(out, &ctx.globals);
    ctx.write_audio(&out_path, &out, None)?;
    run.record_in(&p, ctx);
    ctx.note("plugin", p.name.clone());
    ctx.metric("latency_samples", run.latency);
    ctx.metric("tail_s", run.tail_frames as f64 / input.sample_rate as f64);
    Ok(json!({
        "plugin": ref_json(&p),
        "params": run.params,
        "preset": run.preset,
        "latency_samples": run.latency,
        "tail_s": run.tail_frames as f64 / input.sample_rate as f64,
        "output": out_path.display().to_string(),
        "deterministic": "unknown",
    }))
}

fn plugin_presets_list(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = resolve(a, "name", ctx)?;
    let mut s = PluginSession::open(&p, watchdog(a)?)?;
    let presets = s.host.list_presets()?;
    s.host.shutdown();
    if presets.is_empty() {
        ctx.warn("no_presets", "this plugin exposes no presets through its preset interface");
    }
    Ok(json!({"plugin": ref_json(&p), "count": presets.len(), "presets": presets}))
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn plugin_state_save(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = resolve(a, "name", ctx)?;
    let opts = options(a, ctx)?;
    let mut s = PluginSession::open(&p, opts.watchdog)?;
    if !s.info.has_state {
        return Err(Error::new(
            ErrorKind::Plugin,
            "no_state",
            format!("plugin '{}' does not support saving its state", s.info.descriptor.name),
        ));
    }
    let set = s.configure(&opts)?;
    let state = s.host.get_state()?;
    let params = s.host.get_params()?;
    s.host.shutdown();
    let path = match &ctx.globals.output {
        Some(_) => ctx.output_path(None, "json")?,
        None => {
            let f = PathBuf::from(format!("{}.state.json", slug(&s.info.descriptor.name)));
            match &ctx.globals.outdir {
                Some(d) => d.join(f),
                None => f,
            }
        }
    };
    let doc = json!({
        "format": "ferraus-plugin-state",
        "version": 1,
        "plugin": {
            "name": s.info.descriptor.name, "id": s.info.descriptor.id, "vendor": s.info.descriptor.vendor,
            "version": s.info.descriptor.version, "format": p.format, "path": p.path.display().to_string(),
        },
        "preset": opts.preset,
        "params_set": set,
        "params": params,
        "state_b64": base64::engine::general_purpose::STANDARD.encode(&state),
    });
    let bytes = serde_json::to_vec_pretty(&doc)?;
    ctx.write_file(&path, &bytes, Some("state"))?;
    Ok(json!({
        "plugin": ref_json(&p),
        "output": path.display().to_string(),
        "state_bytes": state.len(),
        "params": params.len(),
    }))
}

fn plugin_state_load(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let p = resolve(a, "name", ctx)?;
    let path = a.path("state").ok_or_else(|| Error::usage("missing --state"))?;
    ctx.check_allowed(&path)?;
    ctx.note_input(&path);
    let state = api::read_state_file(&path)?;
    let mut s = PluginSession::open(&p, watchdog(a)?)?;
    s.host.set_state(&state)?;
    let params = s.host.get_params()?;
    s.host.shutdown();
    Ok(json!({"plugin": ref_json(&p), "loaded": true, "state_bytes": state.len(), "params": params}))
}
