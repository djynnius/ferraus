//! End-to-end tests of `separate` with tiny generated ONNX models (no real weights).

use super::*;
use ferraus_core::testing::{self, TempDir};
use ferraus_ml::onnx_proto::{self, node, Attr, Dim, GraphBuilder};
use std::sync::Once;

/// Point the model cache at an empty temporary directory once for the whole test binary, so
/// tests never see (or touch) the developer's real cache.
fn isolate_cache() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ferraus-separate-tests-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FERRAUS_HOME", &dir);
        std::env::remove_var("FERRAUS_AUTO_DOWNLOAD");
        std::env::remove_var("FERRAUS_OFFLINE");
    });
}

const K: usize = 9;

/// Kernels for a 4-stem toy separator (drums, bass, other, vocals). Their sum is NOT a unit
/// impulse, so the raw stems leave a residual for sum-to-mix to assign.
fn kernels() -> Vec<Vec<f32>> {
    let boxcar: Vec<f32> = vec![1.0 / K as f32; K];
    let mut delta = [0.0f32; K];
    delta[K / 2] = 1.0;
    let hp: Vec<f32> = delta.iter().zip(&boxcar).map(|(d, b)| d - b).collect();
    vec![
        hp.iter().map(|v| v * 0.3).collect(),     // drums
        boxcar.iter().map(|v| v * 0.6).collect(), // bass
        delta.iter().map(|v| v * 0.1).collect(),  // other
        hp.iter().map(|v| v * 0.5).collect(),     // vocals
    ]
}

/// Write the toy model plus its `.spec.json`; returns the model path.
fn toy_model(dir: &TempDir, name: &str, kernels: &[Vec<f32>], stems: &[&str]) -> PathBuf {
    let p = dir.path(&format!("{name}.onnx"));
    std::fs::write(&p, onnx_proto::separator_model(kernels)).unwrap();
    let spec = ModelSpec {
        name: name.into(),
        version: "test".into(),
        task: "separate".into(),
        sha256: None,
        sample_rate: 44_100,
        stems: stems.iter().map(|s| s.to_string()).collect(),
        arch: "waveform".into(),
        inputs: vec![],
        outputs: vec![],
        chunk_seconds: Some(0.25),
        chunk_samples: None,
        overlap: 0.25,
        context_samples: 16,
        provider_hints: vec![],
        license: Default::default(),
        params: Value::Null,
    };
    spec.save(&p.with_extension("spec.json")).unwrap();
    p
}

fn mix(secs: f64, sr: u32) -> AudioBuffer {
    let a = testing::sine(sr, 110.0, secs, 0.3);
    let b = testing::chirp(sr, 500.0, 6000.0, secs, 0.15);
    let c = testing::noise(sr, secs, 0.05, 11);
    let mut m = testing::add(&testing::add(&a, &b), &c).with_channels(2);
    for v in m.data[1].iter_mut() {
        *v *= -0.8;
    }
    m
}

struct Run {
    dir: TempDir,
    input: PathBuf,
}

fn setup(name: &str, buf: &AudioBuffer) -> Run {
    isolate_cache();
    let dir = TempDir::new(name);
    let input = dir.path("song.wav");
    testing::write_wav(&input, buf);
    Run { dir, input }
}

fn run(r: &Run, pairs: &[(&str, &str)], tweak: impl FnOnce(&mut OpCtx)) -> Result<(Value, OpCtx<'static>)> {
    let spec = &ops()[0];
    let a = testing::args(spec, pairs);
    let mut ctx = testing::ctx();
    ctx.command = "separate".into();
    ctx.inputs = vec![r.input.clone()];
    ctx.globals.device = "cpu".into();
    ctx.globals.deterministic = true;
    ctx.globals.threads = 1;
    tweak(&mut ctx);
    let v = separate(&a, &mut ctx)?;
    Ok((v, ctx))
}

fn stem_path(r: &Run, stem: &str) -> PathBuf {
    r.dir.path(&format!("song.{stem}.wav"))
}

#[test]
fn default_tier_without_models_is_exit_5_with_pull_hint() {
    let r = setup("sep-missing", &mix(0.5, 44_100));
    let e = run(&r, &[], |_| {}).err().unwrap();
    assert_eq!(e.exit_code, 5, "{e}");
    assert_eq!(e.code, "model_not_cached");
    // the default tier is auto: the best verified tier not held back (balanced while the
    // quality tier's RoFormer awaits license review)
    let hint = e.hint.unwrap();
    assert!(hint.starts_with("ferraus models pull separate/htdemucs-ft-drums@v4-onnx.2026-05-21 "), "{hint}");
    assert!(!hint.contains("mel-roformer"), "{hint}");
    let e = run(&r, &[("tier", "quality")], |_| {}).err().unwrap();
    assert!(e.hint.unwrap().starts_with("ferraus models pull separate/mel-roformer-kj@2024.08 "));
    let e = run(&r, &[("tier", "balanced")], |_| {}).err().unwrap();
    assert_eq!(e.exit_code, 5, "{e}");
    let hint = e.hint.unwrap();
    assert!(hint.starts_with("ferraus models pull separate/htdemucs-ft-drums@v4-onnx.2026-05-21 "), "{hint}");
    assert!(hint.ends_with("separate/htdemucs-ft-vocals@v4-onnx.2026-05-21"), "{hint}");
    let e = run(&r, &[("tier", "fast"), ("stems", "6")], |_| {}).err().unwrap();
    assert_eq!(e.hint.as_deref(), Some("ferraus models pull separate/htdemucs-6s@v4-onnx.2026-05-21"));
    let e = run(&r, &[("tier", "fast")], |c| c.globals.offline = true).err().unwrap();
    assert_eq!(e.exit_code, 5);
    assert_eq!(e.hint.as_deref(), Some("ferraus models pull separate/htdemucs@v4-onnx.2026-05-21"));
    // nothing was written
    assert!(!stem_path(&r, "vocals").exists());
}

