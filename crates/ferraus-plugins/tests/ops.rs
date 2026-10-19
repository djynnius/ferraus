//! The `plugin ...` and `sounds ...` commands end to end through the shared executor.
//! One test function: it points FERRAUS_HOME and FERRAUS_PLUGIN_DIR at a temp folder.

mod common;

use ferraus_core::exec::{Invocation, Runtime};
use ferraus_core::testing::{read_wav, write_wav, TempDir, NO_PROGRESS, SIMPLE_IO};
use ferraus_core::{AudioBuffer, Envelope, Globals, Registry, Sidecar};
use serde_json::Value;
use std::path::{Path, PathBuf};

struct Ctx {
    reg: Registry,
    sound_paths: Vec<PathBuf>,
}

impl Ctx {
    fn run_with(&self, cmd: &str, args: &[(&str, &str)], inputs: &[&Path], f: impl FnOnce(&mut Globals)) -> Envelope {
        let mut globals = Globals { sound_paths: self.sound_paths.clone(), overwrite: true, ..Default::default() };
        f(&mut globals);
        let rt = Runtime {
            registry: &self.reg,
            io: &SIMPLE_IO,
            progress: &NO_PROGRESS,
            presets: &|_, _| None,
            cancel: Default::default(),
        };
        rt.invoke(Invocation {
            command: cmd.into(),
            args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            inputs: inputs.iter().map(|p| p.display().to_string()).collect(),
            globals,
            argv: vec![cmd.into()],
            stdin: None,
        })
        .envelope
    }

    fn run(&self, cmd: &str, args: &[(&str, &str)], inputs: &[&Path]) -> Envelope {
        self.run_with(cmd, args, inputs, |_| {})
    }

    fn ok(&self, cmd: &str, args: &[(&str, &str)], inputs: &[&Path]) -> Value {
        let e = self.run(cmd, args, inputs);
        assert!(e.ok, "{cmd} failed: {:?}", e.error);
        e.result
    }
}

fn tone() -> AudioBuffer {
    let s: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.03).sin() * 0.8).collect();
    AudioBuffer::from_planar(48000, vec![s.clone(), s])
}

