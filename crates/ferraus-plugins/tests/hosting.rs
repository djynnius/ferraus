//! Out-of-process CLAP hosting against the test plugin bundle (`ferraus-test-plugin`).

mod common;

use ferraus_core::testing::TempDir;
use ferraus_core::{AudioBuffer, ErrorKind};
use ferraus_plugins::api::{PluginSession, Tail};
use ferraus_plugins::{
    process_effect, process_effect_with, render_instrument, render_instrument_with, MidiEvent, PluginOptions, PluginRef,
};
use std::time::{Duration, Instant};

fn plugin(dir: &TempDir, id: &str) -> PluginRef {
    let b = common::clap_bundle(&dir.0, "FerrausTest");
    PluginRef::from_path(b, Some(id))
}

fn opts() -> PluginOptions {
    PluginOptions { watchdog: Duration::from_secs(20), tail: Tail::Seconds(0.0), ..Default::default() }
}

fn tone(frames: usize) -> AudioBuffer {
    let s: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.05).sin() * 0.8).collect();
    AudioBuffer::from_planar(48000, vec![s.clone(), s])
}

#[test]
fn gain_minus_6db_halves_amplitude() {
    let d = TempDir::new("gain");
    let p = plugin(&d, "ferraus.test.gain");
    let input = tone(5000);
    let out = process_effect(&p, &input, &[("gain".into(), "-6dB".into())], None, 0.0).unwrap();
    assert_eq!(out.frames(), input.frames());
    assert_eq!(out.channels(), 2);
    let ratio = out.peak() / input.peak();
    assert!((ratio - 0.5012).abs() < 0.002, "ratio {ratio}");
    // display-name lookup, case-insensitive, and norm: values
    let o = PluginOptions { params: vec![("Gain".into(), "norm:1".into())], ..opts() };
    let r = process_effect_with(&p, &input, &o).unwrap();
    assert_eq!(r.params[0].text, "24.00 dB");
    // a mono input is widened to the plugin's stereo input
    let mono = AudioBuffer::from_mono(48000, tone(100).data[0].clone());
    assert_eq!(process_effect(&p, &mono, &[], None, 0.0).unwrap().channels(), 2);
}

#[test]
fn bad_param_names_and_values_are_usage_errors() {
    let d = TempDir::new("badparam");
    let p = plugin(&d, "ferraus.test.gain");
    let e = process_effect(&p, &tone(100), &[("cutoff".into(), "1kHz".into())], None, 0.0).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage, "{e}");
    assert!(e.message.contains("gain"), "{e}");
    let e = process_effect(&p, &tone(100), &[("gain".into(), "loud".into())], None, 0.0).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage, "{e}");
}

#[test]
fn latency_is_compensated() {
    let d = TempDir::new("latency");
    let p = plugin(&d, "ferraus.test.delay");
    let mut input = AudioBuffer::new(48000, 2, 4000);
    input.data[0][1234] = 1.0;
    input.data[1][1234] = 1.0;
    let run = process_effect_with(&p, &input, &opts()).unwrap();
    assert_eq!(run.latency, 64);
    let out = run.audio;
    assert_eq!(out.frames(), input.frames());
    let pos = out.data[0].iter().position(|v| v.abs() > 0.5).unwrap();
    assert_eq!(pos, 1234);
    assert_eq!(out.data[0], input.data[0]);
}

#[test]
fn sine_instrument_onset_and_state_reproduce() {
    let d = TempDir::new("sine");
    let p = plugin(&d, "ferraus.test.sine");
    let sr = 48000u32;
    let onset = 12_345u64; // not on a block boundary
    let events = [MidiEvent::note_on(onset, 0, 69, 0.9), MidiEvent::note_off(onset + 9600, 0, 69)];
    let o = PluginOptions { params: vec![("level".into(), "-12dB".into())], ..opts() };
    let run = render_instrument_with(&p, &events, sr, 1.0, &o).unwrap();
    let a = &run.audio;
    assert_eq!(a.frames(), sr as usize);
    assert!(a.peak() > 0.1, "silent render");
    let first = a.data[0].iter().position(|v| v.abs() > 1e-4).unwrap() as i64;
    let err_ms = (first - onset as i64).abs() as f64 * 1000.0 / sr as f64;
    assert!(err_ms <= 1.0, "onset off by {err_ms} ms");
    // the note-off stops the voice
    assert!(a.data[0][(onset + 9700) as usize..].iter().all(|v| *v == 0.0));
    // the captured state reproduces the render bit-exactly on a fresh host
    let state = run.state.clone().expect("state captured");
    let again = render_instrument(&p, &events, sr, 1.0, &[], Some(&state)).unwrap();
    assert_eq!(again.data, run.audio.data, "state did not reproduce the render");
    // and differs from the default level
    let default = render_instrument(&p, &events, sr, 1.0, &[], None).unwrap();
    assert_ne!(default.data, run.audio.data);
}

