//! Acceptance tests for the mastering commands (PRD "Feature spec: mastering").

mod common;
use common::*;
use ferraus_analysis::spectrum::{third_octave_levels, tone_balance};
use ferraus_analysis::{loudness, stereo};
use ferraus_core::testing::{read_wav, rms_diff, TempDir};
use ferraus_core::AudioBuffer;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn style(name: &str) -> Style {
    styles().into_iter().find(|s| s.name == name).unwrap()
}

fn short(mut s: Style, secs: f64) -> Style {
    s.secs = secs;
    s
}

fn master_song(td: &TempDir, s: &Style, args: &[(&str, &str)]) -> (PathBuf, PathBuf, Value) {
    let inp = td.path(&format!("{}.wav", s.name));
    write(&inp, &song(s));
    let out = td.path(&format!("{}.master.wav", s.name));
    let o = invoke("master", &[&inp], args, Some(&out));
    assert!(o.envelope.ok, "{}: {:?}", s.name, o.envelope.error);
    (inp, out, serde_json::to_value(&o.envelope).unwrap())
}

/// Streaming loudness, true peak, and stereo correlation on one song.
fn check_streaming(name: &str, m: &AudioBuffer) -> (f64, f64, f64, f64) {
    let l = loudness::loudness(m);
    let st = stereo::stereo_analysis(m);
    assert!((l.integrated_lufs + 14.0).abs() <= 0.5, "{name}: {} LUFS", l.integrated_lufs);
    assert!(l.true_peak_dbtp <= -1.0, "{name}: {} dBTP", l.true_peak_dbtp);
    assert!(st.correlation >= 0.3, "{name}: correlation {}", st.correlation);
    assert!(st.correlation_below_120hz >= 0.9, "{name}: low correlation {}", st.correlation_below_120hz);
    (l.integrated_lufs, l.true_peak_dbtp, st.correlation, st.correlation_below_120hz)
}

#[test]
fn streaming_master_quick() {
    let td = TempDir::new("m-quick");
    for name in ["pop", "acoustic"] {
        let s = short(style(name), 5.0);
        let (_, out, env) = master_song(&td, &s, &[]);
        check_streaming(name, &read_wav(&out));
        assert_eq!(env["result"]["verify"]["pass"], true, "{}", env["result"]["verify"]);
        assert_eq!(env["metrics"]["target_reached"], true);
    }
}

/// PRD acceptance: every song lands within 0.5 LU of -14 LUFS, never exceeds -1 dBTP
/// (4x oversampled), broadband correlation >= 0.3 and >= 0.9 below 120 Hz.
#[test]
#[ignore = "acceptance set: run with --release -- --ignored"]
fn streaming_acceptance_set() {
    let td = TempDir::new("m-accept");
    let mut lines = Vec::new();
    let mut set = styles();
    // two extra variants: a very wide mix and an out-of-phase-bass mix
    let mut wide = style("pop");
    wide.name = "pop-wide";
    wide.spread = 3.0;
    wide.chords = 0.25;
    set.push(wide);
    for s in &set {
        let (_, out, env) = master_song(&td, s, &[]);
        let (l, tp, c, cl) = check_streaming(s.name, &read_wav(&out));
        lines.push(format!(
            "{:9} {l:7.2} LUFS {tp:6.2} dBTP corr {c:.2} low {cl:.2} limiter {} dB genre {}",
            s.name, env["result"]["loudness"]["limiter_gain_reduction_db"], env["result"]["genre"]["name"]
        ));
    }
    // stereo problem variants of the full-length songs
    for s in [style("edm"), style("rock")] {
        for (tag, b) in [("very-wide", very_wide(&song(&s))), ("oop-bass", out_of_phase_bass(&song(&s)))] {
            let name = format!("{}-{tag}", s.name);
            let inp = td.path(&format!("{name}.wav"));
            write(&inp, &b);
            let out = td.path(&format!("{name}.master.wav"));
            let o = invoke("master", &[&inp], &[], Some(&out));
            assert!(o.envelope.ok, "{name}: {:?}", o.envelope.error);
            let (l, tp, c, cl) = check_streaming(&name, &read_wav(&out));
            lines.push(format!("{name:15} {l:7.2} LUFS {tp:6.2} dBTP corr {c:.2} low {cl:.2}"));
        }
    }
    // every clean master passes verify
    for s in &set {
        let out = td.path(&format!("{}.master.wav", s.name));
        let v = verify(&out, &[("target", "streaming")]);
        assert_eq!(v["ok"], true, "{}: {v}", s.name);
    }
    println!("{}", lines.join("\n"));
}

/// A mix whose side signal is louder than its mid (correlation well below 0.3).
fn very_wide(b: &AudioBuffer) -> AudioBuffer {
    let mut out = b.clone();
    let s = ferraus_core::testing::noise(b.sample_rate, b.duration_s(), 0.12, 77);
    let s = ferraus_analysis::util::highpass4(&s.data[0], b.sample_rate as f64, 300.0);
    let (l, r) = out.data.split_at_mut(1);
    for ((a, b), v) in l[0].iter_mut().zip(r[0].iter_mut()).zip(&s) {
        *a += v;
        *b -= v;
    }
    out
}

