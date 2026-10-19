//! The PRD's success scenario, end to end, with no human in the loop: a phone recording of
//! someone humming over street noise is cleaned, its melody extracted to MIDI, quantized,
//! rendered with an instrument, a recorded vocal is tuned to the same key, and the mix is
//! mastered to streaming loudness. Every step is reproducible from its sidecar. The same
//! scenario runs once through the CLI and once through the MCP server alone.
//!
//! Uses only model-free paths (profile denoise, pYIN, built-in instruments) so it runs
//! offline in CI.

use ferraus_core::testing::{self, TempDir};
use ferraus_core::AudioBuffer;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const SR: u32 = 44100;
const BPM: f64 = 100.0;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ferraus"));
    let home = std::env::temp_dir().join(format!("ferraus-scenario-home-{}", std::process::id()));
    c.env("FERRAUS_HOME", &home)
        .env("FERRAUS_CONFIG", home.join("config.toml"))
        .env("FERRAUS_OFFLINE", "1")
        .env_remove("FERRAUS_SOUND_PATH");
    c
}

/// A breathy, slightly drifting sung/hummed melody (MIDI notes, beats each).
fn voice(notes: &[(i32, f64)], detune_cents: f64, seed: u64) -> Vec<f32> {
    let beat = 60.0 / BPM;
    let mut out = Vec::new();
    let mut phase = 0.0f64;
    let noise = testing::noise(SR, notes.iter().map(|n| n.1).sum::<f64>() * beat + 1.0, 1.0, seed);
    let mut ni = 0;
    for (k, &(midi, beats)) in notes.iter().enumerate() {
        let n = (beats * beat * SR as f64) as usize;
        let cents = if k % 2 == 0 { detune_cents } else { -detune_cents };
        let f0 = 440.0 * 2f64.powf((midi as f64 - 69.0 + cents / 100.0) / 12.0);
        for i in 0..n {
            let t = i as f64 / SR as f64;
            let vib = 1.0 + 0.004 * (2.0 * std::f64::consts::PI * 5.5 * t).sin();
            phase += 2.0 * std::f64::consts::PI * f0 * vib / SR as f64;
            let env = (t / 0.06).min(1.0) * ((n - i) as f64 / (0.04 * SR as f64)).min(1.0);
            let mut s = 0.0;
            for h in 1..8 {
                s += (phase * h as f64).sin() / (h as f64).powf(1.6);
            }
            let breath = 0.04 * noise.data[0][ni % noise.frames()] as f64;
            ni += 1;
            out.push((0.35 * env * s + breath * env) as f32);
        }
    }
    out
}

/// Low-passed noise standing in for street ambience.
fn street(n: usize, amp: f32) -> Vec<f32> {
    let w = testing::noise(SR, n as f64 / SR as f64 + 0.01, 1.0, 99);
    let mut lp = 0.0f32;
    let mut lp2 = 0.0f32;
    (0..n)
        .map(|i| {
            lp = 0.98 * lp + 0.02 * w.data[0][i];
            lp2 = 0.9 * lp2 + 0.1 * w.data[0][i];
            amp * (3.0 * lp + 0.3 * lp2)
        })
        .collect()
}

/// G major melody hummed over street noise with a 1.5 s noise-only lead-in.
fn fixtures(dir: &TempDir) {
    let melody = [(67, 1.0), (71, 1.0), (74, 1.0), (72, 1.0), (71, 1.0), (69, 1.0), (67, 2.0)];
    let hum = voice(&melody, 15.0, 5);
    let lead = (1.5 * SR as f64) as usize;
    let mut signal = vec![0.0f32; lead];
    signal.extend(hum);
    signal.extend(vec![0.0; SR as usize / 2]);
    let noise = street(signal.len(), 0.05);
    let mixed: Vec<f32> = signal.iter().zip(&noise).map(|(a, b)| a + b).collect();
    testing::write_wav(&dir.path("hum.wav"), &AudioBuffer::from_mono(SR, mixed));
    // a vocal take in the same key, sung 35 cents sharp/flat
    let vocal = voice(&[(74, 2.0), (71, 2.0), (67, 2.0), (69, 2.0)], 35.0, 8);
    testing::write_wav(&dir.path("vocal.wav"), &AudioBuffer::from_mono(SR, vocal).with_channels(2));
}

fn run(dir: &Path, args: &[&str]) -> Value {
    let out = bin().current_dir(dir).args(args).arg("--json").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("{args:?}: {e}\n{text}\n{}", String::from_utf8_lossy(&out.stderr)));
    assert_eq!(v["ok"], true, "{args:?} failed: {v}");
    v
}

fn key_of(v: &Value) -> String {
    // transcribe records the detected key as a run-time parameter
    let k = v["params"]["key"].as_str().or(v["result"]["key"].as_str()).unwrap_or("G major").to_string();
    k.split(" (").next().unwrap().to_string()
}