#[test]
fn plugin_and_sounds_commands() {
    let d = TempDir::new("ops");
    std::env::set_var("FERRAUS_HOME", d.path("home"));
    std::env::set_var("FERRAUS_PLUGIN_DIR", d.path("home/plugins"));
    std::env::set_var("FERRAUS_PLUGIN_STANDARD_DIRS", "0");
    std::env::set_var("FERRAUS_PLUGIN_WATCHDOG_S", "30");
    std::env::remove_var("FERRAUS_AUTO_DOWNLOAD");
    let folder = common::sound_folder(&d.0);
    let mut c = Ctx {
        reg: {
            let mut r = Registry::new();
            r.extend(ferraus_plugins::ops());
            r
        },
        sound_paths: vec![folder.clone()],
    };

    // every op is documented
    for op in ferraus_plugins::ops() {
        assert!(!op.about.is_empty(), "{} has no about", op.name);
        assert!(op.examples.len() >= 2, "{} needs two examples", op.name);
        for p in &op.params {
            assert!(!p.help.is_empty(), "{} --{} has no help", op.name, p.name);
        }
    }

    // --sound-path is honored by plugin info before any scan
    let info = c.ok("plugin info", &[("name", "Ferraus Test Sine")], &[]);
    assert_eq!(info["kind"], "instrument");
    assert_eq!(info["midi"], true);
    assert_eq!(info["params"][0]["name"], "level");
    c.sound_paths.clear();

    // scan: CLAP plus a fake VST3 bundle with moduleinfo.json
    let plugdir = d.path("plugins");
    common::clap_bundle(&plugdir, "FerrausTest");
    common::fake_vst3(&plugdir, "Fake Synth", true);
    let pd = plugdir.display().to_string();
    let scan = c.ok("plugin scan", &[("paths", &pd)], &[]);
    let names: Vec<&str> = scan["plugins"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Ferraus Test Gain") && names.contains(&"Fake Synth"), "{names:?}");
    let list = c.ok("plugin list", &[("instruments", "true")], &[]);
    let names: Vec<&str> = list["plugins"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Ferraus Test Sine") && names.contains(&"Fake Synth"));
    assert!(!names.contains(&"Ferraus Test Gain"));
    let list = c.ok("plugin list", &[("effects", "true"), ("format", "clap")], &[]);
    assert!(list["plugins"].as_array().unwrap().iter().all(|p| p["format"] == "clap" && p["kind"] == "audio-effect"));

    // info: params, presets, latency
    let info = c.ok("plugin info", &[("name", "Ferraus Test Gain")], &[]);
    assert_eq!(info["params"][0]["unit"], "dB");
    assert_eq!(info["presets"].as_array().unwrap().len(), 2);
    assert_eq!(info["latency_samples"], 0);
    let info = c.ok("plugin info", &[("name", "ferraus.test.delay")], &[]);
    assert_eq!(info["latency_samples"], 64);
    let presets = c.ok("plugin presets list", &[("name", "Ferraus Test Gain")], &[]);
    assert_eq!(presets["presets"][0]["name"], "Quiet");

    // run: display-unit params, sidecar records the plugin stage
    let input = d.path("in.wav");
    write_wav(&input, &tone());
    let out = d.path("out.wav");
    let o = out.display().to_string();
    let e = c.run_with("plugin run", &[("plugin", "Ferraus Test Gain"), ("param", "gain=-6dB")], &[&input], |g| {
        g.output = Some(o.clone())
    });
    assert!(e.ok, "{:?}", e.error);
    let y = read_wav(&out);
    let ratio = y.peak() / tone().peak();
    assert!((ratio - 0.501).abs() < 0.003, "{ratio}");
    let side: Value = serde_json::from_slice(&std::fs::read(Sidecar::path_for(&out)).unwrap()).unwrap();
    assert_eq!(side["deterministic"], "unknown");
    assert_eq!(side["plugins"][0]["id"], "ferraus.test.gain");
    assert_eq!(side["plugins"][0]["params"]["gain"]["value"], -6.0);
    assert!(side["plugins"][0]["state_b64"].is_string());
    // --mix 0% returns the input
    let e = c.run_with(
        "plugin run",
        &[("plugin", "Ferraus Test Gain"), ("param", "gain=-20dB"), ("mix", "0%")],
        &[&input],
        |g| g.output = Some(o.clone()),
    );
    assert!(e.ok);
    assert_eq!(read_wav(&out).data, read_wav(&input).data);
    // dry run resolves the parameter set without writing
    let dry = d.path("dry.wav");
    let dr = dry.display().to_string();
    let e = c.run_with("plugin run", &[("plugin", "Ferraus Test Gain"), ("param", "gain=norm:0.5")], &[&input], |g| {
        g.output = Some(dr.clone());
        g.dry_run = true;
    });
    assert!(e.ok, "{:?}", e.error);
    assert_eq!(e.result["params"][0]["value"], -18.0);
    assert!(!dry.exists());
    // bad parameter → usage error (exit 2); missing plugin → exit 7
    let e = c.run("plugin run", &[("plugin", "Ferraus Test Gain"), ("param", "cutoff=1kHz")], &[&input]);
    assert_eq!(e.error.unwrap().exit_code, 2);
    let e = c.run("plugin run", &[("plugin", "Nope Verb")], &[&input]);
    assert_eq!(e.error.unwrap().exit_code, 7);
    // the crash plugin fails with exit 7 and the next run works
    let e = c.run(
        "plugin run",
        &[("plugin", "Ferraus Test Crash"), ("param", "crash_on_process=on"), ("watchdog", "10s")],
        &[&input],
    );
    let err = e.error.unwrap();
    assert_eq!(err.exit_code, 7, "{err:?}");
    assert!(err.message.contains("Ferraus Test Crash"));
    let e = c.run_with("plugin run", &[("plugin", "Ferraus Test Gain")], &[&input], |g| g.output = Some(o.clone()));
    assert!(e.ok);

    // state save / load / run --state
    let st = d.path("gain.state.json");
    let s = st.display().to_string();
    let e = c.run_with("plugin state save", &[("name", "Ferraus Test Gain"), ("param", "gain=-12dB")], &[], |g| {
        g.output = Some(s.clone())
    });
    assert!(e.ok, "{:?}", e.error);
    let doc: Value = serde_json::from_slice(&std::fs::read(&st).unwrap()).unwrap();
    assert_eq!(doc["format"], "ferraus-plugin-state");
    let loaded = c.ok("plugin state load", &[("name", "Ferraus Test Gain"), ("state", &s)], &[]);
    assert_eq!(loaded["params"][0]["value"], -12.0);
    let e = c.run_with("plugin run", &[("plugin", "Ferraus Test Gain"), ("state", &s)], &[&input], |g| {
        g.output = Some(o.clone())
    });
    assert!(e.ok);
    let ratio = read_wav(&out).peak() / tone().peak();
    assert!((ratio - 0.251).abs() < 0.003, "{ratio}");
    // preset by name
    let e = c.run_with(
        "plugin run",
        &[("plugin", "Ferraus Test Gain"), ("preset", "Loud"), ("param", "gain=0dB")],
        &[&input],
        |g| g.output = Some(o.clone()),
    );
    assert!(e.ok);
    assert_eq!(read_wav(&out).data, read_wav(&input).data, "explicit params override the preset");

    // sounds registry
    let info = c.ok("sounds info", &[("id", "gm-basic")], &[]);
    assert!(info["license_text"].as_str().unwrap().contains("GeneralUser"));
    assert_eq!(info["status"], "verified");
    assert_eq!(info["installed"], false);
    let search = c.ok("sounds search", &[("query", "piano")], &[]);
    assert_eq!(search["results"][0]["id"], "salamander-grand-piano");
    let search = c.ok("sounds search", &[("query", ""), ("type", "ir"), ("license", "CC0")], &[]);
    assert!(search["count"].as_u64().unwrap() >= 2);
    let info = c.ok("sounds info", &[("id", "piano")], &[]);
    assert_eq!(info["id"], "salamander-grand-piano", "registry alias suggestions resolve");
    let e = c.run("plugin run", &[], &[&input]);
    assert_eq!(e.error.unwrap().exit_code, 2);
    let e = c.run("sounds install", &[("id", "salamander-grand-piano")], &[]);
    assert_eq!(e.error.unwrap().code, "sound_unverified");
    let e = c.run("sounds install", &[("id", "vital")], &[]);
    let err = e.error.unwrap();
    assert_eq!(err.code, "vendor_install_required");
    assert!(err.hint.unwrap().contains("--from"));
    let e = c.run("sounds install", &[("id", "gm-basic")], &[]);
    assert_eq!(e.error.unwrap().code, "download_needs_consent");
    let e = c.run_with("sounds install", &[("id", "gm-basic")], &[], |g| {
        g.yes = true;
        g.offline = true;
    });
    assert_eq!(e.error.unwrap().exit_code, 10);
    let e = c.run_with("sounds install", &[("id", "gm-basic")], &[], |g| g.dry_run = true);
    assert!(e.ok && e.result["installed"].is_null());

    // install --from, list, alias, remove
    let sf2 = folder.join("Warm Strings.sf2");
    let r = c.ok("sounds install", &[("from", &sf2.display().to_string())], &[]);
    assert_eq!(r["installed"]["id"], "warm-strings");
    let list = c.ok("sounds list", &[], &[]);
    assert!(list["installed"].as_array().unwrap().iter().any(|r| r["id"] == "warm-strings"));
    let a = c.ok(
        "sounds alias",
        &[("assignment", "strings=warm-strings"), ("assignment", "piano=salamander-grand-piano")],
        &[],
    );
    assert_eq!(a["aliases"]["strings"], "warm-strings");
    let toml = std::fs::read_to_string(d.path("home/config/sounds/aliases.toml")).unwrap();
    assert!(toml.contains("[aliases]") && toml.contains("piano = \"salamander-grand-piano\""));
    let a = c.ok("sounds alias", &[("remove", "piano")], &[]);
    assert!(a["aliases"].get("piano").is_none());
    let r = c.ok("sounds remove", &[("id", "warm-strings")], &[]);
    assert_eq!(r["removed"].as_array().unwrap().len(), 1);

    // folder listing with categories, and a watched scan
    let fl = folder.display().to_string();
    let l = c.ok("sounds list", &[("path", &fl)], &[]);
    let cats: Vec<(&str, &str)> = l["sounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["category"].as_str().unwrap_or("")))
        .collect();
    assert!(cats.contains(&("Felt Piano", "keys")) && cats.contains(&("Warm Strings", "strings")), "{cats:?}");
    let sc = c.ok("sounds scan", &[("dir", &fl), ("watch", "true"), ("interval", "50ms"), ("watch-cycles", "2")], &[]);
    assert!(Path::new(sc["index"].as_str().unwrap()).is_file());
    assert_eq!(sc["count"], l["count"]);
}
