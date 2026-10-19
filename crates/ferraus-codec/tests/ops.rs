mod common;

use common::*;
use ferraus_codec::{read_path, write_file};
use ferraus_core::ctx::EncodeOptions;
use ferraus_core::testing::{sine, TempDir};
use std::collections::BTreeMap;

fn wav16(td: &TempDir, name: &str, secs: f64) -> std::path::PathBuf {
    let p = td.path(name);
    let o = EncodeOptions { bit_depth: Some(16), ..Default::default() };
    write_file(&p, &int_signal(44100, 2, secs, 16), &o).unwrap();
    p
}

#[test]
fn every_op_has_docs_and_examples() {
    for op in ferraus_codec::ops() {
        assert!(!op.about.is_empty(), "{}", op.name);
        assert!(op.examples.len() >= 2, "{}", op.name);
        for p in &op.params {
            assert!(!p.help.is_empty(), "{} --{}", op.name, p.name);
        }
    }
}

#[test]
fn convert_wav_to_wav_is_bit_exact_streaming() {
    let td = TempDir::new("conv-wav");
    let src = wav16(&td, "in.wav", 0.6);
    let out = td.path("out.wav");
    let env = invoke("convert", &[], &[&src], Globals { output: Some(out.display().to_string()), ..globals() });
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["streamed"], true);
    assert_eq!(env.result["bit_depth"], 16);
    assert_eq!(wav_data(&std::fs::read(&src).unwrap()), wav_data(&std::fs::read(&out).unwrap()));
    assert_eq!(env.outputs.len(), 1);
}

use ferraus_core::ctx::Globals;

#[test]
fn convert_flac_round_trip_bit_exact() {
    let td = TempDir::new("conv-flac");
    let src = wav16(&td, "in.wav", 0.6);
    let flac = td.path("mid.flac");
    let back = td.path("back.wav");
    let e1 = invoke("convert", &[], &[&src], Globals { output: Some(flac.display().to_string()), ..globals() });
    assert!(e1.ok, "{:?}", e1.error);
    let e2 = invoke("convert", &[], &[&flac], Globals { output: Some(back.display().to_string()), ..globals() });
    assert!(e2.ok, "{:?}", e2.error);
    assert_eq!(wav_data(&std::fs::read(&src).unwrap()), wav_data(&std::fs::read(&back).unwrap()));
}

#[test]
fn convert_resample_and_dither_uses_full_path() {
    let td = TempDir::new("conv-sr");
    let p = td.path("hi.wav");
    write_file(&p, &sine(48000, 1000.0, 0.5, 0.5), &EncodeOptions { bit_depth: Some(24), ..Default::default() })
        .unwrap();
    let out = td.path("cd.wav");
    let g = Globals {
        output: Some(out.display().to_string()),
        sample_rate: Some(44100),
        bit_depth: Some(16),
        seed: 3,
        ..globals()
    };
    let env = invoke("convert", &[("dither", "triangular")], &[&p], g);
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["streamed"], false);
    let (b, info) = read_path(&out, None).unwrap();
    assert_eq!((info.sample_rate, info.bit_depth), (44100, Some(16)));
    assert!((b.duration_s() - 0.5).abs() < 0.01);
}

#[test]
fn convert_default_name_and_dry_run() {
    let td = TempDir::new("conv-dry");
    let src = wav16(&td, "song.wav", 0.1);
    let env = invoke("convert", &[], &[&src], Globals { format: Some("flac".into()), dry_run: true, ..globals() });
    assert!(env.ok, "{:?}", env.error);
    assert!(env.outputs[0].path.ends_with("song.convert.flac"));
    assert!(!td.path("song.convert.flac").exists());
    // a real run with the default name
    let env = invoke("convert", &[], &[&src], Globals { format: Some("flac".into()), ..globals() });
    assert!(env.ok, "{:?}", env.error);
    assert!(td.path("song.convert.flac").exists());
    // refuses to overwrite
    let env = invoke("convert", &[], &[&src], Globals { format: Some("flac".into()), ..globals() });
    assert!(!env.ok);
    assert_eq!(env.error.unwrap().exit_code, 2);
}

