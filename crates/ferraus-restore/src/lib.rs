//! ferraus-restore: restoration (declip, dehum, declick, dehiss, repair, remove-sound,
//! phase-align, center, leveler), Audition-parity filters (graphic-eq, fft-filter, notch,
//! scientific filter), transfer-curve dynamics, echo, doppler, distort, automation
//! (envelope, pitch-bend), generators, band-split, the channel mixer, and one-knob
//! chains (enhance-voice, enhance, guitar).
//!
//! All DSP lives in [`dsp`] (FFT/STFT, biquads, AR interpolation, filter design, pitch
//! shifting, loudness). Composite chains call other crates' ops by name at run time.

#![allow(clippy::needless_range_loop)] // index loops read naturally in DSP code

pub mod dsp;
pub mod ops;

#[cfg(test)]
pub(crate) mod testutil;

use ferraus_core::OpSpec;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    let mut v = Vec::new();
    v.extend(ops::clicks::ops());
    v.extend(ops::hum::ops());
    v.extend(ops::spectral::ops());
    v.extend(ops::stereo::ops());
    v.extend(ops::level::ops());
    v.extend(ops::filters::ops());
    v.extend(ops::space::ops());
    v.extend(ops::automation::ops());
    v.extend(ops::generate::ops());
    v.extend(ops::chains::ops());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing;
    use ferraus_core::{Args, AudioBuffer, Registry, Runner};

    const GLOBALS: &[&str] = &[
        "output",
        "outdir",
        "format",
        "input-format",
        "json",
        "progress",
        "quiet",
        "verbose",
        "threads",
        "device",
        "deterministic",
        "seed",
        "offline",
        "yes",
        "overwrite",
        "in-place",
        "sidecar",
        "no-sidecar",
        "dry-run",
        "explain",
        "force",
        "sample-rate",
        "bit-depth",
        "channels",
        "bitrate",
        "config",
        "sound-path",
        "bpm",
        "async",
        "timeout",
        "max-memory",
    ];

    #[test]
    fn every_op_is_documented_and_registers() {
        let all = ops();
        let mut r = Registry::new();
        r.extend(all.clone()); // panics on duplicate names
        let expected = [
            "declip",
            "dehum",
            "declick",
            "dehiss",
            "repair",
            "remove-sound",
            "phase-align",
            "center",
            "leveler",
            "enhance-voice",
            "enhance",
            "guitar",
            "envelope",
            "pitch-bend",
            "graphic-eq",
            "fft-filter",
            "notch",
            "filter",
            "dynamics",
            "echo",
            "doppler",
            "distort",
            "generate tone",
            "generate noise",
            "generate silence",
            "band-split",
            "channels mix",
        ];
        for name in expected {
            assert!(r.get(name).is_some(), "missing op {name}");
        }
        assert_eq!(all.len(), expected.len());
        for op in &all {
            assert!(!op.about.is_empty(), "{} has no about", op.name);
            assert!(op.examples.len() >= 2, "{} needs at least two examples", op.name);
            for e in &op.examples {
                assert!(e.cmd.starts_with(&format!("ferraus {}", op.name)), "{}: {}", op.name, e.cmd);
            }
            for p in &op.params {
                assert!(!p.help.is_empty(), "{} --{} has no help", op.name, p.name);
                assert!(!GLOBALS.contains(&p.name), "{} --{} collides with a global flag", op.name, p.name);
                assert_ne!(p.name, "preset", "{}: use OpSpec::preset for presets", op.name);
                if let Some(d) = p.default {
                    p.kind.validate(d, p.choices).unwrap_or_else(|e| panic!("{} --{} default: {}", op.name, p.name, e));
                }
            }
            if let Err(e) = Args::build(op, vec![], &|_, _| None) {
                assert!(op.params.iter().any(|p| p.required), "{} fails with defaults: {}", op.name, e.message);
            }
        }
    }

    /// Required arguments for ops that cannot run with defaults alone.
    fn needed(name: &str) -> Vec<(&'static str, &'static str)> {
        match name {
            "repair" => vec![("range", "0.1s..0.2s")],
            "remove-sound" => vec![("learn", "0s..0.2s")],
            _ => vec![],
        }
    }

    #[test]
    fn every_transform_nulls_at_mix_zero() {
        let sr = 48000;
        let x = testing::stereo(testing::add(&crate::testutil::music(sr, 0.5, 0.4), &testing::noise(sr, 0.5, 0.05, 3)));
        let mut checked = 0;
        for op in ops() {
            if !op.is_transform() || op.effective_params().iter().all(|p| p.name != "mix") {
                continue;
            }
            let mut pairs = needed(op.name);
            pairs.push(("mix", "0%"));
            let a = testing::args(&op, &pairs);
            let mut c = testing::ctx();
            let out = ferraus_core::exec::apply_spec(&op, &a, x.clone(), &mut c)
                .unwrap_or_else(|e| panic!("{}: {}", op.name, e.message));
            assert_eq!(out, x, "{} is not a bit-exact null at --mix 0%", op.name);
            checked += 1;
        }
        // every transform except pitch-bend (length-changing, no mix) has a mix
        assert_eq!(checked, ops().iter().filter(|o| o.is_transform()).count() - 1);
    }

    #[test]
    fn every_transform_handles_silence_mono_and_tiny_inputs() {
        let reg = testing::leak_registry(ops());
        for op in ops() {
            let f = match op.runner {
                Runner::Transform(f) => f,
                _ => continue,
            };
            for input in [
                AudioBuffer::new(48000, 1, 0),
                AudioBuffer::new(48000, 2, 4800),
                AudioBuffer::from_mono(48000, vec![0.25, -0.5, 0.125]),
            ] {
                let a = testing::args(&op, &needed(op.name));
                let mut c = testing::ctx_with(reg);
                match f(&a, input.clone(), &mut c) {
                    Ok(out) => assert!(
                        out.data.iter().flatten().all(|v| v.is_finite()),
                        "{} produced non-finite samples",
                        op.name
                    ),
                    Err(e) => assert!(
                        matches!(e.kind, ferraus_core::ErrorKind::Usage) || e.kind.exit_code() == 3,
                        "{} failed on a degenerate input: {:?}",
                        op.name,
                        e
                    ),
                }
            }
        }
    }

    /// Real-time factor of every transform on 10 s of stereo audio. Run with
    /// `cargo test -p ferraus-restore --release -- --ignored --nocapture perf`.
    #[test]
    #[ignore]
    fn perf_real_time_factors() {
        let sr = 48000;
        let secs = 10.0;
        let base = testing::add(&crate::testutil::music(sr, secs, 0.4), &testing::noise(sr, secs, 0.01, 1));
        let (dirty, _) = crate::testutil::add_clicks(&base, 200, 3);
        let x = testing::stereo(dirty);
        let reg = testing::leak_registry(ops());
        for op in ops() {
            let f = match op.runner {
                Runner::Transform(f) => f,
                _ => continue,
            };
            let a = testing::args(&op, &needed(op.name));
            let mut c = testing::ctx_with(reg);
            let t = std::time::Instant::now();
            let _ = f(&a, x.clone(), &mut c);
            let el = t.elapsed().as_secs_f64();
            eprintln!("{:>14}: {:7.1}x real time", op.name, secs / el.max(1e-9));
        }
    }

    #[test]
    fn length_changing_ops_opt_out_of_mix() {
        let pb = ops().into_iter().find(|o| o.name == "pitch-bend").unwrap();
        assert!(!pb.auto_mix && pb.find_param("mix").is_none());
    }
}
