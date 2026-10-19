//! `accelerate install` / `accelerate status`: optional GPU execution-provider libraries.
//!
//! The base package statically links the CPU ONNX Runtime (with CoreML on macOS and DirectML
//! on Windows, which ship inside the platform runtime). CUDA support needs the ONNX Runtime
//! CUDA provider libraries plus NVIDIA's CUDA and cuDNN runtimes, which are proprietary
//! (redistributable under the NVIDIA EULA) and therefore never in the base package:
//! `accelerate install` downloads Microsoft's official GPU build of the matching ONNX Runtime
//! release into `<cache>/providers/`, checks the archive and the provider libraries it must
//! contain, and records the archive SHA-256 in `<cache>/providers/installed.json`.

use crate::provider::Provider;
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// ONNX Runtime release linked by this build (`ort` 2.0.0-rc.14).
pub const ORT_VERSION: &str = "1.31.0";

pub const NVIDIA_EULA_NOTICE: &str = "The CUDA execution provider needs NVIDIA CUDA 13 and cuDNN 9 runtime libraries. They are proprietary software licensed under the NVIDIA Software License Agreement and the CUDA Toolkit EULA (https://docs.nvidia.com/cuda/eula/), redistributable only under those terms. ferraus never bundles them; installing them means you accept NVIDIA's terms.";

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom(
            "accelerate install",
            Category::Models,
            "Install GPU execution-provider libraries for ONNX Runtime",
            install,
        )
        .inputs(Inputs::NONE)
        .about("Downloads the ONNX Runtime GPU execution-provider libraries that match this build into the user cache and verifies them. On macOS CoreML is built in and on Windows DirectML is built in, so nothing is downloaded for them. The CUDA provider requires NVIDIA's CUDA and cuDNN runtimes, which are covered by the NVIDIA EULA; pass --accept-eula (or -y) to confirm you accept it.")
        .param(param("provider", ParamKind::Choice).choices(&["auto", "cuda", "coreml", "directml"]).default("auto").help("Which provider to install (auto: CUDA on Linux/Windows, report CoreML on macOS)"))
        .param(param("accept-eula", ParamKind::Flag).help("Accept the NVIDIA EULA for the CUDA runtime libraries"))
        .example("ferraus accelerate install --accept-eula", "Fetch the CUDA provider on Linux or Windows")
        .example("ferraus accelerate status", "Show which provider is active"),
        OpSpec::custom(
            "accelerate status",
            Category::Models,
            "Show available execution providers and installed GPU libraries",
            status,
        )
        .inputs(Inputs::NONE)
        .example("ferraus accelerate status --json", "Machine-readable provider report"),
    ]
}

/// Where provider libraries live.
pub fn providers_dir() -> PathBuf {
    ferraus_core::paths::cache_dir().join("providers")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Installed {
    #[serde(default)]
    pub packages: Vec<InstalledPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub provider: String,
    pub onnxruntime: String,
    pub archive_url: String,
    pub archive_sha256: String,
    pub dir: String,
    pub libraries: Vec<String>,
    pub installed_at: u64,
}

pub fn load_installed(dir: &Path) -> Installed {
    std::fs::read(dir.join("installed.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The GPU package for this platform: (archive file name, libraries it must contain).
pub fn cuda_package(os: &str, arch: &str) -> Option<(String, Vec<&'static str>)> {
    match (os, arch) {
        ("linux", "x86_64") => Some((
            format!("onnxruntime-linux-x64-gpu-{ORT_VERSION}.tgz"),
            vec![
                "libonnxruntime_providers_cuda.so",
                "libonnxruntime_providers_shared.so",
            ],
        )),
        ("windows", "x86_64") => Some((
            format!("onnxruntime-win-x64-gpu-{ORT_VERSION}.zip"),
            vec![
                "onnxruntime_providers_cuda.dll",
                "onnxruntime_providers_shared.dll",
            ],
        )),
        _ => None,
    }
}

pub fn cuda_package_url(file: &str) -> String {
    format!("https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{file}")
}

/// Directories searched for the NVIDIA runtime libraries.
fn cuda_runtime_found() -> Value {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["LD_LIBRARY_PATH", "PATH", "CUDA_PATH"] {
        if let Some(v) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&v));
        }
    }
    for d in [
        "/usr/local/cuda/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib64",
        "/usr/lib/aarch64-linux-gnu",
    ] {
        dirs.push(PathBuf::from(d));
    }
    let find = |prefix: &str| -> Option<String> {
        for d in &dirs {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.starts_with(prefix) {
                        return Some(e.path().display().to_string());
                    }
                }
            }
        }
        None
    };
    json!({
        "cudart": find("libcudart.so").or_else(|| find("cudart64_")),
        "cudnn": find("libcudnn.so").or_else(|| find("cudnn64_")),
        "cublas": find("libcublas.so").or_else(|| find("cublas64_")),
    })
}

