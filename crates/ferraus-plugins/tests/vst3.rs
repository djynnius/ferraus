//! VST3 hosting through `ferraus-plughost-vst3`, against the VST3 gain class exported by
//! the test plugin library.

mod common;

use ferraus_core::testing::TempDir;
use ferraus_core::AudioBuffer;
use ferraus_plugins::api::{PluginSession, Tail};
use ferraus_plugins::client::PluginFormat;
use ferraus_plugins::scan::{self, ScanOptions};
use ferraus_plugins::{process_effect_with, PluginOptions};
use std::time::Duration;

fn tone() -> AudioBuffer {
    let s: Vec<f32> = (0..3000).map(|i| (i as f32 * 0.07).sin() * 0.7).collect();
    AudioBuffer::from_planar(44100, vec![s.clone(), s])
}

#[test]
fn vst3_scan_info_params_and_state() {
    let d = TempDir::new("vst3");
    let dir = d.path("vst3");
    common::vst3_bundle(&dir, "FerrausTestVst");
    let cache = d.path("scan.json");
    let r = scan::scan(&ScanOptions {
        extra: vec![dir.clone()],
        formats: vec![PluginFormat::Vst3],
        standard: false,
        watchdog: Duration::from_secs(20),
        cache_path: cache.clone(),
        ..Default::default()
    })
    .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.plugins.len(), 1, "{:?}", r.plugins);
    let p = &r.plugins[0];
    assert_eq!(
        (p.name.as_str(), p.kind.as_str(), p.source.as_str()),
        ("Ferraus Test VST3 Gain", "audio-effect", "probe")
    );
    assert_eq!(p.id.len(), 32);

    let plugin = scan::resolve_plugin_with("Ferraus Test VST3 Gain", &[], Duration::from_secs(20), &cache).unwrap();
    assert_eq!(plugin.format, PluginFormat::Vst3);
    let mut s = PluginSession::open(&plugin, Duration::from_secs(20)).unwrap();
    assert_eq!(s.info.format, "vst3");
    let g = &s.info.params[0];
    assert_eq!((g.name.as_str(), g.unit.as_deref()), ("Gain", Some("dB")));
    assert!((g.min + 60.0).abs() < 1e-9 && (g.max - 24.0).abs() < 1e-9 && g.default.abs() < 1e-9);
    assert_eq!((s.info.main_in_channels, s.info.main_out_channels), (2, 2));
    let v = s.host.set_params(&[("gain".into(), "-6dB".into())]).unwrap();
    assert!((v[0].value + 6.0).abs() < 1e-9, "{v:?}");
    assert_eq!(v[0].text, "-6.00");
    let st = s.host.get_state().unwrap();
    s.host.set_params(&[("gain".into(), "norm:1".into())]).unwrap();
    s.host.set_state(&st).unwrap();
    assert!((s.host.get_params().unwrap()[0].value + 6.0).abs() < 1e-9);
    s.host.shutdown();

    // processing: -6 dB halves the amplitude; the captured state reproduces it
    let o = PluginOptions {
        params: vec![("Gain".into(), "-6dB".into())],
        tail: Tail::Auto,
        watchdog: Duration::from_secs(20),
        ..Default::default()
    };
    let run = process_effect_with(&plugin, &tone(), &o).unwrap();
    assert_eq!(run.audio.frames(), tone().frames(), "VST3 kNoTail adds nothing");
    let ratio = run.audio.peak() / tone().peak();
    assert!((ratio - 0.501).abs() < 0.003, "{ratio}");
    let o2 = PluginOptions { params: Vec::new(), state: run.state.clone(), ..o.clone() };
    let again = process_effect_with(&plugin, &tone(), &o2).unwrap();
    assert_eq!(again.audio.data, run.audio.data);
}

#[test]
fn vst3_bundle_without_host_metadata_still_lists_via_moduleinfo() {
    let d = TempDir::new("vst3mi");
    let b = common::fake_vst3(&d.0, "Only Metadata", false);
    let r = scan::probe_bundle(PluginFormat::Vst3, &b, Duration::from_secs(10)).unwrap();
    assert_eq!(r[0].name, "Only Metadata");
    assert_eq!(r[0].kind, "audio-effect");
    // loading it fails cleanly (no binary) with a plugin error, not a crash of the caller
    let p = r[0].plugin_ref();
    let e = PluginSession::open(&p, Duration::from_secs(10)).err().unwrap();
    assert_eq!(e.kind, ferraus_core::ErrorKind::Plugin, "{e}");
    assert!(e.message.contains("no binary"), "{e}");
}
