//! End-to-end tests of the real binary: envelopes, exit codes, sidecars, replay/verify,
//! pipelines with caching, stdin/stdout piping, batches, dry runs, and help as JSON.

use ferraus_core::testing::{self, TempDir};
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ferraus"));
    // keep tests away from the user's config, cache, and data
    let home = std::env::temp_dir().join(format!("ferraus-cli-test-home-{}", std::process::id()));
    c.env("FERRAUS_HOME", &home).env("FERRAUS_CONFIG", home.join("config.toml")).env_remove("FERRAUS_SOUND_PATH");
    c
}

fn run_json(dir: &Path, args: &[&str]) -> (i32, Value) {
    let out = bin().current_dir(dir).args(args).arg("--json").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("bad JSON from {args:?}: {e}\nstdout: {text}\nstderr: {}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code().unwrap_or(-1), v)
}

fn fixture(dir: &TempDir) {
    let tone = testing::stereo(testing::sine(44100, 440.0, 2.0, 0.5));
    testing::write_wav(&dir.path("tone.wav"), &tone);
}

#[test]
fn envelope_shape_and_sidecar() {
    let d = TempDir::new("cli-env");
    fixture(&d);
    let (code, v) = run_json(&d.0, &["gain", "tone.wav", "--by", "-6dB"]);
    assert_eq!(code, 0, "{v}");
    for k in ["ok", "command", "ferraus_version", "schema_version", "inputs", "outputs", "params", "models", "metrics", "warnings", "timing_ms"] {
        assert!(v.get(k).is_some(), "missing {k} in {v}");
    }
    assert_eq!(v["command"], "gain");
    assert_eq!(v["params"]["by"], "-6dB");
    let out = d.path("tone.gain.wav");
    assert!(out.exists());
    let sc: Value = serde_json::from_slice(&std::fs::read(d.path("tone.gain.wav.ferraus.json")).unwrap()).unwrap();
    assert_eq!(sc["command"], "gain");
    assert!(sc["outputs"][0]["sha256"].as_str().unwrap().len() == 64);
    let b = testing::read_wav(&out);
    assert!((b.peak() - 0.25).abs() < 0.01, "peak {}", b.peak());
}

#[test]
fn exit_codes() {
    let d = TempDir::new("cli-exit");
    fixture(&d);
    let (code, v) = run_json(&d.0, &["gain", "missing.wav"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], false);
    let (code, v) = run_json(&d.0, &["gain", "tone.wav", "--by", "loud"]);
    assert_eq!(code, 2, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("--by"));
    let (code, _) = run_json(&d.0, &["gain", "tone.wav", "-o", "a.wav"]);
    assert_eq!(code, 0);
    let (code, v) = run_json(&d.0, &["gain", "tone.wav", "--by", "-1dB", "-o", "a.wav"]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error"]["code"], "output_exists");
    assert!(v["error"]["hint"].as_str().unwrap().contains("--overwrite"));
    std::fs::write(d.path("junk.wav"), b"definitely not audio").unwrap();
    let (code, v) = run_json(&d.0, &["gain", "junk.wav"]);
    assert_eq!(code, 4, "{v}");
    let (code, v) = run_json(&d.0, &["nonexistent-command"]);
    assert_eq!(code, 2, "{v}");
}

#[test]
fn idempotent_reuse_force_and_overwrite() {
    let d = TempDir::new("cli-reuse");
    fixture(&d);
    let (code, _) = run_json(&d.0, &["gain", "tone.wav", "--by", "-3dB", "-o", "g.wav"]);
    assert_eq!(code, 0);
    let (code, v) = run_json(&d.0, &["gain", "tone.wav", "--by", "-3dB", "-o", "g.wav"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["reused"], true);
    let (code, v) = run_json(&d.0, &["gain", "tone.wav", "--by", "-3dB", "-o", "g.wav", "--force", "--overwrite"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["result"].is_null());
}

#[test]
fn replay_and_verify() {
    let d = TempDir::new("cli-replay");
    fixture(&d);
    let (code, _) = run_json(&d.0, &["fade", "tone.wav", "--in", "0.5s", "-o", "f.wav"]);
    assert_eq!(code, 0);
    let (code, v) = run_json(&d.0, &["verify", "f.wav.ferraus.json"]);
    assert_eq!(code, 0, "{v}");
    let (code, v) = run_json(&d.0, &["replay", "f.wav.ferraus.json", "--overwrite"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["outputs"][0]["identical"], true, "{v}");
    // tamper → verify fails with exit 9
    let mut f = std::fs::OpenOptions::new().append(true).open(d.path("f.wav")).unwrap();
    f.write_all(b"x").unwrap();
    let (code, v) = run_json(&d.0, &["verify", "f.wav.ferraus.json"]);
    assert_eq!(code, 9, "{v}");
}

#[test]
fn dry_run_writes_nothing() {
    let d = TempDir::new("cli-dry");
    fixture(&d);
    let (code, v) = run_json(&d.0, &["fx", "tone.wav", "-o", "x.wav", "--fx", "gain:by=-3dB", "--fx", "fade:in=1s", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["result"]["stages"].as_array().unwrap().len(), 2);
    assert_eq!(v["result"]["stages"][1]["args"]["curve"], "linear");
    assert!(!d.path("x.wav").exists());
}

#[test]
fn null_test_through_fx_chain() {
    let d = TempDir::new("cli-null");
    fixture(&d);
    let (code, v) = run_json(&d.0, &["fx", "tone.wav", "-o", "n.wav", "--fx", "compress:mix=0%", "--fx", "saturate:mix=0%", "--bit-depth", "32"]);
    assert_eq!(code, 0, "{v}");
    let a = testing::read_wav(&d.path("tone.wav"));
    let b = testing::read_wav(&d.path("n.wav"));
    assert_eq!(a.data, b.data);
}

#[test]
fn stdin_stdout_pipe() {
    let d = TempDir::new("cli-pipe");
    fixture(&d);
    let first = bin().current_dir(&d.0).args(["gain", "tone.wav", "--by", "-6dB", "-o", "-"]).output().unwrap();
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert!(first.stdout.starts_with(b"RIFF"));
    let mut child = bin()
        .current_dir(&d.0)
        .args(["info", "-", "--input-format", "wav", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&first.stdout).unwrap();
    let out = child.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true, "{v}");
}

#[test]
fn batch_with_outdir() {
    let d = TempDir::new("cli-batch");
    testing::write_wav(&d.path("a.wav"), &testing::sine(16000, 300.0, 0.5, 0.5));
    testing::write_wav(&d.path("b.wav"), &testing::sine(16000, 500.0, 0.5, 0.5));
    let (code, v) = run_json(&d.0, &["gain", "a.wav", "b.wav", "--by", "-6dB", "--outdir", "out"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(v["result"].as_array().unwrap().len(), 2);
    assert!(d.path("out/a.gain.wav").exists() && d.path("out/b.gain.wav").exists());
    // glob input
    let (code, v) = run_json(&d.0, &["gain", "*.wav", "--by", "-1dB", "--outdir", "out2"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["outputs"].as_array().unwrap().len(), 2);
}

#[test]
fn pipeline_runs_and_caches() {
    let d = TempDir::new("cli-pipe-cache");
    fixture(&d);
    std::fs::write(
        d.path("job.yaml"),
        "version: 1\ninputs:\n  src: tone.wav\nstages:\n  - id: quiet\n    op: gain\n    in: src\n    args: { by: -6dB }\n  - id: faded\n    op: fade\n    in: quiet\n    args: { in: 0.5s }\n  - id: chain\n    op: fx\n    in: faded\n    args:\n      chain:\n        - { op: reverse }\n        - { op: gain, by: -1dB }\noutputs:\n  - { from: chain, path: out/final.wav, bit_depth: 24 }\n  - { from: quiet, path: out/quiet.wav }\n",
    )
    .unwrap();
    let (code, v) = run_json(&d.0, &["pipeline", "run", "job.yaml"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["computed"], 3);
    assert!(d.path("out/final.wav").exists() && d.path("out/job.ferraus.json").exists());
    // change only the last stage → exactly one stage of work
    let (code, v) = run_json(&d.0, &["pipeline", "run", "job.yaml", "--set", "stages.chain.args.chain=[{\"op\":\"gain\",\"by\":\"-2dB\"}]", "--overwrite"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["computed"], 1, "{v}");
    assert_eq!(v["result"]["cached"], 2);
    let (code, v) = run_json(&d.0, &["pipeline", "validate", "job.yaml"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["order"], serde_json::json!(["quiet", "faded", "chain"]));
}

#[test]
fn help_json_and_schema() {
    let out = bin().args(["compress", "--help", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["command"], "compress");
    assert_eq!(v["arguments"]["properties"]["ratio"]["default"], "3:1");
    let d = TempDir::new("cli-schema");
    let (code, v) = run_json(&d.0, &["schema", "compress"]);
    assert_eq!(code, 0);
    assert_eq!(v["result"]["arguments"]["$schema"], "https://json-schema.org/draft/2020-12/schema");
}

#[test]
fn progress_json_events() {
    let d = TempDir::new("cli-progress");
    fixture(&d);
    let out = bin().current_dir(&d.0).args(["gain", "tone.wav", "--progress", "json"]).output().unwrap();
    let events: Vec<Value> = String::from_utf8_lossy(&out.stderr).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    assert!(events.iter().any(|e| e["event"] == "progress"));
    assert!(events.iter().any(|e| e["event"] == "output"));
}

#[test]
fn presets_and_explain() {
    let d = TempDir::new("cli-presets");
    fixture(&d);
    let (code, v) = run_json(&d.0, &["presets", "show", "compress/tighten"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["values"]["ratio"], "4:1");
    let (code, v) = run_json(&d.0, &["compress", "tone.wav", "--preset", "vocal", "--ratio", "2:1", "--explain", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["params"]["ratio"], "2:1");
    let explain = v["metrics"]["explain"].as_array().unwrap();
    assert!(explain.iter().any(|e| e["param"] == "ratio" && e["why"] == "given on the command line"));
}
