//! The `score` ops end to end (files in a temp dir; the audio path uses stub ops).
#![allow(clippy::field_reassign_with_default, clippy::manual_is_multiple_of, clippy::needless_range_loop)]

mod common;
use common::*;
use ferraus_core::prelude::*;
use ferraus_core::{Globals, OpSpec, Runner};
use ferraus_score::midi::{write_midi, MidiData, MidiNote, MidiTrack};
use ferraus_score::model::*;
use std::path::PathBuf;

fn write_test_midi(path: &std::path::Path) {
    // a C major tune with chords underneath, 2 bars of 4/4 with a G7 → C cadence
    let mut notes = Vec::new();
    for (i, m) in [72u8, 76, 79, 76, 74, 71, 72, 72].iter().enumerate() {
        notes.push(MidiNote { track: 1, channel: 0, key: *m, velocity: 90, start: i as u64 * 480, end: i as u64 * 480 + 480 });
    }
    let d = MidiData {
        ppq: 480,
        notes,
        tracks: vec![MidiTrack::default(), MidiTrack { name: Some("Melody".into()), program: Some((0, 0)) }],
        tempos: vec![(0, 600_000)],
        time_sigs: vec![(0, 4, 4)],
        ..MidiData::default()
    };
    std::fs::write(path, write_midi(&d).unwrap()).unwrap();
}

#[test]
fn score_from_midi_writes_every_text_output() {
    let t = tmp("score-op");
    let mid = t.path("tune.mid");
    write_test_midi(&mid);
    let out = t.path("tune.musicxml");
    let pairs = [
        ("midi", t.path("out.mid").display().to_string()),
        ("abc", t.path("out.abc").display().to_string()),
        ("ly", t.path("out.ly").display().to_string()),
        ("mei", t.path("out.mei").display().to_string()),
        ("solfa-txt", t.path("out.md").display().to_string()),
        ("json-out", t.path("out.json").display().to_string()),
        ("solfa", "on".to_string()),
        ("title", "Tune".to_string()),
    ];
    let p: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (v, outs) = run_op("score", vec![mid.clone()], &p, Some(out.clone())).unwrap();
    assert_eq!(outs.len(), 7, "{outs:?}");
    let xml = std::fs::read_to_string(&out).unwrap();
    assert!(xml.contains("<score-partwise version=\"4.0\">"));
    assert!(xml.contains("<work-title>Tune</work-title>"));
    assert!(xml.contains("name=\"solfa\""));
    assert!(std::fs::read_to_string(t.path("out.abc")).unwrap().contains("K:C"));
    assert!(std::fs::read_to_string(t.path("out.ly")).unwrap().contains("\\version"));
    assert!(std::fs::read_to_string(t.path("out.mei")).unwrap().contains("<mei "));
    assert!(std::fs::read_to_string(t.path("out.md")).unwrap().contains("Doh is C"));
    let model: Score = serde_json::from_slice(&std::fs::read(t.path("out.json")).unwrap()).unwrap();
    assert_eq!(model.parts.len(), 1);
    assert_eq!(v["notation"]["key"], "C major");
    assert_eq!(v["check"]["ok"], true);
    assert!(std::fs::read(t.path("out.mid")).unwrap().starts_with(b"MThd"));
}

#[test]
fn solfa_only_writes_text_and_no_staff() {
    let t = tmp("solfa-only");
    let mid = t.path("tune.mid");
    write_test_midi(&mid);
    let out = t.path("tune.txt");
    let (_, outs) = run_op("score", vec![mid.clone()], &[("solfa", "only")], Some(out.clone())).unwrap();
    assert_eq!(outs, vec![out.display().to_string()]);
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("|d :m :s :m |r :t, :d :d ||"), "{text}");
    for staff_markup in ["<", "staff", "measure", "\\new", "X:"] {
        assert!(!text.contains(staff_markup), "{staff_markup} in {text}");
    }
    // asking for a staff format with --solfa only is a usage error
    let e = run_op("score", vec![mid.clone()], &[("solfa", "only")], Some(t.path("x.musicxml"))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
    // fixed do, markdown
    let md = t.path("tune.md");
    run_op("score", vec![mid], &[("solfa", "fixed-only")], Some(md.clone())).unwrap();
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("fixed do") && text.contains("do") && !text.contains("<"));
}

