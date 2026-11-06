//! `ferraus update` against a local stand-in for the GitHub releases page.

use ferraus_core::testing::TempDir;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Serve `routes` (path → body) over HTTP on a free port until the test process exits.
fn serve(routes: Vec<(String, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut line = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            // drain the headers
            let mut h = String::new();
            while reader.read_line(&mut h).map(|n| n > 2).unwrap_or(false) {
                h.clear();
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let resp = match routes.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
            };
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    base
}

fn manifest(version: &str) -> String {
    json!({
        "announcement_tag": format!("v{version}"),
        "announcement_title": format!("{version} - test"),
        "announcement_changelog": "### Added\n- something new",
        "releases": [
            {"app_name": "ferraus", "app_version": version},
            {"app_name": "ferraus-plughost-vst3", "app_version": version},
            {"app_name": "ferraus-engrave", "app_version": version}
        ]
    })
    .to_string()
}

fn exe_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_ferraus")).canonicalize().unwrap().parent().unwrap().to_path_buf()
}

/// Run `ferraus <args> --json` with releases from `base` and receipts under `config`.
fn run(base: &str, config: &Path, args: &[&str]) -> (i32, Value) {
    let home = config.join("home");
    let out = Command::new(env!("CARGO_BIN_EXE_ferraus"))
        .args(args)
        .arg("--json")
        .env("FERRAUS_UPDATE_BASE", base)
        .env("XDG_CONFIG_HOME", config)
        .env("FERRAUS_HOME", &home)
        .env("FERRAUS_CONFIG", home.join("config.toml"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(text.trim()).unwrap_or_else(|e| {
        panic!("bad JSON from {args:?}: {e}\nstdout: {text}\nstderr: {}", String::from_utf8_lossy(&out.stderr))
    });
    (out.status.code().unwrap_or(-1), v)
}

fn receipt(config: &Path, app: &str, prefix: &Path, version: &str) {
    let dir = config.join(app);
    std::fs::create_dir_all(&dir).unwrap();
    let r = json!({"binaries": [app], "install_prefix": prefix.display().to_string(), "modify_path": true,
        "source": {"app_name": app, "owner": "djynnius"}, "version": version});
    std::fs::write(dir.join(format!("{app}-receipt.json")), r.to_string()).unwrap();
}

#[test]
fn check_reports_a_newer_release_and_how_to_get_it() {
    let base = serve(vec![("/latest/download/dist-manifest.json".into(), manifest("9.9.9"))]);
    let d = TempDir::new("update-check");
    let (code, v) = run(&base, &d.0, &["update", "--check"]);
    assert_eq!(code, 0, "{v}");
    let r = &v["result"];
    assert_eq!(r["current"], env!("CARGO_PKG_VERSION"));
    assert_eq!(r["target"], "9.9.9");
    assert_eq!(r["update_available"], true);
    assert_eq!(r["action"], "none");
    assert_eq!(r["release"]["notes"], "### Added\n- something new");
    assert_eq!(r["release"]["url"], format!("{base}/tag/v9.9.9"));
    // a test binary in target/ was not installed by any installer or package manager
    assert_eq!(r["install"]["method"], "unknown");
    assert!(r["command"].as_str().unwrap().contains("/download/v9.9.9/ferraus-installer."), "{r}");
    assert!(r["text"].as_str().unwrap().contains("9.9.9 is available"));
}

#[test]
fn check_says_up_to_date_and_the_old_name_still_works() {
    let current = env!("CARGO_PKG_VERSION");
    let base = serve(vec![("/latest/download/dist-manifest.json".into(), manifest(current))]);
    let d = TempDir::new("update-current");
    for cmd in ["update", "self-update"] {
        let (code, v) = run(&base, &d.0, &[cmd, "--check"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["result"]["update_available"], false);
        assert!(v["result"]["text"].as_str().unwrap().contains("up to date"));
    }
    // without --check, an up-to-date copy is left alone
    let (code, v) = run(&base, &d.0, &["update"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["action"], "none");
}

#[test]
fn a_copy_the_installer_did_not_place_is_left_to_its_owner() {
    let base = serve(vec![("/latest/download/dist-manifest.json".into(), manifest("9.9.9"))]);
    let d = TempDir::new("update-managed");
    // a receipt for another folder does not make this binary an installer copy
    receipt(&d.0, "ferraus", &d.0.join("elsewhere"), "0.0.1");
    let (code, v) = run(&base, &d.0, &["update"]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error"]["code"], "managed_install");
    assert!(v["error"]["hint"].as_str().unwrap().contains("/download/v9.9.9/ferraus-installer."), "{v}");
}

#[test]
fn dry_run_plans_the_pinned_installers_for_an_installer_copy() {
    let base = serve(vec![
        ("/latest/download/dist-manifest.json".into(), manifest("9.9.9")),
        ("/download/v9.9.8/dist-manifest.json".into(), manifest("9.9.8")),
    ]);
    let d = TempDir::new("update-plan");
    let dir = exe_dir();
    receipt(&d.0, "ferraus", &dir, "0.0.1");
    receipt(&d.0, "ferraus-plughost-vst3", &dir, "0.0.1");
    let (code, v) = run(&base, &d.0, &["update", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    let r = &v["result"];
    assert_eq!(r["install"]["method"], "installer");
    assert_eq!(r["action"], "would_update");
    let plan = r["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2, "{r}");
    assert_eq!(plan[0]["app"], "ferraus");
    assert_eq!(plan[0]["to"], "9.9.9");
    assert_eq!(plan[0]["env"]["FERRAUS_INSTALL_DIR"], dir.display().to_string());
    assert!(plan[0]["installer"].as_str().unwrap().starts_with(&format!("{base}/download/v9.9.9/ferraus-installer.")));
    assert_eq!(plan[1]["app"], "ferraus-plughost-vst3");
    assert!(plan[1]["env"].get("FERRAUS_PLUGHOST_VST3_INSTALL_DIR").is_some());

    // --version pins the release (here one older than the latest)
    let (code, v) = run(&base, &d.0, &["update", "--version", "9.9.8", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["target"], "9.9.8");
    assert_eq!(v["result"]["pinned"], true);
}

#[test]
fn errors_name_the_cause() {
    let base = serve(vec![("/latest/download/dist-manifest.json".into(), "<html>not json</html>".into())]);
    let d = TempDir::new("update-errors");
    let (code, v) = run(&base, &d.0, &["update", "--check"]);
    assert_eq!(code, 10, "{v}");
    assert_eq!(v["error"]["code"], "bad_release_manifest");
    let (code, v) = run(&base, &d.0, &["update", "--version", "1.2.3"]);
    assert_eq!(code, 2, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("no ferraus release 1.2.3"), "{v}");
    let (code, v) = run(&base, &d.0, &["update", "--version", "latest"]);
    assert_eq!(code, 2, "{v}");
    let (code, v) = run(&base, &d.0, &["update", "--check", "--offline"]);
    assert_eq!(code, 10, "{v}");
    assert_eq!(v["error"]["code"], "offline");
}