#[test]
fn four_stems_written_24_bit_and_sum_to_mix_within_one_lsb() {
    let m = mix(1.0, 44_100);
    let r = setup("sep-sum", &m);
    let dir = TempDir::new("sep-sum-model");
    let model = toy_model(&dir, "toy", &kernels(), &["drums", "bass", "other", "vocals"]);
    let (v, ctx) = run(&r, &[("model", model.to_str().unwrap())], |_| {}).unwrap();
    assert_eq!(v["stems"].as_array().unwrap().len(), 4);
    assert_eq!(ctx.outputs.len(), 4);
    let roles: Vec<&str> = ctx.outputs.iter().map(|o| o.role.as_deref().unwrap()).collect();
    assert_eq!(roles, vec!["vocals", "drums", "bass", "other"], "other (residual) written last");
    assert!(v["sum_to_mix"]["applied"].as_bool().unwrap());
    assert_eq!(v["sum_to_mix"]["grid_bits"], 24);
    let mut sum = AudioBuffer::new(44_100, 2, m.frames());
    for s in STEMS_4 {
        let bytes = std::fs::read(stem_path(&r, s)).unwrap();
        let (b, bits, float) = ferraus_core::wav::decode(&bytes).unwrap();
        assert_eq!((bits, float), (24, false), "{s} must be 24-bit PCM");
        sum = testing::add(&sum, &b);
    }
    let lsb = 1.0 / 8_388_608.0;
    let mut worst = 0f64;
    for c in 0..2 {
        for i in 0..m.frames() {
            worst = worst.max((sum.data[c][i] as f64 - m.data[c][i] as f64).abs());
        }
    }
    assert!(worst <= lsb, "stems sum to the input within {} LSB", worst / lsb);
    // single-model quality = mask confidence
    for s in v["stems"].as_array().unwrap() {
        if s["stem"] != "other" {
            assert_eq!(s["quality"]["method"], "mask_confidence");
            let q = s["quality"]["score"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&q));
        }
    }
    assert!(ctx.models.iter().any(|m| m.name.ends_with("toy.onnx")));
}