#[test]
fn convert_to_json_from_json_and_check() {
    let t = tmp("convert");
    let mid = t.path("tune.mid");
    write_test_midi(&mid);
    // two positional paths
    let xml = t.path("tune.musicxml");
    run_op("score convert", vec![mid.clone(), xml.clone()], &[], None).unwrap();
    // -o form
    let abc = t.path("tune.abc");
    let (v, _) = run_op("score convert", vec![xml.clone()], &[], Some(abc.clone())).unwrap();
    assert_eq!(v["to"], "abc");
    let ly = t.path("tune.ly");
    run_op("score convert", vec![abc.clone(), ly.clone()], &[], None).unwrap();
    let mei = t.path("tune.mei");
    run_op("score convert", vec![ly.clone(), mei.clone()], &[], None).unwrap();
    let mxl = t.path("tune.mxl");
    run_op("score convert", vec![mei.clone(), mxl.clone()], &[], None).unwrap();
    let txt = t.path("tune.txt");
    run_op("score convert", vec![mxl.clone(), txt.clone()], &[], None).unwrap();
    assert!(std::fs::read_to_string(&txt).unwrap().contains("Doh is C"));
    // every hop preserved the notes
    let a = ferraus_score::formats::load(&xml, &std::fs::read(&xml).unwrap()).unwrap().score;
    let b = ferraus_score::formats::load(&mxl, &std::fs::read(&mxl).unwrap()).unwrap().score;
    assert_eq!(flat_notes(&a, 0, 0), flat_notes(&b, 0, 0));
    // to-json / from-json
    let (j, _) = run_op("score to-json", vec![xml.clone()], &[], None).unwrap();
    assert_eq!(j["parts"][0]["id"], "P1");
    let json = t.path("tune.json");
    run_op("score to-json", vec![xml.clone()], &[("pretty", "true")], Some(json.clone())).unwrap();
    let back = t.path("back.musicxml");
    run_op("score from-json", vec![json.clone()], &[], Some(back.clone())).unwrap();
    assert_eq!(std::fs::read_to_string(&back).unwrap(), std::fs::read_to_string(&xml).unwrap());
    // check passes on our output
    let (r, _) = run_op("score check", vec![back.clone()], &[], None).unwrap();
    assert_eq!(r["ok"], true);
    // a broken measure fails with exit code 9
    let mut s: Score = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    s.parts[0].staves[0].measures[0].voices[0].events.pop();
    let broken = t.path("broken.json");
    std::fs::write(&broken, serde_json::to_vec(&s).unwrap()).unwrap();
    let e = run_op("score check", vec![broken.clone()], &[], Some(t.path("report.json"))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Verification);
    assert_eq!(e.exit_code, 9);
    assert!(e.message.contains("measure_too_short"));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(t.path("report.json")).unwrap()).unwrap();
    assert_eq!(report["failing_measures"], serde_json::json!([1]));
}