fn provider_available(p: Provider) -> bool {
    use ort::ep::ExecutionProvider;
    if !p.compiled() {
        return false;
    }
    let r = match p {
        Provider::Cpu => Ok(true),
        #[cfg(feature = "cuda")]
        Provider::Cuda => ort::ep::CUDA::is_available(),
        #[cfg(any(feature = "coreml", target_os = "macos"))]
        Provider::CoreMl => ort::ep::CoreML::is_available(),
        #[cfg(any(feature = "directml", windows))]
        Provider::DirectMl => ort::ep::DirectML::is_available(),
        #[allow(unreachable_patterns)]
        _ => Ok(false),
    };
    r.unwrap_or(false)
}

/// The provider report used by `doctor` and `accelerate status`.
pub fn report() -> Value {
    let providers: Vec<Value> = crate::provider::AUTO_ORDER
        .iter()
        .map(|p| {
            json!({
                "name": p.name(),
                "compiled": p.compiled(),
                "available": provider_available(*p),
            })
        })
        .collect();
    let available: Vec<&str> = crate::provider::AUTO_ORDER
        .iter()
        .filter(|p| provider_available(**p))
        .map(|p| p.name())
        .collect();
    let active = available.first().copied().unwrap_or("cpu");
    let dir = providers_dir();
    let installed = load_installed(&dir);
    let gpu_present = installed.packages.iter().any(|p| {
        p.libraries
            .iter()
            .all(|l| Path::new(&p.dir).join(l).exists())
    });
    json!({
        "providers": available,
        "active": active,
        "provider_details": providers,
        "selection_order": crate::provider::AUTO_ORDER.iter().map(|p| p.name()).collect::<Vec<_>>(),
        "onnxruntime": ORT_VERSION,
        "gpu_libs": {
            "dir": dir,
            "installed": gpu_present,
            "packages": installed.packages,
            "nvidia_runtime": if cfg!(any(target_os = "linux", windows)) { cuda_runtime_found() } else { Value::Null },
        },
        "built_in": if cfg!(target_os = "macos") { json!("coreml") } else if cfg!(windows) { json!("directml") } else { Value::Null },
    })
}

fn status(_a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    Ok(report())
}

fn install(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let want = a.choice("provider");
    let want = if want.is_empty() { "auto".to_string() } else { want };
    if os == "macos" && (want == "auto" || want == "coreml") {
        return Ok(json!({
            "provider": "coreml",
            "status": "built_in",
            "message": "CoreML is built into the macOS ONNX Runtime used by ferraus; nothing to download. ferraus selects it automatically (use --device cpu to opt out).",
            "available": provider_available(Provider::CoreMl),
        }));
    }
    if want == "coreml" {
        return Err(Error::unsupported("the CoreML provider exists only on macOS"));
    }
    if want == "directml" {
        if os == "windows" {
            return Ok(json!({
                "provider": "directml",
                "status": "built_in",
                "message": "DirectML is built into the Windows ONNX Runtime used by ferraus; nothing to download.",
                "available": provider_available(Provider::DirectMl),
            }));
        }
        return Err(Error::unsupported("the DirectML provider exists only on Windows"));
    }
    // CUDA
    let Some((file, libs)) = cuda_package(os, arch) else {
        return Err(Error::unsupported(format!(
            "no ONNX Runtime CUDA package is published for {os}/{arch}; ferraus runs on CPU here"
        )));
    };
    ctx.warn("nvidia_eula", NVIDIA_EULA_NOTICE);
    if !(a.flag("accept-eula") || ctx.globals.yes) {
        return Err(Error::usage(
            "installing the CUDA provider requires accepting the NVIDIA EULA (see the warning above)",
        )
        .with_hint("ferraus accelerate install --accept-eula"));
    }
    let url = cuda_package_url(&file);
    let dir = providers_dir();
    let pkg_dir = dir.join(format!("onnxruntime-{ORT_VERSION}-cuda"));
    if ctx.globals.dry_run {
        return Ok(json!({"provider": "cuda", "would_download": url, "into": pkg_dir}));
    }
    if ferraus_models::offline(ctx) {
        return Err(Error::new(
            ErrorKind::Network,
            "offline",
            "--offline is set; cannot download execution providers",
        ));
    }
    std::fs::create_dir_all(&dir)?;
    let archive = dir.join(&file);
    let progress = ctx.progress;
    let cancel = ctx.cancel.clone();
    let mut log = |l: &str| progress.log(l);
    let check = move || cancel.check();
    let urls = vec![url.clone()];
    let rep = ferraus_models::download::download(ferraus_models::download::DownloadRequest {
        urls: &urls,
        sha256: None,
        dest: &archive,
        log: &mut log,
        check_cancel: &check,
    })?;
    let _ = std::fs::remove_dir_all(&pkg_dir);
    std::fs::create_dir_all(&pkg_dir)?;
    let extracted = extract(&archive, &pkg_dir, &libs).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&pkg_dir);
    })?;
    let _ = std::fs::remove_file(&archive);
    let missing: Vec<&str> = libs
        .iter()
        .filter(|l| !pkg_dir.join(l).exists())
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(Error::verification(format!(
            "{file} did not contain {}",
            missing.join(", ")
        )));
    }
    let mut installed = load_installed(&dir);
    installed.packages.retain(|p| p.provider != "cuda");
    installed.packages.push(InstalledPackage {
        provider: "cuda".into(),
        onnxruntime: ORT_VERSION.into(),
        archive_url: url.clone(),
        archive_sha256: rep.sha256.clone(),
        dir: pkg_dir.display().to_string(),
        libraries: libs.iter().map(|s| s.to_string()).collect(),
        installed_at: ferraus_models::cache::now(),
    });
    std::fs::write(dir.join("installed.json"), serde_json::to_vec_pretty(&installed)?)?;
    let runtime = cuda_runtime_found();
    if runtime.get("cudnn").map(|v| v.is_null()).unwrap_or(true) {
        ctx.warn(
            "cuda_runtime_missing",
            "NVIDIA CUDA/cuDNN runtime libraries were not found on this system; install CUDA 13 and cuDNN 9 from NVIDIA (or the nvidia-*-cu13 wheels) and add them to the library path",
        );
    }
    Ok(json!({
        "provider": "cuda",
        "status": "installed",
        "dir": pkg_dir,
        "archive_sha256": rep.sha256,
        "files": extracted,
        "nvidia_runtime": runtime,
        "note": "CUDA inference needs a ferraus build with the `cuda` feature (the CUDA-enabled ONNX Runtime); the default package is CPU-only.",
    }))
}