#[test]
fn chunked_equals_whole_file_and_no_seams() {
    let m = mix(1.5, 44_100);
    let r = setup("sep-chunk", &m);
    let dir = TempDir::new("sep-chunk-model");
    let model = toy_model(&dir, "toy", &kernels(), &["drums", "bass", "other", "vocals"]);
    let mp = model.to_str().unwrap();
    let float = |c: &mut OpCtx| {
        c.globals.bit_depth = Some(32);
        c.globals.overwrite = true;
    };
    run(&r, &[("model", mp), ("segment", "whole"), ("sum-to-mix", "off")], float).unwrap();
    let whole: Vec<AudioBuffer> = STEMS_4.iter().map(|s| testing::read_wav(&stem_path(&r, s))).collect();
    run(&r, &[("model", mp), ("segment", "0.2s"), ("overlap", "25%"), ("sum-to-mix", "off")], float).unwrap();
    let chunked: Vec<AudioBuffer> = STEMS_4.iter().map(|s| testing::read_wav(&stem_path(&r, s))).collect();
    for (w, c) in whole.iter().zip(&chunked) {
        let d = testing::rms_diff(w, c);
        assert!(d < 1e-6, "chunked differs from whole: rms {d}");
        let peak = w.data[0].iter().zip(&c.data[0]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(peak < 1e-5, "peak {peak}");
    }
    // spectral discontinuity: high-frequency energy at window boundaries equals elsewhere
    let plan = ferraus_ml::ChunkPlan::new((0.2f64 * 44_100.0).round() as usize, 0.25, 16).unwrap();
    let y = &chunked[3].data[0];
    let d2: Vec<f32> = (1..y.len() - 1).map(|i| y[i + 1] - 2.0 * y[i] + y[i - 1]).collect();
    let w = &whole[3].data[0];
    let d2w: Vec<f32> = (1..w.len() - 1).map(|i| w[i + 1] - 2.0 * w[i] + w[i - 1]).collect();
    for b in plan.boundaries(y.len()) {
        if b < 64 || b + 64 > d2.len() {
            continue;
        }
        let e = |v: &[f32]| v[b - 32..b + 32].iter().map(|x| x * x).sum::<f32>();
        let ratio = e(&d2) / e(&d2w).max(1e-20);
        assert!((ratio - 1.0).abs() < 1e-3, "seam at {b}: HF energy ratio {ratio}");
    }
}

#[test]
fn ensemble_weights_tta_and_agreement() {
    let m = mix(0.6, 44_100);
    let r = setup("sep-ens", &m);
    let dir = TempDir::new("sep-ens-model");
    let a = toy_model(&dir, "a", &kernels(), &["drums", "bass", "other", "vocals"]);
    let k2: Vec<Vec<f32>> = kernels().iter().map(|k| k.iter().map(|v| v * 0.9).collect()).collect();
    let b = toy_model(&dir, "b", &k2, &["drums", "bass", "other", "vocals"]);
    let ens = format!("{},{}", a.display(), b.display());
    let (v, ctx) = run(&r, &[("ensemble", &ens), ("weights", "0.7,0.3"), ("shifts", "2")], |_| {}).unwrap();
    assert_eq!(v["models"].as_array().unwrap().len(), 2);
    assert_eq!(v["shifts"], 2);
    let voc = v["stems"].as_array().unwrap().iter().find(|s| s["stem"] == "vocals").unwrap();
    assert_eq!(voc["quality"]["method"], "ensemble_agreement");
    let q = voc["quality"]["score"].as_f64().unwrap();
    assert!(q > 0.95 && q <= 1.0, "near-identical members agree: {q}");
    assert!(ctx.metrics.contains_key("quality_vocals"));
    // weight count mismatch is a usage error
    let e = run(&r, &[("ensemble", &ens), ("weights", "1")], |c| c.globals.overwrite = true).err().unwrap();
    assert_eq!(e.exit_code, 2);
    // same seed → identical output (deterministic TTA)
    let first = std::fs::read(stem_path(&r, "vocals")).unwrap();
    run(&r, &[("ensemble", &ens), ("weights", "0.7,0.3"), ("shifts", "2")], |c| c.globals.overwrite = true).unwrap();
    assert_eq!(first, std::fs::read(stem_path(&r, "vocals")).unwrap());
}

#[test]
fn only_instrumental_two_stem_and_mono() {
    let m = mix(0.5, 44_100);
    let r = setup("sep-modes", &m);
    let dir = TempDir::new("sep-modes-model");
    let model = toy_model(&dir, "toy", &kernels(), &["drums", "bass", "other", "vocals"]);
    let mp = model.to_str().unwrap();
    let (_, ctx) = run(&r, &[("model", mp), ("only", "vocals,drums")], |_| {}).unwrap();
    assert_eq!(ctx.outputs.len(), 2);
    assert!(!stem_path(&r, "bass").exists());

    let (_, ctx) = run(&r, &[("model", mp), ("instrumental", "true")], |c| c.globals.overwrite = true).unwrap();
    assert_eq!(ctx.outputs.len(), 1);
    assert_eq!(ctx.outputs[0].role.as_deref(), Some("instrumental"));
    // instrumental + vocals = mix
    let inst = testing::read_wav(&stem_path(&r, "instrumental"));
    let voc = testing::read_wav(&stem_path(&r, "vocals"));
    let sum = testing::add(&inst, &voc);
    assert!(testing::rms_diff(&sum, &m) < 2.0 / 8_388_608.0);

    let (v, _) = run(&r, &[("model", mp), ("stems", "2")], |c| c.globals.overwrite = true).unwrap();
    let names: Vec<&str> = v["stems"].as_array().unwrap().iter().map(|s| s["stem"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["vocals", "instrumental"]);

    // mono in → mono out, input rate preserved
    let mono = testing::sine(22_050, 330.0, 0.4, 0.4);
    let r2 = setup("sep-mono", &mono);
    run(&r2, &[("model", mp)], |_| {}).unwrap();
    let v = testing::read_wav(&stem_path(&r2, "vocals"));
    assert_eq!(v.channels(), 1);
    assert_eq!(v.sample_rate, 22_050);
    assert_eq!(v.frames(), mono.frames());

    // a 6-stem request with a 4-stem model is a usage error
    let e = run(&r, &[("model", mp), ("stems", "6")], |c| c.globals.overwrite = true).err().unwrap();
    assert_eq!(e.exit_code, 2);
}

#[test]
fn two_stem_model_switches_to_two_stems() {
    let m = mix(0.4, 44_100);
    let r = setup("sep-2stem", &m);
    let dir = TempDir::new("sep-2stem-model");
    let mut delta = vec![0.0; K];
    delta[K / 2] = 0.5;
    let model = toy_model(&dir, "voc", &[delta.clone(), delta], &["vocals", "instrumental"]);
    let (v, _) = run(&r, &[("model", model.to_str().unwrap())], |_| {}).unwrap();
    let names: Vec<&str> = v["stems"].as_array().unwrap().iter().map(|s| s["stem"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["vocals", "instrumental"]);
    let e =
        run(&r, &[("model", model.to_str().unwrap()), ("stems", "4")], |c| c.globals.overwrite = true).err().unwrap();
    assert_eq!(e.hint.as_deref(), Some("pass --stems 2"));
}

#[test]
fn dry_run_plans_without_writing() {
    let r = setup("sep-dry", &mix(0.3, 44_100));
    let (v, ctx) = run(&r, &[("tier", "fast")], |c| c.globals.dry_run = true).unwrap();
    assert_eq!(v["plan"][0]["model"], "separate/htdemucs@v4-onnx.2026-05-21");
    assert_eq!(v["plan"][0]["cached"], false);
    assert_eq!(v["outputs"].as_array().unwrap().len(), 4);
    assert!(ctx.outputs.is_empty() || !stem_path(&r, "vocals").exists());
    assert!(!stem_path(&r, "vocals").exists());

    // cascades are planned with their models and extra outputs
    let pairs =
        [("tier", "fast"), ("vocal-split", "true"), ("drum-split", "true"), ("extended", "guitar,piano,strings")];
    let (v, _) = run(&r, &pairs, |c| c.globals.dry_run = true).unwrap();
    let c = v["cascades"].as_array().unwrap();
    assert_eq!(c.len(), 3);
    assert_eq!((c[0]["stage"].as_str(), c[0]["available"].as_bool()), (Some("vocal-split"), Some(false)));
    assert!(c[0]["hint"].as_str().unwrap().contains("--cascade-models vocal-split="));
    assert_eq!(c[1]["stage"], "drum-split");
    assert_eq!(c[1]["models"][0]["model"], "separate/drumsep-inagoy@2022");
    let drum_outs: Vec<&str> =
        c[1]["outputs"].as_array().unwrap().iter().map(|o| o["stem"].as_str().unwrap()).collect();
    assert_eq!(drum_outs, vec!["kick", "snare", "toms", "cymbals"], "no hi-hat output: hi-hats land in cymbals");
    assert!(c[1]["outputs"][0]["path"].as_str().unwrap().ends_with("song.kick.wav"));
    assert_eq!(c[2]["stage"], "extended");
    assert_eq!(c[2]["models"][0]["model"], "separate/htdemucs-6s@v4-onnx.2026-05-21");
    assert_eq!(c[2]["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(c[2]["unavailable"], json!(["strings"]));
    assert!(!r.dir.path("song.kick.wav").exists());
    // auto resolves to the best verified tier not held back by auto_skip, records it, and does
    // not warn (skipping quality is a release decision, not a missing model)
    let (v, ctx) = run(&r, &[], |c| c.globals.dry_run = true).unwrap();
    assert_eq!(ctx.notes.get("tier").and_then(|t| t.as_str()), Some("balanced (auto)"));
    assert_eq!(v["plan"][0]["model"], "separate/htdemucs-ft-drums@v4-onnx.2026-05-21");
    assert!(ctx.warnings.iter().all(|w| w.code != "tier_fallback"));
    // --stems 2 keeps only the members that produce vocals
    let (v, _) = run(&r, &[("tier", "max"), ("stems", "2")], |c| c.globals.dry_run = true).unwrap();
    let models: Vec<&str> = v["plan"].as_array().unwrap().iter().map(|m| m["model"].as_str().unwrap()).collect();
    assert_eq!(models, vec!["separate/mel-roformer-kj@2024.08", "separate/htdemucs-ft-vocals@v4-onnx.2026-05-21"]);
    // --only narrows the planned outputs
    let (v, _) = run(&r, &[("tier", "fast"), ("only", "vocals")], |c| c.globals.dry_run = true).unwrap();
    assert_eq!(v["outputs"].as_array().unwrap().len(), 1);
}

#[test]
fn memory_plans() {
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    let min = |m: f64| (m * 60.0 * 44_100.0) as usize;
    let demucs = [(5200 * MB, 1500 * MB)];
    let roformer = [(6000 * MB, 3800 * MB), (5200 * MB, 1500 * MB)];
    // unknown model peak, short song: everything in memory, full sessions
    let p = plan_memory(2 * GB, min(0.2), 2, 9, &[(0, 0)]);
    assert!(!p.spill && !p.lean && p.fits);
    // htdemucs at 2 GB: a short song stays in memory but needs a lean session ...
    let p = plan_memory(2 * GB, min(4.0), 2, 5, &demucs);
    assert!(!p.spill && p.lean && p.fits, "{p:?}");
    // ... an hour spills as well
    let p = plan_memory(2 * GB, min(60.0), 2, 5, &demucs);
    assert!(p.spill && p.lean && p.fits && p.estimate <= 2 * GB, "{p:?}");
    // with 8 GB an hour spills but keeps the fast session
    let p = plan_memory(8 * GB, min(60.0), 2, 5, &demucs);
    assert!(p.spill && !p.lean && p.fits, "{p:?}");
    // the RoFormer alone exceeds 2 GB: lean, and spill only when the signals are large
    let p = plan_memory(2 * GB, min(1.0), 2, 5, &roformer);
    assert!(!p.fits && p.lean && !p.spill, "{p:?}");
    let p = plan_memory(2 * GB, min(60.0), 2, 5, &roformer);
    assert!(!p.fits && p.lean && p.spill && p.estimate < 4 * GB, "{p:?}");
    // blocks are at least 16384 frames (the assembly margins reach 6144 samples back)
    assert!(plan_memory(MB, min(1.0), 2, 20, &[]).block >= 1 << 14);
}

fn spill_dirs() -> Vec<PathBuf> {
    let prefix = format!("{}{}-", stream::SPILL_PREFIX, std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect()
}

/// A tiny `--max-memory` spills every signal to temp files: the stems are identical to the
/// in-memory run, and the spill directory is gone after success, failure, and cancel.
#[test]
fn max_memory_spills_to_temp_files_with_identical_results() {
    let _g = stream::tests::spill_lock();
    let m = mix(2.0, 44_100);
    let r = setup("sep-spill", &m);
    let dir = TempDir::new("sep-spill-model");
    let a = toy_model(&dir, "a", &kernels(), &["drums", "bass", "other", "vocals"]);
    let k2: Vec<Vec<f32>> = kernels().iter().map(|k| k.iter().map(|v| v * 0.8).collect()).collect();
    let b = toy_model(&dir, "b", &k2, &["drums", "bass", "other", "vocals"]);
    let ens = format!("{},{}", a.display(), b.display());
    let pairs = [("ensemble", ens.as_str()), ("shifts", "2"), ("segment", "0.2s")];
    let (v, _) = run(&r, &pairs, |c| c.globals.overwrite = true).unwrap();
    assert_eq!(v["memory"]["spilled"], false);
    let in_memory: Vec<Vec<u8>> = STEMS_4.iter().map(|s| std::fs::read(stem_path(&r, s)).unwrap()).collect();
    let quality_mem = v["stems"].clone();

    let small = |c: &mut OpCtx| {
        c.globals.overwrite = true;
        c.globals.max_memory = 4 << 20;
    };
    let (v, ctx) = run(&r, &pairs, small).unwrap();
    assert_eq!(v["memory"]["spilled"], true, "{}", v["memory"]);
    assert!(v["memory"]["block_frames"].as_u64().unwrap() < m.frames() as u64, "several blocks");
    assert!(ctx.warnings.iter().all(|w| w.code != "memory_budget_exceeded"));
    for (s, want) in STEMS_4.iter().zip(&in_memory) {
        assert!(std::fs::read(stem_path(&r, s)).unwrap() == *want, "{s} differs when spilled");
    }
    for (x, y) in v["stems"].as_array().unwrap().iter().zip(quality_mem.as_array().unwrap()) {
        let (qx, qy) = (x["quality"]["score"].as_f64().unwrap(), y["quality"]["score"].as_f64().unwrap());
        assert!((qx - qy).abs() < 1e-9, "{qx} vs {qy}");
    }
    assert!(spill_dirs().is_empty(), "spill files removed after success");

    // cancel: the run stops with exit 8 and leaves no spill files
    let e = run(&r, &pairs, |c| {
        small(c);
        c.cancel.cancel();
    })
    .err()
    .unwrap();
    assert_eq!(e.exit_code, 8, "{e}");
    assert!(spill_dirs().is_empty(), "spill files removed after cancel");

    // failure in a cascade after the main stems: no spill files either
    let e = run(&r, &[("model", a.to_str().unwrap()), ("vocal-split", "true")], small).err().unwrap();
    assert_eq!(e.exit_code, 4);
    assert!(spill_dirs().is_empty(), "spill files removed after failure");
}

/// `SimpleIo` without its streaming reader and writer: the whole-buffer read and encode path.
struct WholeIo;

impl ferraus_core::AudioIo for WholeIo {
    fn read(&self, path: &Path) -> Result<(AudioBuffer, ferraus_core::SourceInfo)> {
        ferraus_core::wav::SimpleIo.read(path)
    }
    fn read_bytes(&self, bytes: &[u8], hint: Option<&str>) -> Result<(AudioBuffer, ferraus_core::SourceInfo)> {
        ferraus_core::wav::SimpleIo.read_bytes(bytes, hint)
    }
    fn probe(&self, path: &Path) -> Result<ferraus_core::SourceInfo> {
        ferraus_core::wav::SimpleIo.probe(path)
    }
    fn write(&self, path: &Path, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<()> {
        ferraus_core::wav::SimpleIo.write(path, buf, opts)
    }
    fn write_bytes(&self, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<Vec<u8>> {
        ferraus_core::wav::SimpleIo.write_bytes(buf, opts)
    }
    fn can_encode(&self, ext: &str) -> bool {
        ext == "wav"
    }
}

static WHOLE_IO: WholeIo = WholeIo;

/// Streaming the input into its track and the stems into their files gives byte-identical
/// stems to decoding and encoding whole buffers, in memory and spilled, mono and stereo.
#[test]
fn streamed_input_and_stems_are_identical_to_whole_buffer_io() {
    let _g = stream::tests::spill_lock();
    let dir = TempDir::new("sep-stream-io-model");
    let model = toy_model(&dir, "a", &kernels(), &["drums", "bass", "other", "vocals"]);
    let model = model.display().to_string();
    for (mono, spill) in [(false, false), (false, true), (true, true)] {
        let mut m = mix(1.3, 44_100);
        if mono {
            m = m.with_channels(1);
        }
        let r = setup(&format!("sep-stream-io-{mono}-{spill}"), &m);
        let pairs = [("model", model.as_str()), ("segment", "0.2s")];
        let tweak = |c: &mut OpCtx| {
            c.globals.overwrite = true;
            if spill {
                c.globals.max_memory = 1 << 20;
            }
        };
        let (v, ctx) = run(&r, &pairs, |c| {
            tweak(c);
            c.io = &WHOLE_IO;
        })
        .unwrap();
        assert_eq!(v["memory"]["spilled"], spill);
        let inputs_whole = serde_json::to_value(&ctx.input_records).unwrap();
        let whole: Vec<Vec<u8>> = STEMS_4.iter().map(|s| std::fs::read(stem_path(&r, s)).unwrap()).collect();
        let (v, ctx) = run(&r, &pairs, tweak).unwrap();
        assert_eq!(v["memory"]["spilled"], spill);
        assert_eq!(serde_json::to_value(&ctx.input_records).unwrap(), inputs_whole, "the input is recorded alike");
        for (s, want) in STEMS_4.iter().zip(&whole) {
            assert!(std::fs::read(stem_path(&r, s)).unwrap() == *want, "{s} (mono {mono}, spilled {spill}) differs");
        }
    }
    assert!(spill_dirs().is_empty());
}

#[test]
fn cascades_extended_and_partial_results_survive_failure() {
    let m = mix(0.5, 44_100);
    let r = setup("sep-cascade", &m);
    let dir = TempDir::new("sep-cascade-model");
    let model = toy_model(&dir, "toy", &kernels(), &["drums", "bass", "other", "vocals"]);
    let mut k6 = kernels();
    k6.push(kernels()[1].clone());
    k6.push(kernels()[0].clone());
    let six = toy_model(&dir, "six", &k6, &["drums", "bass", "other", "vocals", "guitar", "piano"]);
    let mp = model.to_str().unwrap();
    let ext = format!("extended={}", six.display());
    let (v, _) =
        run(&r, &[("model", mp), ("extended", "guitar,piano,strings"), ("cascade-models", &ext)], |_| {}).unwrap();
    let c = &v["cascades"][0];
    assert_eq!(c["stage"], "extended");
    assert_eq!(c["best_effort"], true);
    assert_eq!(c["stems"], json!(["guitar", "piano"]));
    assert_eq!(c["unavailable"], json!(["strings"]));
    assert!(stem_path(&r, "guitar").exists());
    let g = v["stems"].as_array().unwrap().iter().find(|s| s["stem"] == "guitar").unwrap();
    assert_eq!(g["quality"]["best_effort"], true);

    // vocal-split has no license-clean registry model: the cascade fails (exit 4) but the
    // main stems were already written
    let r2 = setup("sep-partial", &m);
    let e = run(&r2, &[("model", mp), ("vocal-split", "true")], |_| {}).err().unwrap();
    assert_eq!(e.exit_code, 4, "{e}");
    for s in STEMS_4 {
        assert!(stem_path(&r2, s).exists(), "{s} survives the failed cascade");
    }
    // drum-split resolves the registry tier → not cached → exit 5 with the pull hint
    let e = run(&r2, &[("model", mp), ("drum-split", "true")], |c| c.globals.overwrite = true).err().unwrap();
    assert_eq!(e.exit_code, 5);
    assert_eq!(e.hint.as_deref(), Some("ferraus models pull separate/drumsep-inagoy@2022"));
}

#[test]
fn bad_parameters() {
    let r = setup("sep-bad", &mix(0.2, 44_100));
    let dir = TempDir::new("sep-bad-model");
    let model = toy_model(&dir, "toy", &kernels(), &["drums", "bass", "other", "vocals"]);
    let mp = model.to_str().unwrap();
    assert_eq!(run(&r, &[("model", mp), ("overlap", "99%")], |_| {}).err().unwrap().exit_code, 2);
    assert_eq!(run(&r, &[("model", mp), ("extended", "kazoo")], |_| {}).err().unwrap().exit_code, 2);
    assert_eq!(run(&r, &[("model", mp), ("only", "kazoo")], |_| {}).err().unwrap().exit_code, 2);
    assert_eq!(run(&r, &[("model", "/nope/missing.onnx")], |_| {}).err().unwrap().exit_code, 3);
}

/// MDX adapter with a generated spectrogram model (`y = 0.5 x` on `[1, 4, F, T]`).
#[test]
fn mdx_adapter_round_trip() {
    isolate_cache();
    let dir = TempDir::new("sep-mdx");
    let (n_fft, hop, dim_t) = (256usize, 64usize, 32usize);
    let dim_f = n_fft / 2 + 1;
    let shape = vec![Dim::Fixed(1), Dim::Fixed(4), Dim::Fixed(dim_f as i64), Dim::Fixed(dim_t as i64)];
    let bytes = GraphBuilder::new()
        .input("input", shape.clone())
        .output("output", shape)
        .init_f32("g", vec![1], vec![0.5])
        .node(node("Mul", &["input", "g"], &["output"]))
        .build();
    let _ = Attr::Int(0);
    let p = dir.path("mdx.onnx");
    std::fs::write(&p, bytes).unwrap();
    let spec = ModelSpec {
        name: "mdx".into(),
        version: "t".into(),
        task: "separate".into(),
        sha256: None,
        sample_rate: 44_100,
        stems: vec!["vocals".into(), "instrumental".into()],
        arch: "mdx".into(),
        inputs: vec![],
        outputs: vec![],
        chunk_seconds: None,
        chunk_samples: None,
        overlap: 0.25,
        context_samples: 0,
        provider_hints: vec![],
        license: Default::default(),
        params: json!({"n_fft": n_fft, "hop": hop, "dim_f": dim_f, "dim_t": dim_t, "compensate": 1.0}),
    };
    let (session, _) = ferraus_ml::load(
        &p,
        &ferraus_ml::SessionOptions {
            device: "cpu".into(),
            deterministic: true,
            threads: 1,
            provider_hints: vec![],
            lean: false,
        },
    )
    .unwrap();
    let model = adapter::open("mdx", spec, session).unwrap();
    assert_eq!(model.segment(), hop * (dim_t - 1));
    let m = mix(0.3, 44_100);
    let out = pipeline::separate_buffer(model.as_ref(), &m, &RunOptions::default(), &mut |_| Ok(())).unwrap();
    let mut half = m.clone();
    half.scale(0.5);
    assert!(testing::rms_diff(&out[0].1, &half) < 1e-4, "primary = 0.5 mix");
    assert!(testing::rms_diff(&out[1].1, &half) < 1e-4, "complement = mix - primary");
}

/// Hybrid-Demucs adapter (spectrogram outside the graph) with a generated model whose
/// `freq_out` passes the spectrogram through (stem 0) or halves it (stem 1) and whose
/// `time_out` is 0 (stem 0) or a quarter of the mix (stem 1): `_ispec(_spec(x))` must give x
/// back away from the window edges, so stem 0 ≈ x and stem 1 ≈ 0.75 x.
#[test]
fn hdemucs_spec_adapter_round_trip() {
    isolate_cache();
    let dir = TempDir::new("sep-hdemucs");
    let (n_fft, hop, t) = (256usize, 64usize, 2048usize);
    let (f, fr) = (n_fft / 2, t / hop);
    let d = |v: &[usize]| v.iter().map(|x| Dim::Fixed(*x as i64)).collect::<Vec<_>>();
    let bytes = GraphBuilder::new()
        .input("mix", d(&[1, 2, t]))
        .input("mag", d(&[1, 4, f, fr]))
        .output("freq_out", d(&[1, 2, 4, f, fr]))
        .output("time_out", d(&[1, 2, 2, t]))
        .init_i64("fshape", vec![5], vec![1, 1, 4, f as i64, fr as i64])
        .init_i64("tshape", vec![4], vec![1, 1, 2, t as i64])
        .init_f32("half", vec![1], vec![0.5])
        .init_f32("zero", vec![1], vec![0.0])
        .init_f32("quarter", vec![1], vec![0.25])
        .node(node("Reshape", &["mag", "fshape"], &["f0"]))
        .node(node("Mul", &["f0", "half"], &["f1"]))
        .node(node("Concat", &["f0", "f1"], &["freq_out"]).attr("axis", Attr::Int(1)))
        .node(node("Mul", &["mix", "zero"], &["t0m"]))
        .node(node("Mul", &["mix", "quarter"], &["t1m"]))
        .node(node("Reshape", &["t0m", "tshape"], &["t0"]))
        .node(node("Reshape", &["t1m", "tshape"], &["t1"]))
        .node(node("Concat", &["t0", "t1"], &["time_out"]).attr("axis", Attr::Int(1)))
        .build();
    let p = dir.path("hd.onnx");
    std::fs::write(&p, bytes).unwrap();
    let spec = ModelSpec {
        name: "hd".into(),
        version: "t".into(),
        task: "separate".into(),
        sha256: None,
        sample_rate: 44_100,
        stems: vec!["kick".into(), "snare".into()],
        arch: "hdemucs-spec".into(),
        inputs: vec![],
        outputs: vec![],
        chunk_seconds: None,
        chunk_samples: None,
        overlap: 0.25,
        context_samples: 0,
        provider_hints: vec![],
        license: Default::default(),
        params: json!({"n_fft": n_fft, "hop": hop}),
    };
    let (session, _) = ferraus_ml::load(
        &p,
        &ferraus_ml::SessionOptions {
            device: "cpu".into(),
            deterministic: true,
            threads: 1,
            provider_hints: vec![],
            lean: false,
        },
    )
    .unwrap();
    let model = adapter::open("hd", spec, session).unwrap();
    assert_eq!((model.segment(), model.fixed_segment()), (t, true));
    // no energy at Nyquist (Demucs' _spec drops that bin)
    let secs = t as f64 / 44_100.0 + 0.01;
    let m = testing::add(&testing::sine(44_100, 110.0, secs, 0.3), &testing::chirp(44_100, 500.0, 6000.0, secs, 0.15));
    let w: Vec<Vec<f32>> = vec![m.data[0][..t].to_vec(), m.data[0][..t].iter().map(|v| -0.8 * v).collect()];
    let out = model.process(&w).unwrap();
    assert_eq!((out.len(), out[0].len(), out[0][0].len()), (2, 2, t));
    for c in 0..2 {
        let mid = n_fft..t - n_fft;
        let e0 = mid.clone().map(|i| (out[0][c][i] - w[c][i]).abs()).fold(0f32, f32::max);
        let e1 = mid.map(|i| (out[1][c][i] - 0.75 * w[c][i]).abs()).fold(0f32, f32::max);
        assert!(e0 < 2e-3 && e1 < 2e-3, "channel {c}: {e0} {e1}");
    }
}

/// Real-model check: set FERRAUS_HTDEMUCS_ONNX to the verified htdemucs ONNX file
/// (sha256 d05c269d…) and run `cargo test -p ferraus-separate -- --ignored real_htdemucs`.
/// The file is installed into a temporary cache exactly as `models pull` would, then the
/// `fast` tier separates a synthetic mix through the registry path.
#[test]
#[ignore]
fn real_htdemucs_fast_tier() {
    isolate_cache();
    let src = std::env::var("FERRAUS_HTDEMUCS_ONNX").expect("set FERRAUS_HTDEMUCS_ONNX");
    let manifest = ferraus_models::Manifest::embedded();
    let entry = manifest.resolve("separate/htdemucs").unwrap().clone();
    let cache = Cache::default_root();
    let dest = cache.model_path(&entry);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dest).unwrap();
    let sha = ferraus_models::download::sha256_file(&dest).unwrap();
    assert_eq!(Some(sha.as_str()), entry.sha256.as_deref(), "registry hash matches the file");
    cache.record(&entry, &sha, std::fs::metadata(&dest).unwrap().len(), None).unwrap();

    // synthetic mix: a "bass" line, a "drum" click track, a "vocal"-like vibrato tone, noise
    let sr = 44_100;
    let secs = 12.0;
    let bass = testing::sine(sr, 55.0, secs, 0.35);
    let n = bass.frames();
    let mut drums = vec![0f32; n];
    let mut i = 0;
    while i < n {
        for j in 0..2000.min(n - i) {
            let env = (-(j as f32) / 300.0).exp();
            drums[i + j] += env * 0.6 * if j % 7 < 3 { 1.0 } else { -1.0 };
        }
        i += sr as usize / 2;
    }
    let drums = AudioBuffer::from_mono(sr, drums);
    let vocal = AudioBuffer::from_mono(
        sr,
        (0..n)
            .map(|k| {
                let t = k as f64 / sr as f64;
                let f = 440.0 + 15.0 * (2.0 * std::f64::consts::PI * 5.0 * t).sin();
                let ph = 2.0
                    * std::f64::consts::PI
                    * (440.0 * t
                        - 15.0 / (2.0 * std::f64::consts::PI * 5.0) * (2.0 * std::f64::consts::PI * 5.0 * t).cos());
                let _ = f;
                (0.25 * (ph.sin() + 0.5 * (2.0 * ph).sin() + 0.25 * (3.0 * ph).sin())) as f32
            })
            .collect(),
    );
    let m = testing::add(&testing::add(&bass, &drums), &vocal).with_channels(2);
    let r = setup("sep-real", &m);
    let start = std::time::Instant::now();
    let (v, ctx) = run(&r, &[("tier", "fast")], |c| {
        c.globals.deterministic = false;
        c.globals.threads = 0;
        c.globals.device = "cpu".into();
    })
    .unwrap();
    let elapsed = start.elapsed().as_secs_f64();
    let get = |s: &str| testing::read_wav(&stem_path(&r, s));
    let sdr = |reference: &AudioBuffer, s: &str| testing::si_sdr(&reference.data[0], &get(s).data[0]);
    let report = json!({
        "seconds": elapsed,
        "realtime_factor": secs / elapsed,
        "si_sdr_db": {"bass": sdr(&bass, "bass"), "drums": sdr(&drums, "drums"), "vocals": sdr(&vocal, "vocals")},
        "quality": v["stems"],
        "provider": ctx.notes.get("execution_provider"),
    });
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(sdr(&bass, "bass") > 3.0, "{report}");
}

/// Install every verified separation model whose download file (the basename of its first
/// URL) is in `dir` into the test cache, checking the registry hash. Returns the installed ids.
fn install_from_dir(dir: &Path) -> Vec<String> {
    let manifest = ferraus_models::Manifest::embedded();
    let cache = Cache::default_root();
    let mut ids = Vec::new();
    for e in manifest.models.iter().filter(|e| e.task == "separate" && e.sha256.is_some()) {
        let Some(base) = e.urls.first().and_then(|u| u.rsplit('/').next()) else { continue };
        let src = dir.join(base);
        if !src.exists() {
            continue;
        }
        let dest = cache.model_path(e);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        if std::fs::hard_link(&src, &dest).is_err() {
            std::fs::copy(&src, &dest).unwrap();
        }
        let sha = ferraus_models::download::sha256_file(&dest).unwrap();
        assert_eq!(Some(sha.as_str()), e.sha256.as_deref(), "{}: registry hash matches the file", e.id());
        cache.record(e, &sha, std::fs::metadata(&dest).unwrap().len(), None).unwrap();
        ids.push(e.id());
    }
    ids
}

/// The newly verified registry models run through their adapters: set FERRAUS_REAL_MODEL_DIR to
/// a directory holding the downloaded files under their URL names (htdemucs_fp16weights.onnx,
/// htdemucs_ft_{drums,bass,other,vocals}_fp16weights.onnx, htdemucs_6s_fp16weights.onnx,
/// mel-roformer.fp16.onnx) and run `cargo test --release -p ferraus-separate -- --ignored
/// real_registry_tiers --nocapture`. Each tier separates the same synthetic mix; the report
/// gives SI-SDR per stem, time, and the memory plan. FERRAUS_REAL_TIERS overrides the tier list
/// (default `fast,six,balanced,quality`; add `max` for the ensemble).
#[test]
#[ignore]
fn real_registry_tiers() {
    isolate_cache();
    let dir = std::env::var("FERRAUS_REAL_MODEL_DIR").expect("set FERRAUS_REAL_MODEL_DIR");
    let ids = install_from_dir(Path::new(&dir));
    eprintln!("installed: {ids:?}");
    let sr = 44_100;
    let secs = 12.0;
    let bass = testing::sine(sr, 55.0, secs, 0.35);
    let n = bass.frames();
    let mut drums = vec![0f32; n];
    let mut i = 0;
    while i < n {
        for j in 0..2000.min(n - i) {
            drums[i + j] += (-(j as f32) / 300.0).exp() * 0.6 * if j % 7 < 3 { 1.0 } else { -1.0 };
        }
        i += sr as usize / 2;
    }
    let drums = AudioBuffer::from_mono(sr, drums);
    let m = testing::add(&bass, &drums).with_channels(2);
    let tiers = std::env::var("FERRAUS_REAL_TIERS").unwrap_or_else(|_| "fast,six,balanced,quality".into());
    let mut reports = Vec::new();
    for t in tiers.split(',') {
        let r = setup(&format!("sep-real-{t}"), &m);
        let mut pairs = vec![("tier", if t == "six" { "fast" } else { t })];
        if t == "six" {
            pairs.push(("stems", "6"));
        }
        let start = std::time::Instant::now();
        let (v, ctx) = run(&r, &pairs, |c| {
            c.globals.deterministic = false;
            c.globals.threads = 0;
        })
        .unwrap_or_else(|e| panic!("tier {t}: {e}"));
        let names: Vec<String> =
            v["stems"].as_array().unwrap().iter().map(|s| s["stem"].as_str().unwrap().to_string()).collect();
        let want = if t == "six" { STEMS_6.len() } else { 4 };
        assert_eq!(names.len(), want, "{t}: {names:?}");
        let get = |s: &str| testing::read_wav(&stem_path(&r, s));
        let sdr = |reference: &AudioBuffer, s: &str| testing::si_sdr(&reference.data[0], &get(s).data[0]);
        // every tier's stems add back to the input
        let mut sum = AudioBuffer::new(sr, 2, n);
        for s in &names {
            sum = testing::add(&sum, &get(s));
        }
        assert!(testing::rms_diff(&sum, &m) < 1e-6, "{t}: stems sum to the mix");
        let rep = json!({
            "tier": t,
            "seconds": start.elapsed().as_secs_f64(),
            "models": v["models"].as_array().unwrap().iter().map(|m| m["model"].clone()).collect::<Vec<_>>(),
            "si_sdr_db": {"bass": sdr(&bass, "bass"), "drums": sdr(&drums, "drums")},
            "memory": v["memory"],
            "warnings": ctx.warnings.iter().map(|w| w.code.clone()).collect::<Vec<_>>(),
        });
        eprintln!("{}", serde_json::to_string_pretty(&rep).unwrap());
        assert!(rep["si_sdr_db"]["bass"].as_f64().unwrap() > 3.0, "{rep}");
        reports.push(rep);
    }
}

/// Real MDX-Net check: set FERRAUS_KIM_VOCAL_2_ONNX to the verified Kim_Vocal_2.onnx
/// (sha256 ce74ef3b…) and run with `--ignored real_mdx`. The model has no clear license, so it
/// is only reachable through `--model` and must warn.
#[test]
#[ignore]
fn real_mdx_kim_vocal_2() {
    isolate_cache();
    let src = std::env::var("FERRAUS_KIM_VOCAL_2_ONNX").expect("set FERRAUS_KIM_VOCAL_2_ONNX");
    let manifest = ferraus_models::Manifest::embedded();
    let entry = manifest.resolve("separate/mdx-kim-vocal-2").unwrap().clone();
    let cache = Cache::default_root();
    let dest = cache.model_path(&entry);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dest).unwrap();
    let sha = ferraus_models::download::sha256_file(&dest).unwrap();
    assert_eq!(Some(sha.as_str()), entry.sha256.as_deref());
    cache.record(&entry, &sha, std::fs::metadata(&dest).unwrap().len(), None).unwrap();
    let sr = 44_100;
    let secs = 10.0;
    let n = (sr as f64 * secs) as usize;
    let vocal = AudioBuffer::from_mono(
        sr,
        (0..n)
            .map(|k| {
                let t = k as f64 / sr as f64;
                let ph = 2.0 * std::f64::consts::PI * (330.0 * t + 2.0 * (2.0 * std::f64::consts::PI * 5.0 * t).sin());
                let env = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * 0.5 * t).sin();
                (0.3 * env * (ph.sin() + 0.4 * (2.0 * ph).sin() + 0.2 * (3.0 * ph).sin())) as f32
            })
            .collect(),
    );
    let bed = testing::add(&testing::sine(sr, 82.4, secs, 0.3), &testing::noise(sr, secs, 0.05, 5));
    let m = testing::add(&vocal, &bed).with_channels(2);
    let r = setup("sep-real-mdx", &m);
    let start = std::time::Instant::now();
    let (v, ctx) = run(&r, &[("model", "separate/mdx-kim-vocal-2")], |c| {
        c.globals.deterministic = false;
        c.globals.threads = 0;
    })
    .unwrap();
    let voc = testing::read_wav(&stem_path(&r, "vocals"));
    let inst = testing::read_wav(&stem_path(&r, "instrumental"));
    let report = json!({
        "seconds": start.elapsed().as_secs_f64(),
        "si_sdr_vocals_db": testing::si_sdr(&vocal.data[0], &voc.data[0]),
        "si_sdr_instrumental_db": testing::si_sdr(&bed.data[0], &inst.data[0]),
        "stems": v["stems"],
        "warnings": ctx.warnings.iter().map(|w| w.code.clone()).collect::<Vec<_>>(),
    });
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(ctx.warnings.iter().any(|w| w.code == "model_license_unclear"));
    assert!(report["si_sdr_vocals_db"].as_f64().unwrap() > 0.0, "{report}");
}