#[test]
fn crash_is_a_plugin_error_and_the_next_job_still_runs() {
    let d = TempDir::new("crash");
    let crash = plugin(&d, "ferraus.test.crash");
    let o = PluginOptions {
        params: vec![("crash_on_process".into(), "on".into())],
        watchdog: Duration::from_secs(10),
        ..opts()
    };
    let t = Instant::now();
    let e = process_effect_with(&crash, &tone(2000), &o).unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(10));
    assert_eq!(e.kind, ErrorKind::Plugin, "{e}");
    assert_eq!(e.exit_code, 7);
    assert_eq!(e.code, "plugin_crashed");
    assert!(e.message.contains("Ferraus Test Crash"), "{}", e.message);
    assert!(e.message.contains("FerrausTest.clap"), "{}", e.message);
    // the same process keeps working
    let gain = plugin(&d, "ferraus.test.gain");
    let out = process_effect(&gain, &tone(2000), &[("gain".into(), "0dB".into())], None, 0.0).unwrap();
    assert_eq!(out.data, tone(2000).data);
}

#[test]
fn hang_trips_the_watchdog() {
    let d = TempDir::new("hang");
    let hang = plugin(&d, "ferraus.test.hang");
    let o = PluginOptions { watchdog: Duration::from_millis(1500), ..opts() };
    let t = Instant::now();
    let e = process_effect_with(&hang, &tone(2000), &o).unwrap_err();
    let took = t.elapsed();
    assert_eq!(e.kind, ErrorKind::Plugin, "{e}");
    assert_eq!(e.code, "plugin_timeout");
    assert!(e.message.contains("Ferraus Test Hang"), "{}", e.message);
    assert!(took >= Duration::from_millis(1400) && took < Duration::from_secs(8), "{took:?}");
}

#[test]
fn presets_state_and_info() {
    let d = TempDir::new("presets");
    let p = plugin(&d, "ferraus.test.gain");
    let mut s = PluginSession::open(&p, Duration::from_secs(20)).unwrap();
    assert_eq!(s.info.format, "clap");
    assert_eq!(s.info.params.len(), 1);
    let g = &s.info.params[0];
    assert_eq!((g.name.as_str(), g.min, g.max, g.default), ("gain", -60.0, 24.0, 0.0));
    assert_eq!(g.unit.as_deref(), Some("dB"));
    assert_eq!(s.info.main_in_channels, 2);
    assert!(s.info.has_state && s.info.has_presets);
    let presets = s.host.list_presets().unwrap();
    let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Quiet", "Loud"]);
    let v = s.host.load_preset("quiet").unwrap();
    assert_eq!(v[0].value, -12.0);
    let st = s.host.get_state().unwrap();
    s.host.set_params(&[("gain".into(), "3".into())]).unwrap();
    s.host.set_state(&st).unwrap();
    assert_eq!(s.host.get_params().unwrap()[0].value, -12.0);
    let e = s.host.load_preset("nope").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
    // a preset applies in a run, and is recorded
    let o = PluginOptions { preset: Some("Quiet".into()), ..opts() };
    let run = process_effect_with(&p, &tone(1000), &o).unwrap();
    let ratio = run.audio.peak() / tone(1000).peak();
    assert!((ratio - 0.2512).abs() < 0.002, "{ratio}");
    let rec = run.record(&p);
    assert_eq!(rec["id"], "ferraus.test.gain");
    assert_eq!(rec["format"], "clap");
    assert!(rec["state_b64"].as_str().unwrap().len() > 8);
}

#[test]
fn instruments_are_rejected_as_effects() {
    let d = TempDir::new("notfx");
    let p = plugin(&d, "ferraus.test.sine");
    let e = process_effect(&p, &tone(100), &[], None, 0.0).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
}