#[test]
fn convert_copies_tags_and_lossy_defaults_to_16_bit() {
    let td = TempDir::new("conv-tags");
    let src = wav16(&td, "a.wav", 0.3);
    let flac = td.path("a.flac");
    let env = invoke("convert", &[], &[&src], Globals { output: Some(flac.display().to_string()), ..globals() });
    assert!(env.ok);
    let mut edit = ferraus_codec::tags::TagEdit::default();
    edit.set.push(("title".into(), "Blue Hour".into()));
    edit.set.push(("artist".into(), "Nadia".into()));
    ferraus_codec::tags::write(&flac, &edit).unwrap();
    let ogg = td.path("a.ogg");
    let env = invoke("convert", &[], &[&flac], Globals { output: Some(ogg.display().to_string()), ..globals() });
    assert!(env.ok, "{:?}", env.error);
    let t = ferraus_codec::tags::read(&ogg).unwrap();
    assert_eq!(t.tags.get("title").map(|s| s.as_str()), Some("Blue Hour"));
    // lossy source → 16-bit by default
    let back = td.path("from-ogg.wav");
    let env = invoke("convert", &[], &[&ogg], Globals { output: Some(back.display().to_string()), ..globals() });
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["bit_depth"], 16);
}

#[test]
fn info_reports_fields_loudness_and_spectrum() {
    let td = TempDir::new("info");
    let p = td.path("tone.flac");
    let mut tags = BTreeMap::new();
    tags.insert("title".to_string(), "Tone".to_string());
    write_file(&p, &sine(48000, 1000.0, 2.0, 0.1), &EncodeOptions { bit_depth: Some(24), tags, ..Default::default() })
        .unwrap();
    let env = invoke("info", &[("loudness", "true"), ("spectrum", "true")], &[&p], globals());
    assert!(env.ok, "{:?}", env.error);
    let r = &env.result;
    assert_eq!(r["container"], "flac");
    assert_eq!(r["codec"], "flac");
    assert_eq!(r["sample_rate"], 48000);
    assert_eq!(r["channels"], 1);
    assert_eq!(r["bit_depth"], 24);
    assert_eq!(r["lossless"], true);
    assert_eq!(r["tags"]["title"], "Tone");
    assert!((r["duration_s"].as_f64().unwrap() - 2.0).abs() < 0.01);
    let lufs = r["loudness"]["integrated_lufs"].as_f64().unwrap();
    assert!((lufs + 23.0).abs() < 0.6, "{lufs}");
    let bands = r["spectrum"]["bands"].as_array().unwrap();
    assert!(!bands.is_empty());
}

#[test]
fn info_on_garbage_is_exit_4_and_missing_is_3() {
    let td = TempDir::new("info-bad");
    let p = td.path("bad.mp3");
    std::fs::write(&p, vec![0u8; 3000]).unwrap();
    let env = invoke("info", &[], &[&p], globals());
    assert!(!env.ok);
    assert_eq!(env.error.unwrap().exit_code, 4);
    let env = invoke("info", &[], &[&td.path("none.wav")], globals());
    assert_eq!(env.error.unwrap().exit_code, 3);
}

#[test]
fn tags_read_write_in_place_and_copy() {
    let td = TempDir::new("tags");
    let src = wav16(&td, "s.wav", 0.1);
    let flac = td.path("s.flac");
    invoke("convert", &[], &[&src], Globals { output: Some(flac.display().to_string()), ..globals() });
    // writing without --in-place/-o is a usage error
    let env = invoke("tags", &[("set", "title=X")], &[&flac], globals());
    assert_eq!(env.error.unwrap().exit_code, 2);
    // in place, keeping a .bak
    let env = invoke(
        "tags",
        &[("set", "title=Song"), ("set", "artist=Me")],
        &[&flac],
        Globals { in_place: true, ..globals() },
    );
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["tags"]["title"], "Song");
    assert!(td.path("s.flac.bak").exists());
    // read back
    let env = invoke("tags", &[], &[&flac], globals());
    assert_eq!(env.result["tags"]["artist"], "Me");
    // copy with art and a removal; the audio is unchanged
    let png = td.path("c.png");
    std::fs::write(&png, tiny_png()).unwrap();
    let copy = td.path("copy.flac");
    let env = invoke(
        "tags",
        &[("art", png.to_str().unwrap()), ("remove", "artist")],
        &[&flac],
        Globals { output: Some(copy.display().to_string()), ..globals() },
    );
    assert!(env.ok, "{:?}", env.error);
    assert_eq!(env.result["art"]["present"], true);
    assert!(env.result["tags"].get("artist").is_none());
    assert_eq!(read_path(&copy, None).unwrap().0, read_path(&flac, None).unwrap().0);
    // clear
    let env = invoke("tags", &[("clear", "true")], &[&copy], Globals { in_place: true, overwrite: true, ..globals() });
    assert!(env.ok, "{:?}", env.error);
    assert!(env.result["tags"].as_object().unwrap().is_empty());
    // unknown key
    let env = invoke("tags", &[("set", "nonsense_key_xyz=1")], &[&copy], Globals { in_place: true, ..globals() });
    assert_eq!(env.error.unwrap().exit_code, 2);
}

fn tiny_png() -> Vec<u8> {
    // 1x1 transparent PNG
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D,
        0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}
