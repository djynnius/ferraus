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
    /// Trade speed for memory: skip ONNX Runtime's constant folding (graphs with static shapes,
    /// like the Demucs exports, otherwise materialize large index and window tensors while
    /// folding), its memory pattern, weight prepacking, and the layout-level (`ORT_ENABLE_ALL`)
    /// graph rewrites. Measured on an M1 Pro, 10 threads: htdemucs 0.95 GB peak instead of
    /// 4.6 GB at about the same speed per window; Mel-Band RoFormer 2.8 GB instead of 5.4 GB.
    pub lean: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        SessionOptions {
            device: "auto".into(),
            deterministic: false,
            threads: 0,
            provider_hints: Vec::new(),
            lean: false,
        }
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
            lean: false,
        }
    }

    fn intra_threads(&self) -> usize {
        if self.threads > 0 {
            self.threads
        } else if self.deterministic {
            DETERMINISTIC_THREADS
        } else {
            default_threads()
        }
    }
}

/// Intra-op threads when neither `--threads` nor `--deterministic` sets them (0 = ONNX
/// Runtime's own choice). On Apple silicon (no SMT) ONNX Runtime's default leaves cores idle:
/// on an M1 Pro htdemucs ran 1.94 s per window by default and 1.55 s on all 10 cores, the
/// Mel-Band RoFormer 10.0 s and 6.5 s. Elsewhere its default (the physical cores) is kept,
/// since hyper-threads slow its GEMM kernels down.
pub fn default_threads() -> usize {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
    } else {
        0
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

/// Model file, graph variant (`""` = the file as is), options.
type CacheKey = (PathBuf, String, SessionOptions);

fn cache() -> &'static Mutex<HashMap<CacheKey, Arc<ModelSession>>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Arc<ModelSession>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Build a session on one provider (no fallback).
pub fn build_on(path: &Path, provider: Provider, opts: &SessionOptions) -> std::result::Result<Session, String> {
    build_from(Source::File(path), provider, opts)
}

/// Where a session's graph comes from.
#[derive(Clone, Copy)]
enum Source<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

fn build_from(src: Source, provider: Provider, opts: &SessionOptions) -> std::result::Result<Session, String> {
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
    if opts.lean {
        // measured on htdemucs (12 s input, CPU): 5.1 GB peak by default; 2.2 GB without
        // constant folding; 1.45 GB also without the memory pattern (one preallocated block
        // sized for every intermediate tensor); 1.3 GB also without weight prepacking
        // ... and 0.95 GB also without the ORT_ENABLE_ALL rewrites (same speed on CPU; the
        // Mel-Band RoFormer drops from 3.5 GB to 2.8 GB)
        b = b
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level2)
            .map_err(s)?
            .with_config_entry("optimization.disable_specified_optimizers", "ConstantFolding")
            .map_err(s)?
            .with_memory_pattern(false)
            .map_err(s)?
            .with_config_entry("session.disable_prepacking", "1")
            .map_err(s)?;
    }
    b = register(b, provider)?;
    match src {
        Source::File(path) => b.commit_from_file(path).map_err(|e| e.to_string()),
        Source::Bytes(bytes) => b.commit_from_memory(bytes).map_err(|e| e.to_string()),
    }
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
        // `ort`'s CPU provider disables ONNX Runtime's memory arena unless asked; with the
        // arena (ONNX Runtime's own default) a lean htdemucs window peaks about 0.5 GB lower
        // (1.3 GB instead of 1.9 GB on an M1 Pro) at the same speed.
        Provider::Cpu => b
            .with_auto_device(ort::session::builder::AutoDevicePolicy::Default)
            .map_err(s)?
            .with_execution_providers([ep::CPU::default().with_arena_allocator(true).build().error_on_failure()])
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
    load_variant(path, "", opts, None)
}

/// Like [`load`], for a transformed copy of the model: `variant` names the transformation
/// (it is part of the cache key) and `make` turns the file's bytes into the graph to load.
pub fn load_transformed(
    path: &Path,
    variant: &str,
    opts: &SessionOptions,
    make: &dyn Fn(&[u8]) -> Result<Vec<u8>>,
) -> Result<(Arc<ModelSession>, Vec<String>)> {
    load_variant(path, variant, opts, Some(make))
}

fn load_variant(
    path: &Path,
    variant: &str,
    opts: &SessionOptions,
    make: Option<&dyn Fn(&[u8]) -> Result<Vec<u8>>>,
) -> Result<(Arc<ModelSession>, Vec<String>)> {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let key = (canon, variant.to_string(), opts.clone());
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
    let bytes = match make {
        Some(f) => {
            let raw =
                std::fs::read(path).map_err(|e| Error::input(format!("cannot read model {}: {e}", path.display())))?;
            Some(f(&raw)?)
        }
        None => None,
    };
    let sel = select(&order, |p| match &bytes {
        Some(b) => build_from(Source::Bytes(b), p, opts),
        None => build_on(path, p, opts),
    })?;
    drop(bytes);
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
    load_for_with(ctx, path, hints, false)
}

/// Like [`load_for_hinted`], optionally building a [lean](SessionOptions::lean) session.
pub fn load_for_with(ctx: &mut OpCtx, path: &Path, hints: &[String], lean: bool) -> Result<Arc<ModelSession>> {
    let mut opts = SessionOptions::from_ctx(ctx);
    opts.provider_hints = hints.to_vec();
    opts.lean = lean;
    let (s, warnings) = load(path, &opts)?;
    for w in warnings {
        ctx.warn("provider_fallback", w);
    }
    ctx.note("execution_provider", s.provider.name());
    Ok(s)
}

/// [`load_transformed`] with the context's settings (see [`load_for_with`]).
pub fn load_transformed_for(
    ctx: &mut OpCtx,
    path: &Path,
    variant: &str,
    hints: &[String],
    lean: bool,
    make: &dyn Fn(&[u8]) -> Result<Vec<u8>>,
) -> Result<Arc<ModelSession>> {
    let mut opts = SessionOptions::from_ctx(ctx);
    opts.provider_hints = hints.to_vec();
    opts.lean = lean;
    let (s, warnings) = load_transformed(path, variant, &opts, make)?;
    for w in warnings {
        ctx.warn("provider_fallback", w);
    }
    ctx.note("execution_provider", s.provider.name());
    Ok(s)
}

/// Forget every cached session of one model file, so its memory is freed as soon as the
/// caller drops its own handle (separation runs several large models one after another).
pub fn evict(path: &Path) {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    cache().lock().expect("session cache").retain(|(p, _, _), _| *p != canon);
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
