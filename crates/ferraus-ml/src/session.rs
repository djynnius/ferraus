//! ONNX Runtime sessions: one process-wide environment, a session cache keyed by model path
//! and provider settings, provider fallback, deterministic mode, and an `f32` run helper.

use crate::provider::{candidates, select, Provider};
use ferraus_core::{Error, OpCtx, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// How to build a session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionOptions {
    /// `auto`, `cpu`, `cuda`, `coreml`, `directml`.
    pub device: String,
    /// Fixed thread counts and deterministic kernels (CPU results bit-exact across runs).
    pub deterministic: bool,
    /// Intra-op threads (0 = ONNX Runtime default).
    pub threads: usize,
    /// Providers the model is known to work with (`ModelSpec::provider_hints`); automatic
    /// selection skips the others. Empty = no restriction.
    pub provider_hints: Vec<String>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        SessionOptions { device: "auto".into(), deterministic: false, threads: 0, provider_hints: Vec::new() }
    }
}

/// Threads used in deterministic mode when `--threads` is not given: a constant, so results
/// do not depend on the machine's core count.
pub const DETERMINISTIC_THREADS: usize = 4;

impl SessionOptions {
    pub fn from_ctx(ctx: &OpCtx) -> SessionOptions {
        SessionOptions {
            device: ctx.globals.device.clone(),
            deterministic: ctx.globals.deterministic,
            threads: ctx.globals.threads,
            provider_hints: Vec::new(),
        }
    }

    fn intra_threads(&self) -> usize {
        if self.deterministic && self.threads == 0 {
            DETERMINISTIC_THREADS
        } else {
            self.threads
        }
    }
}

/// A loaded model.
pub struct ModelSession {
    session: Mutex<Session>,
    pub provider: Provider,
    pub path: PathBuf,
    pub input_names: Vec<String>,
    pub output_names: Vec<String>,
    /// Declared input shapes (`-1` = dynamic).
    pub input_shapes: Vec<Vec<i64>>,
}

/// A named `f32` tensor.
#[derive(Clone, Debug, PartialEq)]
pub struct TensorF32 {
    pub name: String,
    pub shape: Vec<i64>,
    pub data: Vec<f32>,
}

impl TensorF32 {
    pub fn new(name: &str, shape: Vec<i64>, data: Vec<f32>) -> TensorF32 {
        TensorF32 { name: name.to_string(), shape, data }
    }
}

fn environment() -> Result<&'static ort::environment::Environment> {
    static ENV: OnceLock<std::result::Result<ort::environment::Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| ort::init().with_name("ferraus").with_telemetry(false).build().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| Error::processing(format!("ONNX Runtime failed to start: {e}")))
}

type CacheKey = (PathBuf, SessionOptions);

fn cache() -> &'static Mutex<HashMap<CacheKey, Arc<ModelSession>>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Arc<ModelSession>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Build a session on one provider (no fallback).
pub fn build_on(path: &Path, provider: Provider, opts: &SessionOptions) -> std::result::Result<Session, String> {
    let env = environment().map_err(|e| e.message)?;
    let s = |e: ort::Error<ort::session::builder::SessionBuilder>| e.to_string();
    let mut b = Session::builder(env).map_err(|e| e.to_string())?;
    let threads = opts.intra_threads();
    if threads > 0 {
        b = b.with_intra_threads(threads).map_err(s)?;
    }
    if opts.deterministic {
        b = b
            .with_inter_threads(1)
            .map_err(s)?
            .with_parallel_execution(false)
            .map_err(s)?
            .with_deterministic_compute(true)
            .map_err(s)?;
    }
    b = register(b, provider)?;
    b.commit_from_file(path).map_err(|e| e.to_string())
}

fn register(
    b: ort::session::builder::SessionBuilder,
    provider: Provider,
) -> std::result::Result<ort::session::builder::SessionBuilder, String> {
    use ort::ep;
    let s = |e: ort::Error<ort::session::builder::SessionBuilder>| e.to_string();
    match provider {
        // `ort` sets a "max efficiency" automatic device policy on every new builder, which
        // silently picks CoreML/NPU devices; the CPU EP registration does not append a
        // provider, so the policy must be reset explicitly or "cpu" would not mean CPU.
        Provider::Cpu => b
            .with_auto_device(ort::session::builder::AutoDevicePolicy::Default)
            .map_err(s)?
            .with_execution_providers([ep::CPU::default().build().error_on_failure()])
            .map_err(s),
        Provider::Cuda => {
            #[cfg(feature = "cuda")]
            {
                b.with_execution_providers([ep::CUDA::default().build().error_on_failure()]).map_err(s)
            }
            #[cfg(not(feature = "cuda"))]
            {
                let _ = b;
                Err("this build of ferraus does not include the CUDA execution provider (build with --features cuda after `ferraus accelerate install`)".into())
            }
        }
        Provider::CoreMl => {
            #[cfg(any(feature = "coreml", target_os = "macos"))]
            {
                b.with_execution_providers([ep::CoreML::default().build().error_on_failure()]).map_err(s)
            }
            #[cfg(not(any(feature = "coreml", target_os = "macos")))]
            {
                let _ = b;
                Err("CoreML is only available on macOS".into())
            }
        }
        Provider::DirectMl => {
            #[cfg(any(feature = "directml", windows))]
            {
                b.with_execution_providers([ep::DirectML::default().build().error_on_failure()]).map_err(s)
            }
            #[cfg(not(any(feature = "directml", windows)))]
            {
                let _ = b;
                Err("DirectML is only available on Windows".into())
            }
        }
    }
}