/// A mix whose right-channel bass is out of polarity.
fn out_of_phase_bass(b: &AudioBuffer) -> AudioBuffer {
    let mut out = b.clone();
    // zero-phase low band (forward-backward), so subtracting it twice flips its polarity
    let sr = out.sample_rate as f64;
    let mut low = ferraus_analysis::util::lowpass4(&out.data[1], sr, 150.0);
    low.reverse();
    let mut low = ferraus_analysis::util::lowpass4(&low, sr, 150.0);
    low.reverse();
    for (v, l) in out.data[1].iter_mut().zip(&low) {
        *v -= 2.0 * l;
    }
    out
}

#[test]
fn correlation_guard_and_mono_bass() {
    let td = TempDir::new("m-corr");
    let base = song(&short(style("pop"), 5.0));
    for (name, b) in [("wide", very_wide(&base)), ("oop", out_of_phase_bass(&base))] {
        let before = stereo::stereo_analysis(&b);
        let inp = td.path(&format!("{name}.wav"));
        write(&inp, &b);
        let out = td.path(&format!("{name}.master.wav"));
        let o = invoke("master", &[&inp], &[("width", "130%")], Some(&out));
        assert!(o.envelope.ok, "{:?}", o.envelope.error);
        let (_, _, c, cl) = check_streaming(name, &read_wav(&out));
        println!(
            "{name}: correlation {:.2} -> {c:.2}, below 120 Hz {:.2} -> {cl:.2}",
            before.correlation, before.correlation_below_120hz
        );
        if name == "wide" {
            assert!(before.correlation < 0.3);
            assert!(o.envelope.warnings.iter().any(|w| w.code == "correlation_guard"));
        } else {
            assert!(before.correlation_below_120hz < 0.0);
        }
    }
}

#[test]
fn reference_match_at_100_percent() {
    let td = TempDir::new("m-ref");
    let s = short(style("pop"), 6.0);
    let mix = song(&s);
    let inp = td.path("mix.wav");
    write(&inp, &mix);
    // the reference: the same arrangement mixed with a different balance
    let refp = td.path("ref.wav");
    let o = invoke(
        "eq",
        &[&inp],
        &[("band", "lowshelf:150Hz:-3dB"), ("band", "peak:1kHz:+2dB:q=0.7"), ("band", "highshelf:5kHz:+3.5dB")],
        Some(&refp),
    );
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let out = td.path("matched.wav");
    let o = invoke("master", &[&inp], &[("reference", refp.to_str().unwrap()), ("match-amount", "100%")], Some(&out));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let m = read_wav(&out);
    let r = read_wav(&refp);
    let tb = tone_balance(&third_octave_levels(&m), &third_octave_levels(&r));
    let before = tone_balance(&third_octave_levels(&mix), &third_octave_levels(&r));
    let mut worst: f64 = 0.0;
    for b in tb.bands.iter().filter(|b| b.center_hz >= 60.0 && b.center_hz <= 12600.0) {
        worst = worst.max(b.delta_db.abs());
        assert!(b.delta_db.abs() <= 3.0, "{:.0} Hz: {:.2} dB off the reference", b.center_hz, b.delta_db);
    }
    println!(
        "reference match: worst band {worst:.2} dB (before {:.2} dB), rms {:.2} dB",
        before.largest_deviations[0].delta_db.abs(),
        tb.rms_deviation_db
    );
    assert!(tb.rms_deviation_db < before.rms_deviation_db);
    let v = serde_json::to_value(&o.envelope).unwrap();
    assert_eq!(v["result"]["tonal_reference"], "reference");
    // the corrective EQ never asks for more than 4 dB in a band
    for st in v["result"]["plan"]["stages"].as_array().unwrap() {
        if st["op"] == "eq" {
            for b in st["args"]["band"].as_array().cloned().unwrap_or_else(|| vec![st["args"]["band"].clone()]) {
                let g: f64 = b.as_str().unwrap().split(':').nth(2).unwrap().trim_end_matches("dB").parse().unwrap();
                assert!(g.abs() <= 4.0, "{b}");
            }
        }
    }
}

fn verify(path: &Path, args: &[(&str, &str)]) -> Value {
    let o = invoke("master verify", &[path], args, None);
    serde_json::to_value(&o.envelope).unwrap()
}

