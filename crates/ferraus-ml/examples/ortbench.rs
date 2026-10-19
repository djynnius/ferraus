//! Time one ONNX model under different ONNX Runtime session settings (dev tool).
//!
//! `cargo run --release -p ferraus-ml --example ortbench -- <model.onnx>` with env:
//! RUNS, THREADS (intra), INTER, PAR=1, NOFOLD=1, NOMEMPAT=1, NOPREPACK=1, OPT=0..3,
//! SPIN=0/1, EP=cpu|coreml-cpu|coreml-gpu|coreml-all|coreml-ane, MLPROG=1, SAVE_OPT=<path>,
//! REF_SAVE=<file> (write the first output), REF=<file> (compare against it), CONC=<n> (n
//! concurrent sessions, each running RUNS windows), NOARENA=1 (no CPU memory arena),
//! PROFILE=<prefix> (ONNX Runtime profile JSON), HOST_STFT=1 (cut an htdemucs export's
//! STFT/iSTFT out as `separate` does; STEMS=6 for htdemucs-6s). Inputs are random tensors of
//! the declared shapes; the report gives load time, time per window, and peak RSS (macOS).

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use std::time::Instant;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn flag(k: &str) -> bool {
    env(k).map(|v| v == "1").unwrap_or(false)
}

fn build(path: &str) -> Session {
    static ENV: std::sync::OnceLock<ort::environment::Environment> = std::sync::OnceLock::new();
    let e = ENV.get_or_init(|| ort::init().with_name("bench").with_telemetry(false).build().unwrap());
    let mut b = Session::builder(e).unwrap();
    if let Some(t) = env("THREADS") {
        b = b.with_intra_threads(t.parse().unwrap()).unwrap();
    }
    if let Some(t) = env("INTER") {
        b = b.with_inter_threads(t.parse().unwrap()).unwrap();
    }
    if flag("PAR") {
        b = b.with_parallel_execution(true).unwrap();
    }
    if let Some(o) = env("OPT") {
        let l = match o.as_str() {
            "0" => GraphOptimizationLevel::Disable,
            "1" => GraphOptimizationLevel::Level1,
            "2" => GraphOptimizationLevel::Level2,
            _ => GraphOptimizationLevel::All,
        };
        b = b.with_optimization_level(l).unwrap();
    }
    if flag("NOFOLD") {
        b = b.with_config_entry("optimization.disable_specified_optimizers", "ConstantFolding").unwrap();
    }
    if flag("NOMEMPAT") {
        b = b.with_memory_pattern(false).unwrap();
    }
    if flag("NOPREPACK") {
        b = b.with_config_entry("session.disable_prepacking", "1").unwrap();
    }
    if let Some(s) = env("SPIN") {
        b = b.with_intra_op_spinning(s == "1").unwrap();
    }
    if let Some(p) = env("PROFILE") {
        b = b.with_profiling(p).unwrap();
    }
    if let Some(p) = env("SAVE_OPT") {
        b = b.with_optimized_model_path(p).unwrap();
    }
    let ep = env("EP").unwrap_or_else(|| "cpu".into());
    use ort::ep;
    b = b.with_auto_device(ort::session::builder::AutoDevicePolicy::Default).unwrap();
    b = if let Some(u) = ep.strip_prefix("coreml-") {
        coreml(b, u)
    } else {
        b.with_execution_providers([ep::CPU::default()
            .with_arena_allocator(!flag("NOARENA"))
            .build()
            .error_on_failure()])
            .unwrap()
    };
    if flag("HOST_STFT") {
        // the htdemucs host-STFT cut (4 stems unless STEMS is set)
        let s: i64 = env("STEMS").map(|v| v.parse().unwrap()).unwrap_or(4);
        let raw = std::fs::read(path).unwrap();
        let cut = ferraus_ml::onnx_edit::Model::parse(&raw)
            .unwrap()
            .cut(
                &[("/real_stft/Reshape_3_output_0", vec![1, 2, 2, 2049, 340])],
                &[("/Pad_2_output_0", vec![1, s, 2, 2, 2049, 340]), ("/Add_4_output_0", vec![1, s, 2, 343980])],
            )
            .unwrap();
        drop(raw);
        return b.commit_from_memory(&cut).unwrap();
    }
    b.commit_from_file(path).unwrap()
}

#[cfg(target_os = "macos")]
fn coreml(b: ort::session::builder::SessionBuilder, units: &str) -> ort::session::builder::SessionBuilder {
    use ort::ep;
    let units = match units {
        "cpu" => ep::coreml::ComputeUnits::CPUOnly,
        "gpu" => ep::coreml::ComputeUnits::CPUAndGPU,
        "ane" => ep::coreml::ComputeUnits::CPUAndNeuralEngine,
        _ => ep::coreml::ComputeUnits::All,
    };
    let mut c = ep::CoreML::default().with_compute_units(units);
    if flag("MLPROG") {
        c = c.with_model_format(ep::coreml::ModelFormat::MLProgram);
    }
    b.with_execution_providers([c.build().error_on_failure()]).unwrap()
}