/// Extract only the provider libraries (flattened) from a `.tgz` or `.zip`.
fn extract(archive: &Path, into: &Path, libs: &[&str]) -> Result<Vec<String>> {
    let name = archive.to_string_lossy().to_string();
    let mut out = Vec::new();
    if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        let f = std::fs::File::open(archive)?;
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
        for entry in ar
            .entries()
            .map_err(|e| Error::verification(format!("corrupt archive: {e}")))?
        {
            let mut entry = entry.map_err(|e| Error::verification(format!("corrupt archive: {e}")))?;
            let path = entry
                .path()
                .map_err(|e| Error::verification(e.to_string()))?
                .to_path_buf();
            let Some(fname) = path.file_name().map(|f| f.to_string_lossy().to_string()) else {
                continue;
            };
            if libs.iter().any(|l| fname == *l) {
                let dest = into.join(&fname);
                let mut w = std::fs::File::create(&dest)?;
                std::io::copy(&mut entry, &mut w)?;
                out.push(fname);
            }
        }
        return Ok(out);
    }
    #[cfg(windows)]
    if name.ends_with(".zip") {
        let f = std::fs::File::open(archive)?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| Error::verification(format!("corrupt archive: {e}")))?;
        for i in 0..z.len() {
            let mut e = z.by_index(i).map_err(|e| Error::verification(e.to_string()))?;
            let fname = Path::new(e.name())
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            if libs.iter().any(|l| fname == *l) {
                let mut w = std::fs::File::create(into.join(&fname))?;
                std::io::copy(&mut e, &mut w)?;
                out.push(fname);
            }
        }
        return Ok(out);
    }
    Err(Error::unsupported(format!("unknown archive type: {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_per_platform() {
        assert!(cuda_package("linux", "x86_64").unwrap().0.contains("linux-x64-gpu"));
        assert!(cuda_package("windows", "x86_64").unwrap().0.ends_with(".zip"));
        assert!(cuda_package("macos", "aarch64").is_none());
        assert!(cuda_package("linux", "aarch64").is_none());
        assert!(cuda_package_url("x.tgz").starts_with("https://github.com/microsoft/onnxruntime/releases/download/v"));
    }

    #[test]
    fn report_has_cpu() {
        let r = report();
        assert!(r["providers"].as_array().unwrap().iter().any(|p| p == "cpu"));
        assert!(r["active"].is_string());
    }

    #[test]
    fn tgz_extraction_keeps_only_libraries() {
        let dir = ferraus_core::testing::TempDir::new("accel-tgz");
        let archive = dir.path("pkg.tgz");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            for (name, data) in [
                ("onnxruntime-x/lib/libonnxruntime_providers_cuda.so", b"cuda".as_slice()),
                ("onnxruntime-x/lib/libonnxruntime_providers_shared.so", b"shared"),
                ("onnxruntime-x/README.md", b"readme"),
            ] {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, name, data).unwrap();
            }
            b.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path("out");
        std::fs::create_dir_all(&out).unwrap();
        let libs = ["libonnxruntime_providers_cuda.so", "libonnxruntime_providers_shared.so"];
        let got = extract(&archive, &out, &libs).unwrap();
        assert_eq!(got.len(), 2);
        assert!(out.join("libonnxruntime_providers_cuda.so").exists());
        assert!(!out.join("README.md").exists());
    }

    #[test]
    fn macos_reports_built_in_coreml() {
        if std::env::consts::OS != "macos" {
            return;
        }
        let spec = &ops()[0];
        let a = ferraus_core::testing::args(spec, &[]);
        let mut ctx = ferraus_core::testing::ctx();
        let v = install(&a, &mut ctx).unwrap();
        assert_eq!(v["status"], "built_in");
        assert_eq!(v["provider"], "coreml");
    }
}