#[test]
fn read_fix_and_review_on_musicxml() {
    let t = tmp("read-fix");
    let s = score(KeySig::major(0), TimeSig::new(4, 4), false, &[&["C4:q", "D4:q", "E4:q", "F4:q"], &["G4:q", "A4:q", "B4:q"]]);
    let src = t.path("scan.musicxml");
    std::fs::write(&src, ferraus_score::musicxml::write(&s)).unwrap();
    let out = t.path("checked.musicxml");
    let (v, _) = run_op("score read", vec![src.clone()], &[("review", "true")], Some(out.clone())).unwrap();
    assert_eq!(v["check_ok"], false);
    assert_eq!(v["review"]["failing_measures"], serde_json::json!([2]));
    let edits = t.path("edits.json");
    std::fs::write(&edits, r#"[{"measure": 2, "voice": 1, "note": 3, "set": {"duration": "half"}}, {"measure": 1, "note": 1, "set": {"pitch": "E5"}}]"#).unwrap();
    let fixed = t.path("fixed.musicxml");
    let (v, _) = run_op("score read", vec![src], &[("fix", edits.to_str().unwrap()), ("review", "true")], Some(fixed.clone())).unwrap();
    assert_eq!(v["check_ok"], true, "{v}");
    assert_eq!(v["fixes"][0]["ok"], true);
    let xml = std::fs::read_to_string(&fixed).unwrap();
    assert!(xml.contains("<step>E</step>\n          <octave>5</octave>"));
}

#[test]
fn read_pdf_without_models_is_model_not_cached() {
    if ferraus_score::omr::builtin_model_dir().exists() {
        return;
    }
    let t = tmp("read-pdf");
    let pdf = t.path("score.pdf");
    std::fs::write(&pdf, b"%PDF-1.4\n%fake\n").unwrap();
    let e = run_op("score read", vec![pdf], &[], Some(t.path("o.musicxml"))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ModelNotCached);
    assert_eq!(e.exit_code, 5);
    assert_eq!(e.hint.as_deref(), Some("ferraus models pull omr/oemer"));
}

#[test]
fn play_and_perform() {
    let t = tmp("play");
    let s = score(KeySig::major(0), TimeSig::new(4, 4), false, &[&["C4:q", "D4:q", "E4:q", "F4:q"]]);
    let src = t.path("song.musicxml");
    std::fs::write(&src, ferraus_score::musicxml::write(&s)).unwrap();
    let out = t.path("song.mid");
    let (v, _) = run_op("score play", vec![src.clone()], &[("tempo", "60"), ("instrument-map", "P1=strings")], Some(out.clone())).unwrap();
    assert!((v["duration_s"].as_f64().unwrap() - 4.0).abs() < 1e-6);
    let m = ferraus_score::midi::read_midi(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(m.tracks.iter().find_map(|t| t.program).map(|p| p.1), Some(48));
    let e = run_op("score play", vec![src.clone()], &[("instrument-map", "P9=piano")], Some(out.clone())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
    // perform needs the render op; without it the registry's error comes back unchanged
    let e = run_op("score perform", vec![src], &[], Some(t.path("song.wav"))).unwrap_err();
    assert!(e.message.contains("render"), "{}", e.message);
    assert!(t.path("song.mid").exists());
}

#[test]
fn engraving_uses_the_helper_or_says_how_to_get_one() {
    let t = tmp("engrave");
    let mid = t.path("tune.mid");
    write_test_midi(&mid);
    // SVG always works: the helper, LilyPond, or the native fallback
    let svg = t.path("tune.svg");
    run_op("score", vec![mid.clone()], &[("svg", svg.to_str().unwrap())], Some(t.path("tune.musicxml"))).unwrap();
    assert!(std::fs::read_to_string(&svg).unwrap().contains("<svg"));
    // the native renderer on request
    let native = t.path("native.svg");
    run_op("score", vec![mid.clone()], &[("svg", native.to_str().unwrap()), ("engraver", "native")], Some(t.path("n.musicxml"))).unwrap();
    let text = std::fs::read_to_string(&native).unwrap();
    assert!(text.contains("class=\"notehead\"") && text.contains("class=\"clef\""));
    assert_eq!(text.matches("class=\"notehead\"").count(), 8);
    // PDF needs a real engraver
    let pdf = t.path("tune.pdf");
    let res = run_op("score", vec![mid], &[("pdf", pdf.to_str().unwrap())], Some(t.path("tune2.musicxml")));
    match ferraus_score::engrave::choose("auto") {
        Ok(_) => {
            res.unwrap();
            assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
        }
        Err(_) => {
            let e = res.unwrap_err();
            assert_eq!(e.kind, ErrorKind::Unsupported);
            assert_eq!(e.code, "engraver_missing");
            assert!(e.hint.unwrap().contains("ferraus-engrave"));
        }
    }
}

// ---- audio path with stub ops ----------------------------------------------------

fn stub_tempo(_a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    let beats: Vec<f64> = (0..16).map(|i| 0.25 + i as f64 * 0.5).collect();
    Ok(json!({"bpm": 120.0, "beats": beats, "downbeats": [0.25, 2.25], "time_signature": "4/4"}))
}
fn stub_key(_a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    Ok(json!({"key": "G", "mode": "major", "confidence": 0.9}))
}
fn stub_chords(_a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    Ok(json!({"chords": [{"start": 0.25, "end": 2.25, "label": "G:maj"}, {"start": 2.25, "end": 4.25, "label": "D:7"}]}))
}
fn stub_separate(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let sr = 22050;
    for role in ["vocals", "drums", "bass", "other", "piano", "guitar"] {
        let p = ctx.output_path(Some(role), "wav")?;
        let buf = if role == "drums" {
            // a kick every half second
            let mut x = vec![0f32; sr as usize * 4];
            for k in 0..8 {
                let s0 = (0.25 + k as f64 * 0.5) * sr as f64;
                for i in 0..2000 {
                    let tt = i as f64 / sr as f64;
                    x[s0 as usize + i] += ((2.0 * std::f64::consts::PI * 60.0 * tt).sin() * (-tt * 15.0).exp() * 0.9) as f32;
                }
            }
            AudioBuffer::from_mono(sr, x)
        } else {
            ferraus_core::testing::sine(sr, 220.0, 4.0, 0.1)
        };
        ctx.write_audio(&p, &buf, Some(role))?;
    }
    Ok(Value::Null)
}
fn stub_transcribe(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mode = a.str_or("mode", "auto");
    let out = ctx.output_path(None, "mid")?;
    // a G major line at 120 bpm starting on the first beat (0.25 s)
    let base: u8 = if mode == "mono" { 67 } else { 55 };
    let notes = (0..8)
        .map(|i| MidiNote { track: 0, channel: 0, key: base + [0, 2, 4, 5, 7, 5, 4, 2][i], velocity: 80, start: 120 + i as u64 * 480, end: 120 + i as u64 * 480 + 470 })
        .collect();
    let d = MidiData { ppq: 480, notes, tracks: vec![MidiTrack::default()], tempos: vec![(0, 500_000)], ..MidiData::default() };
    ctx.write_file(&out, &write_midi(&d).unwrap(), Some("midi"))?;
    Ok(json!({"notes": [{"pitch": base, "start": 0.25, "confidence": 0.4}]}))
}
fn stub_separate_missing(_a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    Err(Error::new(ErrorKind::ModelNotCached, "model_not_cached", "separation model missing").with_hint("ferraus models pull separate/roformer"))
}

fn audio_registry(missing: bool) -> &'static ferraus_core::Registry {
    let mut ops: Vec<OpSpec> = ferraus_score::ops();
    ops.push(OpSpec::custom("analyze tempo", Category::Inspect, "stub", stub_tempo).inputs(ferraus_core::Inputs::ONE_ANY));
    ops.push(OpSpec::custom("analyze key", Category::Inspect, "stub", stub_key).inputs(ferraus_core::Inputs::ONE_ANY));
    ops.push(OpSpec::custom("analyze chords", Category::Inspect, "stub", stub_chords).inputs(ferraus_core::Inputs::ONE_ANY));
    ops.push(
        OpSpec::custom("separate", Category::Separate, "stub", if missing { stub_separate_missing } else { stub_separate })
            .param(param("stems", ParamKind::Str).default("4").help("stems")),
    );
    ops.push(OpSpec::custom("transcribe", Category::Midi, "stub", stub_transcribe).param(param("mode", ParamKind::Str).default("auto").help("mode")));
    ferraus_core::testing::leak_registry(ops)
}

fn run_audio(reg: &'static ferraus_core::Registry, input: PathBuf, pairs: &[(&str, &str)], out: PathBuf) -> Result<Value> {
    let spec = reg.get("score").unwrap();
    let args = ferraus_core::Args::build(spec, pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), &|_, _| None)?;
    let mut g = Globals::default();
    g.output = Some(out.display().to_string());
    g.overwrite = true;
    let mut ctx = OpCtx::new("score", vec![input], g, &ferraus_core::testing::SIMPLE_IO, &ferraus_core::testing::NO_PROGRESS);
    ctx.registry = Some(reg);
    match spec.runner {
        Runner::Custom(f) => f(&args, &mut ctx),
        _ => unreachable!(),
    }
}

#[test]
fn audio_to_notation_composes_the_other_ops() {
    let t = tmp("audio");
    let wav = t.path("take.wav");
    ferraus_core::testing::write_wav(&wav, &ferraus_core::testing::sine(22050, 220.0, 4.0, 0.1));
    let out = t.path("take.musicxml");
    let v = run_audio(audio_registry(false), wav.clone(), &[("parts", "vocals,piano,bass,drums")], out.clone()).unwrap();
    assert_eq!(v["notation"]["key"], "G major");
    let xml = std::fs::read_to_string(&out).unwrap();
    let (s, _) = ferraus_score::musicxml::read(&xml).unwrap();
    let names: Vec<&str> = s.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Vocals", "Piano", "Bass", "Drums"]);
    assert_eq!(s.parts[1].staves.len(), 2, "piano is a grand staff");
    assert!(s.parts[3].percussion);
    assert_eq!(s.parts[2].staves[0].measures[0].clef, Some(Clef::bass()));
    // the first note sits on the downbeat of measure 1, melody G A B C D C B A
    let first = &s.parts[0].staves[0].measures[0].voices[0].events[0];
    assert_eq!(first.notes[0].pitch.name(), "G4");
    // chord symbols from analyze chords
    let h: Vec<String> = s.parts[0].staves[0].measures.iter().flat_map(|m| m.harmonies.iter().map(|h| h.text.clone())).collect();
    assert_eq!(h, vec!["G", "D7"]);
    // kicks were transcribed natively
    let kicks = s.parts[3].staves[0].measures.iter().flat_map(|m| m.voices.iter().flat_map(|v| v.events.iter().flat_map(|e| e.notes.iter()))).filter(|n| n.midi == 36).count();
    assert_eq!(kicks, 8);
    // transcriber confidence reached the measures
    assert!(xml.contains("<score-partwise"));
}

#[test]
fn audio_missing_model_error_is_returned_unchanged() {
    let t = tmp("audio-missing");
    let wav = t.path("take.wav");
    ferraus_core::testing::write_wav(&wav, &ferraus_core::testing::sine(22050, 220.0, 1.0, 0.1));
    let e = run_audio(audio_registry(true), wav, &[], t.path("o.musicxml")).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ModelNotCached);
    assert_eq!(e.exit_code, 5);
    assert_eq!(e.hint.as_deref(), Some("ferraus models pull separate/roformer"));
}

#[test]
fn every_op_has_docs_and_examples() {
    for op in ferraus_score::ops() {
        assert!(!op.about.is_empty(), "{}", op.name);
        assert!(op.examples.len() >= 2, "{}", op.name);
        assert_eq!(op.category, Category::Score);
        for e in &op.examples {
            assert!(e.cmd.starts_with(&format!("ferraus {}", op.name)), "{}", e.cmd);
        }
        for p in &op.params {
            assert!(!p.help.is_empty(), "{} --{}", op.name, p.name);
            assert!(!["output", "json", "format", "bpm", "seed", "force"].contains(&p.name), "{} --{} collides with a global", op.name, p.name);
        }
    }
}
