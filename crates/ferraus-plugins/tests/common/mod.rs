//! Test support: builds the plugin hosts and the test CLAP plugin (workspace members) into
//! the same target directory as this test binary, and lays out `.clap` bundles.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// `target/<profile>` of the running test binary (`target/<profile>/deps/<test>`).
pub fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn dylib_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libferraus_test_plugin.dylib"
    } else if cfg!(windows) {
        "ferraus_test_plugin.dll"
    } else {
        "libferraus_test_plugin.so"
    }
}

/// Build `ferraus-plughost`, `ferraus-plughost-vst3`, and `ferraus-test-plugin` once per
/// test binary; returns the test plugin's shared library.
pub fn ensure_built() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let pd = profile_dir();
            let target = pd.parent().unwrap().to_path_buf();
            let profile = match pd.file_name().unwrap().to_str().unwrap() {
                "debug" => "dev".to_string(),
                other => other.to_string(),
            };
            let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            // the CLAP host binary is shipped by the `ferraus` package
            let status = Command::new(&cargo)
                .args(["build", "--quiet", "-p", "ferraus", "--bin", "ferraus-plughost"])
                .arg("--manifest-path")
                .arg(&manifest)
                .arg("--target-dir")
                .arg(&target)
                .arg("--profile")
                .arg(&profile)
                .env_remove("CC")
                .env_remove("CXX")
                .env_remove("CFLAGS")
                .env_remove("CXXFLAGS")
                .env_remove("LDFLAGS")
                .status()
                .expect("run cargo build for the CLAP host");
            assert!(status.success(), "building the CLAP host failed");
            let status = Command::new(cargo)
                .args(["build", "--quiet", "-p", "ferraus-plughost-vst3", "-p", "ferraus-test-plugin"])
                .arg("--manifest-path")
                .arg(&manifest)
                .arg("--target-dir")
                .arg(&target)
                .arg("--profile")
                .arg(&profile)
                .env_remove("CC")
                .env_remove("CXX")
                .env_remove("CFLAGS")
                .env_remove("CXXFLAGS")
                .env_remove("LDFLAGS")
                .status()
                .expect("run cargo build for the plugin hosts");
            assert!(status.success(), "building the plugin hosts failed");
            let lib = pd.join(dylib_name());
            assert!(lib.is_file(), "test plugin not found at {}", lib.display());
            lib
        })
        .clone()
}

/// Copy the test plugin into `dir` as `<name>.clap` (a plain-file bundle, valid on every OS).
pub fn clap_bundle(dir: &Path, name: &str) -> PathBuf {
    let lib = ensure_built();
    std::fs::create_dir_all(dir).unwrap();
    let dst = dir.join(format!("{name}.clap"));
    std::fs::copy(&lib, &dst).unwrap();
    dst
}

/// A real VST3 bundle around the test library (which also exports a VST3 factory).
pub fn vst3_bundle(dir: &Path, name: &str) -> PathBuf {
    let lib = ensure_built();
    let b = dir.join(format!("{name}.vst3"));
    let bin = if cfg!(target_os = "macos") {
        b.join("Contents/MacOS").join(name)
    } else if cfg!(windows) {
        b.join("Contents/x86_64-win").join(format!("{name}.vst3"))
    } else {
        let arch = if std::env::consts::ARCH == "aarch64" { "aarch64" } else { "x86_64" };
        b.join(format!("Contents/{arch}-linux")).join(format!("{name}.so"))
    };
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::copy(&lib, &bin).unwrap();
    b
}

/// A fake VST3 bundle directory with a moduleinfo.json (no binary).
pub fn fake_vst3(dir: &Path, name: &str, instrument: bool) -> PathBuf {
    let b = dir.join(format!("{name}.vst3"));
    std::fs::create_dir_all(b.join("Contents/Resources")).unwrap();
    std::fs::write(
        b.join("Contents/Resources/moduleinfo.json"),
        format!(
            r#"{{"Name": "{name}", "Version": "1.0.0",
  "Factory Info": {{"Vendor": "Acme", "URL": "", "E-Mail": "", "Flags": {{}}}},
  "Classes": [{{"CID": "0123456789ABCDEF0123456789ABCDEF", "Category": "Audio Module Class",
    "Name": "{name}", "Vendor": "Acme", "Version": "1.0.0",
    "Sub Categories": ["{}"]}}]}}"#,
            if instrument { "Instrument" } else { "Fx" }
        ),
    )
    .unwrap();
    b
}

/// A tiny HTTP/1.1 file server on 127.0.0.1 for download tests. Supports `Range:
/// bytes=N-`. With `truncate_first`, the first full (non-range) response for each path
/// sends only half the body before closing. Returns the base URL.
pub fn http_server(files: Vec<(String, Vec<u8>)>, truncate_first: bool) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut truncated: Vec<String> = Vec::new();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut range: Option<u64> = None;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                let lower = h.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("range: bytes=") {
                    range = v.trim().trim_end_matches('-').parse().ok();
                }
            }
            let Some((_, body)) = files.iter().find(|(p, _)| *p == path) else {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            };
            match range {
                Some(start) if (start as usize) < body.len() => {
                    let rest = &body[start as usize..];
                    let _ = write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        rest.len(),
                        start,
                        body.len() - 1,
                        body.len()
                    );
                    let _ = stream.write_all(rest);
                }
                Some(_) => {
                    let _ = stream.write_all(
                        b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                None => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if truncate_first && !truncated.contains(&path) {
                        truncated.push(path.clone());
                        let _ = stream.write_all(&body[..body.len() / 2]);
                    } else {
                        let _ = stream.write_all(body);
                    }
                }
            }
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

/// Minimal RIFF bytes that look like an SF2 file.
pub fn fake_sf2() -> Vec<u8> {
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&4u32.to_le_bytes());
    v.extend_from_slice(b"sfbk");
    v
}

pub fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}

/// A folder with the CLAP test plugin, an SF2, and an SFZ library.
pub fn sound_folder(dir: &Path) -> PathBuf {
    let root = dir.join("my-sounds");
    clap_bundle(&root.join("plugins"), "FerrausTest");
    std::fs::write(root.join("Warm Strings.sf2"), fake_sf2()).unwrap();
    let lib = root.join("Felt Piano");
    std::fs::create_dir_all(lib.join("samples")).unwrap();
    std::fs::write(lib.join("Felt Piano.sfz"), "<region> sample=samples/c4.wav key=60\n").unwrap();
    std::fs::write(lib.join("samples/c4.wav"), b"RIFF").unwrap();
    root
}