#[cfg(not(target_os = "macos"))]
fn coreml(_: ort::session::builder::SessionBuilder, _: &str) -> ort::session::builder::SessionBuilder {
    panic!("CoreML is only available on macOS")
}

/// Peak resident set size so far (macOS reports bytes; 0 elsewhere).
#[cfg(target_os = "macos")]
fn rss_mb() -> f64 {
    #[repr(C)]
    struct RUsage {
        ru_utime: [i64; 2],
        ru_stime: [i64; 2],
        ru_maxrss: i64,
        rest: [i64; 13],
    }
    extern "C" {
        fn getrusage(who: i32, r: *mut RUsage) -> i32;
    }
    // SAFETY: getrusage fills a caller-owned struct of the platform's layout
    let mut r: RUsage = unsafe { std::mem::zeroed() };
    unsafe { getrusage(0, &mut r) };
    r.ru_maxrss as f64 / 1024.0 / 1024.0
}

#[cfg(not(target_os = "macos"))]
fn rss_mb() -> f64 {
    0.0
}

fn main() {
    let path = std::env::args().nth(1).expect("model path");
    let runs: usize = env("RUNS").map(|v| v.parse().unwrap()).unwrap_or(3);
    let conc: usize = env("CONC").map(|v| v.parse().unwrap()).unwrap_or(1);
    let t0 = Instant::now();
    let sessions: Vec<Session> = (0..conc).map(|_| build(&path)).collect();
    eprintln!("load {:.2}s rss {:.0} MB", t0.elapsed().as_secs_f64(), rss_mb());
    let shapes: Vec<(String, Vec<usize>)> = sessions[0]
        .inputs()
        .iter()
        .map(|i| {
            let s: Vec<usize> =
                i.dtype().tensor_shape().unwrap().iter().map(|d| if *d > 0 { *d as usize } else { 1 }).collect();
            (i.name().to_string(), s)
        })
        .collect();
    let make = |seed: u64| -> Vec<(String, Vec<usize>, Vec<f32>)> {
        shapes
            .iter()
            .map(|(n, s)| {
                let len: usize = s.iter().product();
                let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let data = (0..len)
                    .map(|i| {
                        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                        let r = ((x >> 33) as f32 / (1u64 << 31) as f32) - 0.5;
                        0.3 * r + 0.2 * ((i as f32) * 0.01).sin()
                    })
                    .collect();
                (n.clone(), s.clone(), data)
            })
            .collect()
    };
    let t1 = Instant::now();
    std::thread::scope(|sc| {
        for (si, mut s) in sessions.into_iter().enumerate() {
            let make = &make;
            sc.spawn(move || {
                for r in 0..runs {
                    let inputs: Vec<(String, ort::session::SessionInputValue<'static>)> =
                        make(1).into_iter().map(|(n, s, d)| (n, Tensor::from_array((s, d)).unwrap().into())).collect();
                    let t = Instant::now();
                    let out = s.run(inputs).unwrap();
                    let dt = t.elapsed().as_secs_f64();
                    let (_, data) = out[0].try_extract_tensor::<f32>().unwrap();
                    let sum: f64 = data.iter().map(|v| *v as f64).sum();
                    eprintln!("s{si} run {r}: {dt:.3}s sum {sum:.6} rss {:.0} MB", rss_mb());
                    if r == 0 && si == 0 {
                        if let Some(p) = env("REF_SAVE") {
                            let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
                            std::fs::write(p, bytes).unwrap();
                        }
                        if let Some(p) = env("REF") {
                            let bytes = std::fs::read(p).unwrap();
                            let mut maxd = 0f32;
                            let mut maxv = 0f32;
                            for (c, v) in bytes.chunks_exact(4).zip(data.iter()) {
                                let w = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                                maxd = maxd.max((w - v).abs());
                                maxv = maxv.max(w.abs());
                            }
                            eprintln!("vs ref: max abs diff {maxd:e} (max |ref| {maxv})");
                        }
                    }
                }
                if env("PROFILE").is_some() {
                    eprintln!("profile {}", s.end_profiling().unwrap());
                }
            });
        }
    });
    eprintln!(
        "total {:.2}s for {} windows ({:.3}s/window) peak rss {:.0} MB",
        t1.elapsed().as_secs_f64(),
        runs * conc,
        t1.elapsed().as_secs_f64() / (runs * conc) as f64,
        rss_mb()
    );
}