/// Load (or reuse) a session for `path`, selecting a provider with fallback. Returns the
/// session and the fallback warnings (only on first load).
pub fn load(path: &Path, opts: &SessionOptions) -> Result<(Arc<ModelSession>, Vec<String>)> {
    let key = (std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()), opts.clone());
    if let Some(s) = cache().lock().expect("session cache").get(&key) {
        return Ok((s.clone(), Vec::new()));
    }
    if !path.exists() {
        return Err(Error::input(format!("model file not found: {}", path.display())));
    }
    let mut order = candidates(&opts.device, opts.deterministic, &|p| p.compiled())?;
    if Provider::parse(&opts.device)?.is_none() {
        order = crate::provider::apply_hints(&order, &opts.provider_hints);
    }
    let sel = select(&order, |p| build_on(path, p, opts))?;
    let session = sel.value;
    let input_names = session.inputs().iter().map(|i| i.name().to_string()).collect();
    let output_names = session.outputs().iter().map(|o| o.name().to_string()).collect();
    let input_shapes = session
        .inputs()
        .iter()
        .map(|i| i.dtype().tensor_shape().map(|s| s.iter().copied().collect()).unwrap_or_default())
        .collect();
    let ms = Arc::new(ModelSession {
        session: Mutex::new(session),
        provider: sel.provider,
        path: path.to_path_buf(),
        input_names,
        output_names,
        input_shapes,
    });
    cache().lock().expect("session cache").insert(key, ms.clone());
    Ok((ms, sel.warnings))
}

/// Load with the context's `--device`/`--deterministic`/`--threads`, surfacing fallback
/// warnings through `ctx.warn`.
pub fn load_for(ctx: &mut OpCtx, path: &Path) -> Result<Arc<ModelSession>> {
    load_for_hinted(ctx, path, &[])
}

/// Like [`load_for`], restricting automatic provider selection to `hints` (a model's
/// `provider_hints`, e.g. when its graph is known not to compile for CoreML).
pub fn load_for_hinted(ctx: &mut OpCtx, path: &Path, hints: &[String]) -> Result<Arc<ModelSession>> {
    let mut opts = SessionOptions::from_ctx(ctx);
    opts.provider_hints = hints.to_vec();
    let (s, warnings) = load(path, &opts)?;
    for w in warnings {
        ctx.warn("provider_fallback", w);
    }
    ctx.note("execution_provider", s.provider.name());
    Ok(s)
}

/// Drop every cached session (frees memory between long jobs; used by tests).
pub fn clear_cache() {
    cache().lock().expect("session cache").clear();
}

impl ModelSession {
    /// Run with `f32` inputs (matched by name; an empty shape is a scalar) and return every
    /// output as `f32` (`f64` outputs are narrowed).
    pub fn run(&self, inputs: Vec<TensorF32>) -> Result<Vec<TensorF32>> {
        let mut values: Vec<(String, ort::session::SessionInputValue<'static>)> = Vec::new();
        for t in inputs {
            let shape: Vec<usize> = t.shape.iter().map(|d| *d as usize).collect();
            let tensor = Tensor::from_array((shape, t.data))
                .map_err(|e| Error::processing(format!("cannot build input tensor {}: {e}", t.name)))?;
            values.push((t.name, tensor.into()));
        }
        self.run_values(values)
    }

    /// Run a model whose single input is a `u8` tensor (image models exported with a
    /// `uint8` input, such as oemer's segmentation networks). Outputs as in [`Self::run`].
    pub fn run_u8(&self, name: &str, shape: Vec<i64>, data: Vec<u8>) -> Result<Vec<TensorF32>> {
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        let tensor = Tensor::from_array((dims, data))
            .map_err(|e| Error::processing(format!("cannot build input tensor {name}: {e}")))?;
        self.run_values(vec![(name.to_string(), tensor.into())])
    }

    fn run_values(&self, values: Vec<(String, ort::session::SessionInputValue<'static>)>) -> Result<Vec<TensorF32>> {
        let mut session = self.session.lock().expect("session lock");
        let outputs = session.run(values).map_err(|e| Error::processing(format!("model inference failed: {e}")))?;
        let mut out = Vec::new();
        for name in &self.output_names {
            let v = outputs.get(name).ok_or_else(|| Error::processing(format!("model produced no output '{name}'")))?;
            // f32 outputs are copied; f64 outputs (e.g. SwiftF0's pitch) are narrowed
            let (shape, data) = match v.try_extract_tensor::<f32>() {
                Ok((shape, data)) => (shape.iter().copied().collect(), data.to_vec()),
                Err(e32) => match v.try_extract_tensor::<f64>() {
                    Ok((shape, data)) => (shape.iter().copied().collect(), data.iter().map(|x| *x as f32).collect()),
                    Err(_) => return Err(Error::processing(format!("output {name} is not f32 or f64: {e32}"))),
                },
            };
            out.push(TensorF32 { name: name.clone(), shape, data });
        }
        Ok(out)
    }
}
