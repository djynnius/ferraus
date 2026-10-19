//! End to end through the shared executor (the path the CLI, pipelines, and MCP use):
//! output naming, the `.noiseprof` default extension, notes in the envelope, residual
//! outputs, sidecars, and dry runs.

mod common;

use common::*;
use ferraus_core::exec::{Invocation, PresetLookup, Runtime};
use ferraus_core::testing::{self, TempDir};
use ferraus_core::Globals;

fn runtime() -> Runtime<'static> {
    let presets: &'static PresetLookup = &|_, _| None;
    Runtime {
        registry: testing::leak_registry(ferraus_denoise::ops()),
        io: &testing::SIMPLE_IO,
        progress: &testing::NO_PROGRESS,
        presets,
        cancel: Default::default(),
    }
}

fn inv(cmd: &str, args: &[(&str, &str)], inputs: &[&std::path::Path], output: Option<&std::path::Path>) -> Invocation {
    Invocation {
        command: cmd.into(),
        args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        inputs: inputs.iter().map(|p| p.display().to_string()).collect(),
        globals: Globals { output: output.map(|p| p.display().to_string()), ..Default::default() },
        argv: vec!["ferraus".into(), cmd.into()],
        stdin: None,
    }
}

#[test]
fn profile_apply_merge_through_the_executor() {
    let dir = TempDir::new("rt");
    let clean = speech_like(1.0, 1.5, 0.5);
    let nz = make_noise(NoiseKind::Fan, 2.5, 21);
    let (noisy, _) = mix(&clean.buf, &nz, 10.0, SR as usize);
    let take = dir.path("take.wav");
    testing::write_wav(&take, &noisy);
    let rt = runtime();

    // learn: default name is <input>.denoise-profile.noiseprof beside the input
    let o = rt.invoke(inv("denoise profile", &[("auto", "true")], &[&take], None));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let prof = dir.path("take.denoise-profile.noiseprof");
    assert_eq!(o.envelope.outputs[0].path, prof.display().to_string());
    assert!(prof.exists());
    assert!(o.envelope.params.contains_key("noise_region"));
    // the noise-only lead-in ends at 1.0 s (speech starts at 1.05 s)
    assert!(o.envelope.result["noise_region"]["end_s"].as_f64().unwrap() <= 1.1);

    // apply with a residual
    let out = dir.path("clean.wav");
    let res = dir.path("removed.wav");
    let p = prof.display().to_string();
    let r = res.display().to_string();
    let o = rt.invoke(inv("denoise", &[("profile", p.as_str()), ("residual", r.as_str())], &[&take], Some(&out)));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let roles: Vec<Option<String>> = o.envelope.outputs.iter().map(|x| x.role.clone()).collect();
    assert!(roles.contains(&Some("residual".into())) && roles.contains(&None), "{roles:?}");
    assert_eq!(o.envelope.params["mode"], "profile");
    assert!(o.envelope.metrics.contains_key("removed_rms_dbfs"));
    assert!(out.exists() && res.exists());
    assert!(ferraus_core::Sidecar::path_for(&out).exists());
    let y = testing::read_wav(&out);
    assert_eq!(y.frames(), noisy.frames());

    // merge two profiles
    let prof2 = dir.path("b.noiseprof");
    let o = rt.invoke(inv("denoise profile", &[("range", "0s..1s")], &[&take], Some(&prof2)));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let merged = dir.path("c.noiseprof");
    let o = rt.invoke(inv("denoise profile merge", &[("method", "mean")], &[&prof, &prof2], Some(&merged)));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    assert_eq!(o.envelope.result["method"], "merge-mean");
    assert!(merged.exists());

    // dry run writes nothing
    let dry = dir.path("dry.noiseprof");
    let mut i = inv("denoise profile", &[("auto", "true")], &[&take], Some(&dry));
    i.globals.dry_run = true;
    let o = rt.invoke(i);
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    assert!(!dry.exists());

    // missing model: exit code 5 in the envelope
    let o = rt.invoke(inv("denoise", &[("mode", "ml"), ("model", "music")], &[&take], Some(&dir.path("m.wav"))));
    assert!(!o.envelope.ok);
    assert_eq!(o.envelope.exit_code(), 5);
}