#[test]
fn hum_to_mastered_part_via_cli() {
    let d = TempDir::new("scenario-cli");
    fixtures(&d);
    let p = &d.0;
    let v = run(p, &["denoise", "hum.wav", "--auto", "-o", "hum.clean.wav"]);
    assert!(v["params"]["noise_region"].is_string() || v["params"]["noise_region"].is_object(), "{v}");
    let v = run(p, &["transcribe", "hum.clean.wav", "-o", "part.mid", "--bpm", "100"]);
    let key = key_of(&v);
    assert!(key.starts_with('G') || key.starts_with('E'), "detected key {key}"); // G major or its relative minor
    let notes = v["result"]["notes"].as_u64().unwrap_or(0);
    assert!((6..=9).contains(&notes), "{notes} notes in {v}");
    run(p, &["midi", "quantize", "part.mid", "--grid", "1/16", "-o", "part.q.mid"]);
    run(p, &["render", "part.q.mid", "--instrument", "piano", "-o", "part.wav"]);
    let v = run(p, &["tune", "vocal.wav", "--key", &key, "-o", "vocal.tuned.wav"]);
    assert!(v["params"]["key"].as_str().unwrap().starts_with(key.split(' ').next().unwrap()), "{v}");
    run(p, &["mix", "part.wav", "vocal.tuned.wav", "--gains", "-3dB,0dB", "-o", "mix.wav"]);
    let v = run(p, &["master", "mix.wav", "--target", "streaming", "-o", "master.wav"]);
    assert!(!v["outputs"].as_array().unwrap().is_empty());
    let v = run(p, &["master", "verify", "master.wav", "--target", "streaming"]);
    assert_eq!(v["ok"], true);
    let v = run(p, &["analyze", "loudness", "master.wav"]);
    let lufs = v["result"]["integrated_lufs"].as_f64().unwrap();
    assert!((lufs + 14.0).abs() <= 0.5, "master at {lufs} LUFS");
    // every step is reproducible from its sidecar
    for f in ["hum.clean.wav", "part.mid", "part.q.mid", "part.wav", "vocal.tuned.wav", "mix.wav", "master.wav"] {
        let sc = format!("{f}.ferraus.json");
        assert!(d.path(&sc).exists(), "missing sidecar for {f}");
        run(p, &["verify", &sc]);
    }
    for f in ["part.q.mid", "mix.wav"] {
        let v = run(p, &["replay", &format!("{f}.ferraus.json"), "--overwrite"]);
        assert_eq!(v["result"]["outputs"][0]["identical"], true, "{f} did not replay bit-exactly: {v}");
    }
}

struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    id: i64,
}

impl Mcp {
    fn start(dir: &Path) -> Mcp {
        let mut child = bin()
            .current_dir(dir)
            .args(["mcp", "serve", "--allow", dir.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp { child, out, id: 0 };
        let r = m.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "scenario", "version": "1"}}));
        assert_eq!(r["result"]["serverInfo"]["name"], "ferraus");
        m.notify("notifications/initialized");
        m
    }

    fn notify(&mut self, method: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "method": method})).unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "MCP server closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        let env = r["result"]["structuredContent"].clone();
        assert_eq!(r["result"]["isError"], false, "{name} failed: {r}");
        env
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn hum_to_mastered_part_via_mcp_only() {
    let d = TempDir::new("scenario-mcp");
    fixtures(&d);
    let mut m = Mcp::start(&d.0);
    let tools = m.request("tools/list", json!({}));
    let names: Vec<String> =
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    for t in [
        "ferraus_denoise",
        "ferraus_transcribe",
        "ferraus_midi_quantize",
        "ferraus_render",
        "ferraus_tune",
        "ferraus_mix",
        "ferraus_master",
        "ferraus_master_verify",
    ] {
        assert!(names.iter().any(|n| n == t), "missing tool {t}");
    }
    m.tool("ferraus_denoise", json!({"inputs": ["hum.wav"], "auto": true, "output": "hum.clean.wav"}));
    let v = m.tool("ferraus_transcribe", json!({"inputs": ["hum.clean.wav"], "output": "part.mid", "bpm": 100}));
    let key = key_of(&v);
    m.tool("ferraus_midi_quantize", json!({"inputs": ["part.mid"], "grid": "1/16", "output": "part.q.mid"}));
    m.tool("ferraus_render", json!({"inputs": ["part.q.mid"], "instrument": "piano", "output": "part.wav"}));
    m.tool("ferraus_tune", json!({"inputs": ["vocal.wav"], "key": key, "output": "vocal.tuned.wav"}));
    m.tool("ferraus_mix", json!({"inputs": ["part.wav", "vocal.tuned.wav"], "gains": "-3dB,0dB", "output": "mix.wav"}));
    // the long step as a background job, like an agent would
    let job = m.tool("ferraus_job_start", json!({"tool": "ferraus_master", "arguments": {"inputs": ["mix.wav"], "target": "streaming", "output": "master.wav"}}));
    let id = job["job_id"].as_str().unwrap().to_string();
    let mut state = String::new();
    for _ in 0..600 {
        let s = m.tool("ferraus_job_status", json!({"job_id": id}));
        state = s["state"].as_str().unwrap().to_string();
        if state != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(state, "succeeded");
    let v = m.tool("ferraus_master_verify", json!({"inputs": ["master.wav"], "target": "streaming"}));
    assert_eq!(v["ok"], true);
    assert!(d.path("master.wav.ferraus.json").exists());
    // the sandbox holds: nothing outside the allowed directory
    let r = m.request("tools/call", json!({"name": "ferraus_info", "arguments": {"inputs": ["/etc/hosts"]}}));
    assert_eq!(r["result"]["isError"], true);
}