#[test]
fn verify_flags_each_fault_and_passes_clean_masters() {
    let td = TempDir::new("m-verify");
    let s = short(style("rock"), 6.0);
    let (_, out, _) = master_song(&td, &s, &[]);
    let clean = read_wav(&out);
    let v = verify(&out, &[("target", "streaming")]);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["result"]["pass"], true);

    let write_fault = |name: &str, b: &AudioBuffer| -> PathBuf {
        let p = td.path(&format!("fault-{name}.wav"));
        write(&p, b);
        p
    };
    // clipping: 6 dB too hot and hard-clipped at full scale
    let mut clip = clean.clone();
    clip.data.iter_mut().flatten().for_each(|v| *v = (*v * 2.0).clamp(-1.0, 1.0));
    // DC offset
    let mut dc = clean.clone();
    dc.data.iter_mut().flatten().for_each(|v| *v += 0.02);
    // a 2 dB true-peak overshoot (float, so no sample clipping)
    let mut over = clean.clone();
    let tp = loudness::true_peak_db(&clean);
    over.scale(10f64.powf((-1.0 + 2.0 - tp) / 20.0) as f32);
    // out-of-phase bass: the right channel's low band inverted
    let mut oop = clean.clone();
    let low = ferraus_analysis::util::lowpass4(&oop.data[1], oop.sample_rate as f64, 150.0);
    for (v, l) in oop.data[1].iter_mut().zip(&low) {
        *v -= 2.0 * l;
    }
    // 3 s of head silence
    let mut sil = AudioBuffer::new(clean.sample_rate, 2, 3 * clean.sample_rate as usize);
    sil.append(&clean);

    let faults = [
        ("clipping", write_fault("clipping", &clip)),
        ("dc_offset", write_fault("dc", &dc)),
        ("true_peak", write_fault("overshoot", &over)),
        ("out_of_phase_bass", write_fault("oop", &oop)),
        ("head_silence", write_fault("silence", &sil)),
    ];
    for (check, path) in &faults {
        let v = verify(path, &[("target", "streaming")]);
        assert_eq!(v["ok"], false, "{check} not flagged");
        let err = &v["error"];
        assert_eq!(err["exit_code"], 9, "{err}");
        assert_eq!(err["code"], "verification_failed");
        assert!(err["message"].as_str().unwrap().contains(check), "{check}: {}", err["message"]);
    }

    // the hint fixes the failed check on the master itself instead of mastering it again
    let silence = &faults[4].1;
    let v = verify(silence, &[("target", "streaming")]);
    let hint = v["error"]["hint"].as_str().unwrap();
    assert!(hint.starts_with("ferraus fx ") && hint.contains("--fx \"silence trim:"), "{hint}");
    let v = verify(&faults[1].1, &[("target", "streaming")]);
    assert!(v["error"]["hint"].as_str().unwrap().contains("--fx \"dc-remove\""), "{v}");
    // deliberate head silence: --max-head-silence off passes and warns
    let v = verify(silence, &[("target", "streaming"), ("max-head-silence", "off")]);
    assert_eq!(v["ok"], true, "{v}");
    let check =
        v["result"]["checks"].as_array().unwrap().iter().find(|c| c["check"] == "head_silence").unwrap().clone();
    assert_eq!(check["status"], "warn", "{check}");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["message"].as_str().unwrap().contains("head_silence")));
    // a longer explicit limit passes too; a malformed one is a usage error
    assert_eq!(verify(silence, &[("target", "streaming"), ("max-head-silence", "4s")])["ok"], true);
    assert_eq!(verify(silence, &[("target", "streaming"), ("max-head-silence", "soon")])["error"]["exit_code"], 2);
    // a mix far below the target is not fixed by normalizing: master it
    let quiet_path = td.path("quiet.wav");
    let mut quiet = clean.clone();
    quiet.scale(0.1);
    write(&quiet_path, &quiet);
    let v = verify(&quiet_path, &[("target", "streaming")]);
    assert!(v["error"]["hint"].as_str().unwrap().starts_with("ferraus master "), "{v}");
}

