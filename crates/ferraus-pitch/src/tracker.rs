//! Pitch-tracker selection (`--tracker pyin|crepe|rmvpe|swiftf0`).
//!
//! pYIN is built in. The model trackers (CREPE, RMVPE, SwiftF0) run on ONNX Runtime in
//! `ferraus-ml`, which depends on this crate, implements [`PitchTracker`] and registers a
//! factory with [`register_model_tracker`] (`ferraus_ml::register_all()`, which the CLI
//! calls when it builds its registry). A factory receives the op context so it can apply
//! the model download policy (`-y`, config, `--offline`) and record the model in the
//! provenance sidecar. Until a factory is registered, or when the model is not in the
//! cache, selecting a model tracker fails with `ErrorKind::ModelNotCached` and a
//! `ferraus models pull pitch/<name>` hint.

use crate::pyin::{self, PitchFrame, PyinOptions};
use ferraus_core::{Error, ErrorKind, OpCtx, Result};
use std::sync::{Mutex, OnceLock};

/// The selectable trackers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackerKind {
    Pyin,
    Crepe,
    Rmvpe,
    SwiftF0,
}

impl TrackerKind {
    /// Values accepted by `--tracker`.
    pub const CHOICES: &'static [&'static str] = &["pyin", "crepe", "rmvpe", "swiftf0"];

    pub fn parse(s: &str) -> Result<TrackerKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pyin" => Ok(TrackerKind::Pyin),
            "crepe" => Ok(TrackerKind::Crepe),
            "rmvpe" => Ok(TrackerKind::Rmvpe),
            "swiftf0" | "swift-f0" => Ok(TrackerKind::SwiftF0),
            other => {
                Err(Error::usage(format!("unknown pitch tracker '{other}'; choose pyin, crepe, rmvpe or swiftf0")))
            }
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TrackerKind::Pyin => "pyin",
            TrackerKind::Crepe => "crepe",
            TrackerKind::Rmvpe => "rmvpe",
            TrackerKind::SwiftF0 => "swiftf0",
        }
    }

    /// Model-cache name for model trackers (`pitch/crepe`).
    pub fn model_name(self) -> Option<String> {
        match self {
            TrackerKind::Pyin => None,
            k => Some(format!("pitch/{}", k.name())),
        }
    }
}

/// Settings every tracker understands.
#[derive(Clone, Debug)]
pub struct TrackOptions {
    pub fmin: f64,
    pub fmax: f64,
    pub hop_s: f64,
}

impl Default for TrackOptions {
    fn default() -> Self {
        TrackOptions { fmin: 60.0, fmax: 1200.0, hop_s: 0.010 }
    }
}

/// A frame-wise f0 estimator. Frames must be centred on `k × hop_s` and cover the input.
pub trait PitchTracker: Send + Sync {
    /// Tracker name (`pyin`, `crepe`, ...).
    fn name(&self) -> &str;
    /// Model identity for provenance (name, version, sha256), `None` for built-ins.
    fn model(&self) -> Option<(String, String, String)> {
        None
    }
    /// Track a mono signal.
    fn track(&self, samples: &[f32], sr: u32, opts: &TrackOptions) -> Result<Vec<PitchFrame>>;
}

/// The built-in pYIN tracker.
#[derive(Clone, Debug, Default)]
pub struct PyinTracker;

impl PitchTracker for PyinTracker {
    fn name(&self) -> &str {
        "pyin"
    }
    fn track(&self, samples: &[f32], sr: u32, opts: &TrackOptions) -> Result<Vec<PitchFrame>> {
        let o = PyinOptions { fmin: opts.fmin, fmax: opts.fmax, hop_s: opts.hop_s, ..PyinOptions::default() };
        Ok(pyin::track(samples, sr, &o))
    }
}

/// Factory for a model tracker; returns `ModelNotCached` when its weights are missing and
/// the download policy in `ctx` does not allow fetching them.
pub type TrackerFactory = fn(&mut OpCtx) -> Result<Box<dyn PitchTracker>>;

fn registry() -> &'static Mutex<Vec<(TrackerKind, TrackerFactory)>> {
    static R: OnceLock<Mutex<Vec<(TrackerKind, TrackerFactory)>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register (or replace) the factory for a model tracker. Called by `ferraus-ml`.
pub fn register_model_tracker(kind: TrackerKind, factory: TrackerFactory) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|(k, _)| *k != kind);
    r.push((kind, factory));
}

/// Error returned for a model tracker whose model is not available.
pub fn model_not_cached(kind: TrackerKind) -> Error {
    let model = kind.model_name().unwrap_or_default();
    Error::new(
        ErrorKind::ModelNotCached,
        "model_not_cached",
        format!("pitch tracker '{}' needs the model {model}, which is not installed", kind.name()),
    )
    .with_hint(format!("ferraus models pull {model}"))
}

/// Get a tracker by kind, applying the model download policy of `ctx` and recording the
/// model used in it.
pub fn tracker_for(ctx: &mut OpCtx, kind: TrackerKind) -> Result<Box<dyn PitchTracker>> {
    if kind == TrackerKind::Pyin {
        return Ok(Box::new(PyinTracker));
    }
    let factory = {
        let r = registry().lock().unwrap_or_else(|e| e.into_inner());
        r.iter().find(|(k, _)| *k == kind).map(|(_, f)| *f)
    };
    match factory {
        Some(f) => f(ctx),
        None => Err(model_not_cached(kind)),
    }
}

/// Get a tracker by its `--tracker` name (see [`tracker_for`]).
pub fn by_name_for(ctx: &mut OpCtx, name: &str) -> Result<Box<dyn PitchTracker>> {
    tracker_for(ctx, TrackerKind::parse(name)?)
}

/// Get a tracker by kind without an op context: model downloads follow only the
/// environment (`FERRAUS_AUTO_DOWNLOAD=1`, `FERRAUS_OFFLINE=1`) and warnings are dropped.
/// Prefer [`tracker_for`] inside ops.
pub fn tracker(kind: TrackerKind) -> Result<Box<dyn PitchTracker>> {
    let mut ctx = OpCtx::new(
        "pitch",
        Vec::new(),
        ferraus_core::ctx::Globals::default(),
        &ferraus_core::testing::SIMPLE_IO,
        &ferraus_core::testing::NO_PROGRESS,
    );
    tracker_for(&mut ctx, kind)
}

/// Get a tracker by its `--tracker` name without an op context (see [`tracker`]).
pub fn by_name(name: &str) -> Result<Box<dyn PitchTracker>> {
    tracker(TrackerKind::parse(name)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_trackers_fail_with_pull_hint() {
        let e = by_name("rmvpe").err().unwrap();
        assert_eq!(e.kind, ErrorKind::ModelNotCached);
        assert_eq!(e.hint.as_deref(), Some("ferraus models pull pitch/rmvpe"));
        assert!(by_name("nope").is_err());
        assert_eq!(by_name("pyin").unwrap().name(), "pyin");
    }

    #[test]
    fn registered_factory_is_used() {
        struct Fake;
        impl PitchTracker for Fake {
            fn name(&self) -> &str {
                "swiftf0"
            }
            fn track(&self, _: &[f32], _: u32, _: &TrackOptions) -> Result<Vec<PitchFrame>> {
                Ok(Vec::new())
            }
        }
        fn make(_: &mut OpCtx) -> Result<Box<dyn PitchTracker>> {
            Ok(Box::new(Fake))
        }
        register_model_tracker(TrackerKind::SwiftF0, make);
        assert_eq!(by_name("swiftf0").unwrap().name(), "swiftf0");
    }
}
