//! Model-based denoising: RNNoise (pure-Rust `nnnoiseless`, weights embedded) and the
//! model slots for DeepFilterNet 3, music-aware denoise, and de-reverb checkpoints.

use ferraus_core::{AudioBuffer, Error, ErrorKind, Result};
use serde_json::{json, Value};

/// A registry slot for a denoise-family model. The models crate can list these to know
/// which names `ferraus models pull` should offer for denoising.
#[derive(Clone, Debug)]
pub struct ModelSlot {
    /// Registry name pattern (`denoise/dfn3`, `denoise/music-*`, `denoise/dereverb-*`).
    pub pattern: &'static str,
    /// The concrete name used in hints.
    pub default_name: &'static str,
    pub purpose: &'static str,
    pub domain: &'static str,
    pub license: &'static str,
    /// Whether this build can run the model once it is cached.
    pub runnable: bool,
}

/// Every denoise model slot this crate knows about.
pub fn model_slots() -> Vec<ModelSlot> {
    vec![
        ModelSlot {
            pattern: "denoise/rnnoise",
            default_name: "denoise/rnnoise",
            purpose: "RNNoise speech denoiser (weights embedded in the binary; no download)",
            domain: "speech",
            license: "BSD-3-Clause",
            runnable: cfg!(feature = "rnnoise"),
        },
        ModelSlot {
            pattern: "denoise/dfn3",
            default_name: "denoise/dfn3",
            purpose: "DeepFilterNet 3 speech enhancement at 48 kHz",
            domain: "speech",
            license: "MIT OR Apache-2.0",
            runnable: false,
        },
        ModelSlot {
            pattern: "denoise/music-*",
            default_name: "denoise/music-roformer",
            purpose: "Music-aware denoise checkpoints (community RoFormer); shipped only with an acceptable license",
            domain: "music",
            license: "per checkpoint",
            runnable: false,
        },
        ModelSlot {
            pattern: "denoise/dereverb-*",
            default_name: "denoise/dereverb-roformer",
            purpose: "De-reverb checkpoints (best-effort)",
            domain: "speech or music, per checkpoint",
            license: "per checkpoint",
            runnable: false,
        },
    ]
}

/// Look for a cached model whose registry name matches `prefix` (e.g. `denoise/dereverb-`)
/// under the models directory. Returns the first match.
pub fn find_cached(prefix: &str) -> Option<std::path::PathBuf> {
    let dir = ferraus_core::paths::models_dir();
    let (family, stem) = prefix.split_once('/')?;
    let fam = dir.join(family);
    let mut entries: Vec<_> = std::fs::read_dir(&fam).ok()?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    entries.into_iter().find(|e| e.file_name().to_string_lossy().starts_with(stem)).map(|e| e.path())
}

fn not_cached(name: &str, what: &str) -> Error {
    Error::new(
        ErrorKind::ModelNotCached,
        "model_not_cached",
        format!("{what} needs the model '{name}', which is not in the model cache"),
    )
    .with_hint(format!("ferraus models pull {name}"))
}

/// Resolve a model slot that this build cannot run itself. Always an error: either the
/// model is missing (exit 5 with a pull hint) or it is cached but no runtime for it is
/// compiled into this build. Never falls back silently.
pub fn unavailable_slot(prefix: &str, default_name: &str, what: &str) -> Error {
    match find_cached(prefix) {
        None => not_cached(default_name, what),
        Some(p) => Error::unsupported(format!(
            "{what}: found {} in the model cache, but this build has no runtime for it yet",
            p.display()
        ))
        .with_hint("ferraus doctor"),
    }
}

/// DeepFilterNet 3. The official runtime is not compiled into this build (see the crate
/// docs), so this is always an explicit error.
pub fn dfn3_unavailable() -> Error {
    match find_cached("denoise/dfn3") {
        None => not_cached("denoise/dfn3", "--model dfn3 (DeepFilterNet 3)"),
        Some(p) => Error::unsupported(format!(
            "--model dfn3: found {} in the model cache, but this build was compiled without the DeepFilterNet runtime; use --model rnnoise or the profile path",
            p.display()
        ))
        .with_hint("ferraus denoise <input> --mode ml --model rnnoise -o clean.wav"),
    }
}

/// Model identity recorded in provenance for RNNoise.
pub const RNNOISE_NAME: &str = "denoise/rnnoise";
pub const RNNOISE_VERSION: &str = "nnnoiseless-0.5.2";
/// SHA-256 of the `weights.rnn` embedded by nnnoiseless 0.5.2.
pub const RNNOISE_SHA256: &str = "e6de5fbfadf7ec91d1b24d6a6ccfd0290cb4d8bf555c5eab3ce41506f67a58b1";
/// RNNoise's internal rate and its algorithmic delay (one 10 ms frame).
pub const RNNOISE_RATE: u32 = 48_000;

/// Run RNNoise on every channel independently (48 kHz internally, resampled in and out).
#[cfg(feature = "rnnoise")]
pub fn rnnoise(buf: &AudioBuffer) -> Result<AudioBuffer> {
    use ferraus_core::resample::{resample, Quality};
    use nnnoiseless::DenoiseState;
    let frame = DenoiseState::FRAME_SIZE;
    let x = resample(buf, RNNOISE_RATE, Quality::High);
    let len = x.frames();
    let mut data = Vec::with_capacity(x.channels());
    for ch in &x.data {
        let mut st = DenoiseState::new();
        let mut out = Vec::with_capacity(len + 2 * frame);
        let mut inb = vec![0f32; frame];
        let mut outb = vec![0f32; frame];
        let mut pos = 0;
        // one extra frame flushes the algorithmic delay
        while pos < len + frame {
            for (i, v) in inb.iter_mut().enumerate() {
                *v = ch.get(pos + i).copied().unwrap_or(0.0) * 32768.0;
            }
            st.process_frame(&mut outb, &inb);
            out.extend(outb.iter().map(|v| v / 32768.0));
            pos += frame;
        }
        // compensate the one-frame delay
        out.drain(..frame.min(out.len()));
        out.truncate(len);
        data.push(out);
    }
    let y = AudioBuffer::from_planar(RNNOISE_RATE, data);
    let mut y = resample(&y, buf.sample_rate, Quality::High);
    for c in &mut y.data {
        c.resize(buf.frames(), 0.0);
    }
    Ok(y)
}

#[cfg(not(feature = "rnnoise"))]
pub fn rnnoise(_buf: &AudioBuffer) -> Result<AudioBuffer> {
    Err(Error::unsupported("--model rnnoise: this build was compiled without the 'rnnoise' feature"))
}

/// The JSON note attached to every model-path run.
pub fn speech_note(model: &str) -> Value {
    json!({
        "model": model,
        "domain": "speech",
        "note": "speech-tuned model; on music the profile path (--mode profile) usually sounds better",
    })
}