#[test]
fn a_plan_run_hot_into_its_limiter_does_not_warn_about_clipping() {
    let td = TempDir::new("m-hot-plan");
    let s = short(style("pop"), 6.0);
    let inp = td.path("song.wav");
    write(&inp, &song(&s));
    let plan = td.path("plan.json");
    let o = invoke("master plan", &[&inp], &[("trim-silence", "off")], Some(&plan));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    // the same song 6 dB hotter through the unchanged plan: its gain stage overshoots 0 dBFS
    // in float right before the limiter, which is not clipping
    let mut hot = song(&s);
    hot.scale(2.0);
    let hot_path = td.path("hot.wav");
    write(&hot_path, &hot);
    let out = td.path("hot.master.wav");
    let o = invoke("master", &[&hot_path], &[("plan", plan.to_str().unwrap())], Some(&out));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let v = serde_json::to_value(&o.envelope).unwrap();
    assert!(!v["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "clipping"), "{}", v["warnings"]);
    assert!(loudness::true_peak_db(&read_wav(&out)) <= -0.99);
}

#[test]
fn render_with_trim_off_keeps_the_head_silence_without_failing_verify() {
    let td = TempDir::new("m-trim-off");
    let s = short(style("pop"), 6.0);
    let mut buf = song(&s);
    let mut padded = AudioBuffer::new(buf.sample_rate, buf.channels(), 2 * buf.sample_rate as usize);
    padded.append(&buf);
    buf = padded;
    let inp = td.path("late.wav");
    write(&inp, &buf);
    let out = td.path("late.master.wav");
    let o = invoke("master", &[&inp], &[("trim-silence", "off")], Some(&out));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let v = serde_json::to_value(&o.envelope).unwrap();
    assert_eq!(v["metrics"]["verify_pass"], true, "{}", v["metrics"]);
    assert!(!v["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "verify_failed"), "{}", v["warnings"]);
}

#[test]
fn verify_reports_structured_checks_in_context() {
    // the structured list lives in metrics (visible to callers that keep the context)
    let td = TempDir::new("m-verify-ctx");
    let mut b = song(&short(style("pop"), 3.0));
    b.data.iter_mut().flatten().for_each(|v| *v += 0.05);
    let p = td.path("dc.wav");
    write(&p, &b);
    let spec = ferraus_master::ops().into_iter().find(|o| o.name == "master verify").unwrap();
    let a = ferraus_core::testing::args(&spec, &[]);
    let mut ctx = ferraus_core::testing::ctx_with(registry());
    ctx.inputs = vec![p.clone()];
    let f = match spec.runner {
        ferraus_core::Runner::Custom(f) => f,
        _ => unreachable!(),
    };
    let e = f(&a, &mut ctx).unwrap_err();
    assert_eq!(e.exit_code, 9);
    let failed = ctx.metrics["failed_checks"].as_array().unwrap();
    assert!(failed.iter().any(|c| c == "dc_offset"), "{failed:?}");
    assert!(ctx.metrics["checks"].as_array().unwrap().len() >= 6);
}

#[test]
fn ab_pair_is_loudness_matched() {
    let td = TempDir::new("m-ab");
    let s = short(style("hiphop"), 5.0);
    let (_, out, env) = master_song(&td, &s, &[("ab", "true")]);
    let outputs = env["outputs"].as_array().unwrap();
    let find = |role: &str| -> PathBuf {
        PathBuf::from(
            outputs.iter().find(|o| o["role"] == role).unwrap_or_else(|| panic!("no {role}"))["path"].as_str().unwrap(),
        )
    };
    let before = read_wav(&find("ab-before"));
    let after = read_wav(&find("ab-after"));
    let d = (loudness::integrated_lufs(&before) - loudness::integrated_lufs(&after)).abs();
    assert!(d <= 0.2, "A/B differ by {d} LU");
    assert!(env["metrics"]["ab_loudness_difference_lu"].as_f64().unwrap() <= 0.2);
    assert!(out.exists());
}

#[test]
fn plan_edit_run_round_trip() {
    let td = TempDir::new("m-plan");
    let s = short(style("edm"), 5.0);
    let inp = td.path("mix.wav");
    write(&inp, &song(&s));
    let planp = td.path("plan.json");
    let o = invoke("master plan", &[&inp], &[("target", "streaming")], Some(&planp));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let plan: Value = serde_json::from_slice(&std::fs::read(&planp).unwrap()).unwrap();
    assert_eq!(plan["version"], 1);
    assert_eq!(plan["target"]["name"], "streaming");
    let ops: Vec<&str> = plan["stages"].as_array().unwrap().iter().map(|s| s["op"].as_str().unwrap()).collect();
    for want in ["dc-remove", "silence trim", "eq", "highpass", "multiband", "widen", "saturate", "gain", "limit"] {
        assert!(ops.contains(&want), "{want} missing from {ops:?}");
    }
    // running the plan as written reproduces `master`
    let direct = td.path("direct.wav");
    let o1 = invoke("master", &[&inp], &[("target", "streaming")], Some(&direct));
    assert!(o1.envelope.ok);
    let viaplan = td.path("viaplan.wav");
    let o2 = invoke("master", &[&inp], &[("plan", planp.to_str().unwrap())], Some(&viaplan));
    assert!(o2.envelope.ok, "{:?}", o2.envelope.error);
    assert!(rms_diff(&read_wav(&direct), &read_wav(&viaplan)) < 1e-6);

    // edit: drop the color stages, lower the ceiling to -2 dBTP, gentler multiband
    let mut edited = plan.clone();
    let stages = edited["stages"].as_array_mut().unwrap();
    stages.retain(|s| s["op"] != "saturate");
    for st in stages.iter_mut() {
        if st["op"] == "limit" {
            st["args"]["ceiling"] = Value::String("-2dBTP".into());
        }
        if st["op"] == "multiband" {
            st["args"]["ratio"] = Value::String("1.5:1".into());
        }
    }
    let n_edited = stages.len();
    edited["target"]["true_peak_dbtp"] = serde_json::json!(-2.0);
    let ep = td.path("edited.json");
    std::fs::write(&ep, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
    let out = td.path("edited.wav");
    let o = invoke("master", &[&inp], &[("plan", ep.to_str().unwrap())], Some(&out));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let v = serde_json::to_value(&o.envelope).unwrap();
    assert_eq!(v["result"]["stages_run"].as_array().unwrap().len(), n_edited);
    let tp = loudness::true_peak_db(&read_wav(&out));
    assert!(tp <= -2.0 + 1e-3, "edited plan true peak {tp}");

    // invalid plans are rejected through the registry
    let mut bad = plan.clone();
    bad["stages"][0]["op"] = Value::String("no-such-effect".into());
    let bp = td.path("bad.json");
    std::fs::write(&bp, bad.to_string()).unwrap();
    let o = invoke("master", &[&inp], &[("plan", bp.to_str().unwrap())], Some(&td.path("x.wav")));
    assert!(!o.envelope.ok);
    assert_eq!(o.envelope.error.as_ref().unwrap().exit_code, 2);
    let mut bad = plan.clone();
    bad["stages"][0]["args"]["nonsense"] = Value::String("1".into());
    std::fs::write(&bp, bad.to_string()).unwrap();
    let o = invoke("master", &[&inp], &[("plan", bp.to_str().unwrap())], Some(&td.path("y.wav")));
    assert!(!o.envelope.ok);
    assert!(o.envelope.error.as_ref().unwrap().message.contains("stage 1"));
}

#[test]
fn looks_mastered_input_defaults_to_light() {
    let td = TempDir::new("m-light");
    let s = short(style("edm"), 6.0);
    // make a loud, limited "already mastered" file
    let (_, loud, _) = master_song(&td, &s, &[("target", "club"), ("intensity", "heavy")]);
    let a = invoke("master analyze", &[&loud], &[], None);
    assert!(a.envelope.ok);
    let ctxj = &a.envelope.result["context"];
    assert_eq!(ctxj["looks_mastered"], true, "{ctxj}");
    assert_eq!(ctxj["master_default_intensity"], "light");
    let out = td.path("remaster.wav");
    let o = invoke("master", &[&loud], &[], Some(&out));
    assert!(o.envelope.ok);
    let r = &o.envelope.result;
    assert_eq!(r["intensity"]["name"], "light");
    assert_eq!(r["intensity"]["source"], "looks_mastered");
    assert!(r["intensity"]["why"].as_str().unwrap().contains("looks mastered"));
    // explicit intensity wins
    let o = invoke("master plan", &[&loud], &[("intensity", "heavy")], None);
    assert_eq!(o.envelope.result["intensity"]["name"], "heavy");
}

#[test]
fn intensity_bounds_the_limiter_and_reports_a_shortfall() {
    let td = TempDir::new("m-bound");
    // sparse loud clicks over a quiet tone: a huge peak-to-loudness ratio
    let sr = 48000;
    let tone = ferraus_core::testing::sine(sr, 220.0, 5.0, 0.01);
    let clicks = ferraus_core::testing::clicks(sr, 0.5, 5.0, 0.9);
    let b = ferraus_core::testing::stereo(ferraus_core::testing::add(&tone, &clicks));
    let inp = td.path("peaky.wav");
    write(&inp, &b);
    let out = td.path("peaky.master.wav");
    let o = invoke("master", &[&inp], &[("target", "club"), ("intensity", "light")], Some(&out));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let r = &o.envelope.result;
    assert_eq!(r["loudness"]["reached"], false, "{}", r["loudness"]);
    assert!(r["loudness"]["limiter_gain_reduction_db"].as_f64().unwrap() <= 2.05, "{}", r["loudness"]);
    assert!(o.envelope.warnings.iter().any(|w| w.code == "target_not_reached"));
    assert!(loudness::true_peak_db(&read_wav(&out)) <= -0.3);
}

#[test]
fn cd_target_resamples_and_writes_16_bit() {
    let td = TempDir::new("m-cd");
    let s = short(style("rock"), 4.0);
    let (_, out, env) = master_song(&td, &s, &[("target", "cd")]);
    let (b, bits, float) = ferraus_core::wav::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(b.sample_rate, 44100);
    assert_eq!((bits, float), (16, false));
    let l = loudness::loudness(&b);
    assert!((l.integrated_lufs + 10.0).abs() <= 0.5, "{}", l.integrated_lufs);
    assert!(l.true_peak_dbtp <= -0.3);
    assert_eq!(env["result"]["output_dither"], "triangular");
    assert_eq!(env["result"]["verify"]["pass"], true, "{}", env["result"]["verify"]);
}

#[test]
fn broadcast_atsc_and_custom_targets() {
    let td = TempDir::new("m-bc");
    let s = short(style("acoustic"), 4.0);
    let (_, out, _) = master_song(&td, &s, &[("target", "broadcast"), ("standard", "atsc")]);
    let l = loudness::loudness(&read_wav(&out));
    assert!((l.integrated_lufs + 24.0).abs() <= 0.5);
    assert!(l.true_peak_dbtp <= -2.0);
    let v = verify(&out, &[("target", "broadcast"), ("standard", "atsc")]);
    assert_eq!(v["ok"], true, "{v}");
    let o =
        invoke("master plan", &[&out], &[("target", "custom"), ("lufs", "-12LUFS"), ("true-peak", "-0.5dBTP")], None);
    assert!(o.envelope.ok);
    assert_eq!(o.envelope.result["target"]["lufs"], -12.0);
    let o = invoke("master plan", &[&out], &[("target", "custom")], None);
    assert!(!o.envelope.ok);
}

#[test]
fn analyze_reports_every_section() {
    let td = TempDir::new("m-analyze");
    let mut b = song(&short(style("pop"), 4.0));
    b.data.iter_mut().flatten().for_each(|v| *v += 0.03);
    let p = td.path("dc.wav");
    write(&p, &b);
    let o = invoke("master analyze", &[&p], &[("genre", "rock")], None);
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let r = &o.envelope.result;
    for k in [
        "integrated_lufs",
        "short_term_max_lufs",
        "momentary_max_lufs",
        "loudness_range_lu",
        "true_peak_dbtp",
        "sample_peak_dbfs",
        "crest_factor_db",
        "plr_db",
    ] {
        assert!(r["loudness"].get(k).is_some(), "loudness.{k}");
    }
    assert_eq!(r["tonal_balance"]["largest_deviations"].as_array().unwrap().len(), 3);
    assert_eq!(r["tonal_balance"]["against"], "genre curve: rock");
    for k in ["gain_to_target_db", "limiter_reduction_needed_db", "transient_density_per_s", "sections"] {
        assert!(r["dynamics"].get(k).is_some(), "dynamics.{k}");
    }
    for k in ["correlation", "correlation_below_120hz", "side_to_mid_db", "mono_compatibility", "bands"] {
        assert!(r["stereo"].get(k).is_some(), "stereo.{k}");
    }
    for k in
        ["tempo_bpm", "key", "duration_s", "sample_rate", "bit_depth", "looks_mastered", "master_default_intensity"]
    {
        assert!(r["context"].get(k).is_some(), "context.{k}");
    }
    let dc = r["problems"].as_array().unwrap().iter().find(|p| p["problem"] == "dc_offset").expect("dc problem");
    assert_eq!(dc["suggested_fix"], "dc-remove");
    assert_eq!(dc["fix_available"], true);
    assert!(dc["command"].as_str().unwrap().contains("--fx \"dc-remove\""));
    // with a reference
    let o = invoke("master analyze", &[&p], &[("reference", p.to_str().unwrap())], None);
    assert!(o.envelope.result["tonal_balance"]["against"].as_str().unwrap().starts_with("reference"));
}

#[test]
fn deliver_writes_the_set_with_tags_and_loudness_sheet() {
    let td = TempDir::new("m-deliver");
    let s = short(style("pop"), 3.0);
    let (_, out, _) = master_song(&td, &s, &[]);
    let base = td.path("release/song.wav");
    let o = invoke(
        "master deliver",
        &[&out],
        &[
            ("formats", "wav24,wav16"),
            ("tags", "title=Test Song,artist=Ferraus"),
            ("isrc", "US-RC1-76-07839"),
            ("loudness-sheet", "true"),
        ],
        Some(&base),
    );
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let roles: Vec<String> = o.envelope.outputs.iter().filter_map(|o| o.role.clone()).collect();
    for r in ["wav24", "wav16", "loudness-sheet", "loudness-sheet-csv"] {
        assert!(roles.iter().any(|x| x == r), "{r} missing: {roles:?}");
    }
    let path_of =
        |role: &str| o.envelope.outputs.iter().find(|x| x.role.as_deref() == Some(role)).unwrap().path.clone();
    let (b16, bits, _) = ferraus_core::wav::decode(&std::fs::read(path_of("wav16")).unwrap()).unwrap();
    assert_eq!((b16.sample_rate, bits), (44100, 16));
    let (b24, bits, _) = ferraus_core::wav::decode(&std::fs::read(path_of("wav24")).unwrap()).unwrap();
    assert_eq!((b24.sample_rate, bits), (48000, 24));
    assert!(loudness::true_peak_db(&b16) <= -1.0 + 0.05);
    let sheet: Value = serde_json::from_slice(&std::fs::read(path_of("loudness-sheet")).unwrap()).unwrap();
    assert_eq!(sheet["deliverables"].as_array().unwrap().len(), 2);
    assert_eq!(sheet["tags"]["isrc"], "USRC17607839");
    assert_eq!(sheet["tags"]["title"], "Test Song");
    let csv = std::fs::read_to_string(path_of("loudness-sheet-csv")).unwrap();
    assert_eq!(csv.lines().count(), 3);
    // the WAV-only test codec cannot write mp3: fail before writing anything
    let o = invoke("master deliver", &[&out], &[("formats", "wav24,mp3")], Some(&td.path("other/song.wav")));
    assert!(!o.envelope.ok);
    assert_eq!(o.envelope.error.as_ref().unwrap().exit_code, 4);
    assert!(!td.path("other/song.wav24.wav").exists());
    let o = invoke("master deliver", &[&out], &[("isrc", "nope")], Some(&td.path("x/song.wav")));
    assert!(!o.envelope.ok);
}

#[test]
fn report_file_and_stems_and_dry_run() {
    let td = TempDir::new("m-misc");
    let s = short(style("pop"), 3.0);
    let inp = td.path("mix.wav");
    write(&inp, &song(&s));
    let rep = td.path("r1.json");
    let out = td.path("m.wav");
    let o = invoke("master", &[&inp], &[("report", rep.to_str().unwrap())], Some(&out));
    assert!(o.envelope.ok);
    let r: Value = serde_json::from_slice(&std::fs::read(&rep).unwrap()).unwrap();
    for k in ["before", "after", "loudness", "verify", "plan", "stages_run"] {
        assert!(r.get(k).is_some(), "report.{k}");
    }
    assert!(o.envelope.outputs.iter().any(|x| x.role.as_deref() == Some("report")));
    // --stems needs `separate`, which this registry lacks: that error comes back unchanged
    let o = invoke("master", &[&inp], &[("stems", "true")], Some(&td.path("s.wav")));
    assert!(!o.envelope.ok);
    assert!(o.envelope.error.as_ref().unwrap().message.contains("separate"));
    // dry run: plan, no files
    let rt = ferraus_core::exec::Runtime {
        registry: registry(),
        io: &ferraus_core::testing::SIMPLE_IO,
        progress: &ferraus_core::testing::NO_PROGRESS,
        presets: &|_, _| None,
        cancel: Default::default(),
    };
    let dry = td.path("dry.wav");
    let o = rt.invoke(ferraus_core::exec::Invocation {
        command: "master".into(),
        args: vec![],
        inputs: vec![inp.display().to_string()],
        globals: ferraus_core::Globals { output: Some(dry.display().to_string()), dry_run: true, ..Default::default() },
        argv: vec![],
        stdin: None,
    });
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    assert!(!dry.exists());
    assert!(o.envelope.result["plan"]["stages"].as_array().unwrap().len() > 5);
}

#[test]
fn mono_and_podcast() {
    let td = TempDir::new("m-mono");
    // a mono "voice": band-limited noise bursts
    let n = ferraus_core::testing::noise(48000, 4.0, 0.2, 9);
    let inp = td.path("voice.wav");
    write(&inp, &n);
    let out = td.path("voice.master.wav");
    let o = invoke("master", &[&inp], &[("target", "podcast")], Some(&out));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let m = read_wav(&out);
    assert_eq!(m.channels(), 1);
    assert!((loudness::integrated_lufs(&m) + 19.0).abs() <= 0.5);
    let r = &o.envelope.result;
    let ops: Vec<&str> = r["plan"]["stages"].as_array().unwrap().iter().map(|s| s["op"].as_str().unwrap()).collect();
    assert!(ops.contains(&"deess"));
    assert!(!ops.contains(&"widen"));
    if registry().get("leveler").is_some() {
        assert!(ops.contains(&"leveler"), "{ops:?}");
    } else {
        assert!(o.envelope.warnings.iter().any(|w| w.code == "leveler_unavailable"));
    }
}

#[test]
fn clipped_mix_runs_declip_first_when_available() {
    let td = TempDir::new("m-declip");
    let mut b = song(&short(style("rock"), 4.0));
    b.scale(4.0);
    b.data.iter_mut().flatten().for_each(|v| *v = v.clamp(-0.9, 0.9));
    let inp = td.path("clipped.wav");
    write(&inp, &b);
    let out = td.path("clipped.master.wav");
    let o = invoke("master", &[&inp], &[], Some(&out));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let r = &o.envelope.result;
    assert!(r["declip"].as_str().unwrap().contains("clipping"), "{}", r["declip"]);
    let first = r["plan"]["stages"][0]["op"].as_str().unwrap();
    if registry().get("declip").is_some() {
        assert_eq!(first, "declip");
    } else {
        assert_ne!(first, "declip");
        assert!(o.envelope.warnings.iter().any(|w| w.code == "declip_unavailable"));
    }
    check_streaming("clipped", &read_wav(&out));
}

#[test]
fn every_op_documents_itself() {
    let names = ["master", "master analyze", "master plan", "master verify", "master deliver", "mix verify"];
    let ops = ferraus_master::ops();
    assert_eq!(ops.len(), names.len());
    for n in names {
        let op = ops.iter().find(|o| o.name == n).unwrap();
        assert!(!op.about.is_empty(), "{n}");
        assert!(op.examples.len() >= if n == "mix verify" { 3 } else { 2 }, "{n}");
        assert!(op.examples.iter().all(|e| e.cmd.starts_with(&format!("ferraus {n}"))), "{n}");
        for p in &op.params {
            assert!(!p.help.is_empty(), "{n} --{}", p.name);
        }
        assert_eq!(op.category, ferraus_core::Category::Mastering);
    }
}

/// Apply fx stage strings in order, through the registry (what `ferraus fx --fx ...` runs).
fn apply_fixes(stages: &[String], mut buf: AudioBuffer) -> AudioBuffer {
    let mut ctx = ferraus_core::testing::ctx_with(registry());
    for s in stages {
        let (name, pairs) = ferraus_core::op::parse_stage_string(s).unwrap();
        buf = ferraus_core::exec::run_transform_pairs(registry(), &|_, _| None, &name, pairs, buf, &mut ctx)
            .unwrap_or_else(|e| panic!("{s}: {}", e.message));
    }
    buf
}

fn failed_checks(env: &Value) -> Vec<String> {
    env["metrics"]["failed_checks"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

#[test]
fn mix_verify_fails_with_a_runnable_hint_and_its_fixes_work() {
    let td = TempDir::new("mix-verify");
    // a hot, clipped bounce of the synthetic pop song
    let mut hot = song(&short(style("pop"), 4.0));
    let peak = hot.peak();
    hot.scale(1.6 / peak);
    hot.data.iter_mut().flatten().for_each(|v| *v = v.clamp(-1.0, 1.0));
    let p = td.path("hot.wav");
    write(&p, &hot);
    let report = td.path("hot.report.json");
    let o = invoke("mix verify", &[&p], &[("genre", "pop")], Some(&report));
    let env = serde_json::to_value(&o.envelope).unwrap();
    assert_eq!(env["ok"], false);
    let err = &env["error"];
    assert_eq!(err["exit_code"], 9, "{err}");
    assert_eq!(err["code"], "verification_failed");
    let failed = failed_checks(&env);
    for c in ["clipping", "true_peak"] {
        assert!(failed.iter().any(|f| f == c), "{c} not in {failed:?}");
        assert!(err["message"].as_str().unwrap().contains(c), "{}", err["message"]);
    }
    let hint = err["hint"].as_str().unwrap();
    assert!(hint.starts_with("ferraus fx ") && hint.contains("--fx \"declip\""), "{hint}");
    assert_eq!(env["metrics"]["pass"], false);
    assert_eq!(env["metrics"]["genre"], "pop");
    // the report is kept when the mix fails
    let saved: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(saved["pass"], false);
    assert!(saved["checks"].as_array().unwrap().len() >= 14);
    assert_eq!(saved["fix_command"].as_str().unwrap(), hint);

    // the fixes the gate asked for clear those checks
    let fixes: Vec<String> =
        env["metrics"]["fixes"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let fixed = apply_fixes(&fixes, read_wav(&p));
    let pf = td.path("hot.fixed.wav");
    write(&pf, &fixed);
    let o = invoke("mix verify", &[&pf], &[("genre", "pop")], None);
    let env = serde_json::to_value(&o.envelope).unwrap();
    let still: Vec<String> = if o.envelope.ok { vec![] } else { failed_checks(&env) };
    for c in ["clipping", "true_peak", "integrated_lufs"] {
        assert!(!still.iter().any(|f| f == c), "{c} still fails after {fixes:?}: {env}");
    }
}

#[test]
fn mix_verify_passes_a_clean_mix_and_rejects_bad_limits() {
    let td = TempDir::new("mix-verify-ok");
    // the synthetic pop song at a sensible mix level (-20 LUFS, or lower so the true peak
    // keeps 3.5 dB of headroom), compared with itself as the reference
    let mut b = song(&short(style("pop"), 4.0));
    let l = loudness::loudness(&b);
    let target = (-20.0f64).min(-3.5 - l.plr_db);
    b.scale(10f64.powf((target - l.integrated_lufs) / 20.0) as f32);
    let p = td.path("pop.wav");
    write(&p, &b);
    let path = p.display().to_string();
    let o = invoke("mix verify", &[&p], &[("reference", path.as_str())], None);
    let env = serde_json::to_value(&o.envelope).unwrap();
    assert!(o.envelope.ok, "{env}");
    let r = &env["result"];
    assert_eq!(r["pass"], true);
    assert_eq!(r["tonal_reference"], format!("reference: {path}"));
    let tonal = r["checks"].as_array().unwrap().iter().find(|c| c["check"] == "tonal_balance").unwrap();
    assert!(tonal["value"].as_f64().unwrap().abs() < 0.5, "{tonal}");
    assert!(r["summary"].as_str().unwrap().starts_with("ready for mastering"));
    // limits are validated
    for (k, v) in [("min-lufs", "-10LUFS"), ("headroom", "+1dBTP"), ("mono-below", "10Hz"), ("tolerance", "0dB")] {
        let o = invoke("mix verify", &[&p], &[(k, v)], None);
        let e = o.envelope.error.as_ref().unwrap_or_else(|| panic!("--{k} {v} accepted"));
        assert_eq!(e.exit_code, 2, "--{k} {v}: {}", e.message);
    }
}

/// `master --plan --relevel`: an edited plan (dynamics stages removed) still lands on the
/// target, while the same plan run as written misses it by the level those stages took out.
#[test]
fn relevel_lands_an_edited_plan_on_target() {
    let td = TempDir::new("m-relevel");
    let s = short(style("pop"), 6.0);
    let inp = td.path("pop.wav");
    write(&inp, &song(&s));
    let plan_path = td.path("plan.json");
    let o = invoke("master plan", &[&inp], &[], Some(&plan_path));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let mut plan: Value = serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
    let stages = plan["stages"].as_array_mut().unwrap();
    let before = stages.len();
    stages.retain(|st| st["stage"] != "dynamics");
    assert!(stages.len() < before, "the pop plan has dynamics stages to remove");
    let edited = td.path("edited.json");
    std::fs::write(&edited, serde_json::to_vec(&plan).unwrap()).unwrap();
    let edited_s = edited.display().to_string();

    let as_written = td.path("as_written.wav");
    let o = invoke("master", &[&inp], &[("plan", &edited_s)], Some(&as_written));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let written_lufs = loudness::integrated_lufs(&read_wav(&as_written));

    let relevelled = td.path("relevelled.wav");
    let o = invoke("master", &[&inp], &[("plan", &edited_s), ("relevel", "true")], Some(&relevelled));
    assert!(o.envelope.ok, "{:?}", o.envelope.error);
    let m = read_wav(&relevelled);
    let l = loudness::loudness(&m);
    assert!((l.integrated_lufs + 14.0).abs() <= 0.5, "relevelled to {} LUFS", l.integrated_lufs);
    assert!(l.true_peak_dbtp <= -1.0, "{} dBTP", l.true_peak_dbtp);
    assert!(
        (l.integrated_lufs + 14.0).abs() < (written_lufs + 14.0).abs() || (written_lufs + 14.0).abs() <= 0.1,
        "relevel did not get closer: {} vs {written_lufs}",
        l.integrated_lufs
    );
    let env = serde_json::to_value(&o.envelope).unwrap();
    assert_eq!(env["metrics"]["target_reached"], true);
    let notes = env["result"]["notes"].as_array().unwrap();
    assert!(notes.iter().any(|n| n.as_str().unwrap_or("").starts_with("relevelled")), "{notes:?}");
    // the rest of the plan ran as written
    let ops: Vec<&str> =
        env["result"]["plan"]["stages"].as_array().unwrap().iter().map(|s| s["op"].as_str().unwrap()).collect();
    assert!(!ops.contains(&"multiband") && ops.ends_with(&["gain", "limit"]), "{ops:?}");

    // a plan without loudness stages cannot be relevelled
    plan["stages"].as_array_mut().unwrap().retain(|st| st["stage"] != "loudness");
    std::fs::write(&edited, serde_json::to_vec(&plan).unwrap()).unwrap();
    let o = invoke("master", &[&inp], &[("plan", &edited_s), ("relevel", "true")], Some(&td.path("x.wav")));
    assert!(!o.envelope.ok);
}
