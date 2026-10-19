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
            let status = Command::new(cargo)
                .args(["build", "--quiet", "-p", "ferraus-plughost", "-p", "ferraus-plughost-vst3", "-p", "ferraus-test-plugin"])
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
