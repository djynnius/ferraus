//! Scanning, the sound resolver, and registry installs (library level, no global state).

mod common;

use ferraus_core::testing::TempDir;
use ferraus_core::ErrorKind;
use ferraus_plugins::client::PluginFormat;
use ferraus_plugins::resolve::{self, SoundKind, SoundsEnv};
use ferraus_plugins::scan::{self, ScanOptions};
use ferraus_plugins::sounds::install::{self, InstallOptions};
use ferraus_plugins::sounds::{Download, Entry, License};
use ferraus_plugins::{render_instrument, MidiEvent};
use std::collections::BTreeMap;
use std::time::Duration;

fn wd() -> Duration {
    Duration::from_secs(20)
}

#[test]
fn scan_finds_clap_and_vst3_and_caches() {
    let d = TempDir::new("scan");
    let plugins = d.path("plugins");
    common::clap_bundle(&plugins.join("nested"), "FerrausTest");
    common::fake_vst3(&plugins, "Fake Synth", true);
    let cache = d.path("cache/scan.json");
    let opts = ScanOptions {
        extra: vec![plugins.clone()],
        standard: false,
        watchdog: wd(),
        cache_path: cache.clone(),
        ..Default::default()
    };
    let r = scan::scan(&opts).unwrap();
    assert_eq!(r.bundles, 2);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let ids: Vec<&str> = r.plugins.iter().map(|p| p.id.as_str()).collect();
    for id in
        ["ferraus.test.gain", "ferraus.test.delay", "ferraus.test.sine", "ferraus.test.crash", "ferraus.test.hang"]
    {
        assert!(ids.contains(&id), "{id} not in {ids:?}");
    }
    let sine = r.plugins.iter().find(|p| p.id == "ferraus.test.sine").unwrap();
    assert_eq!((sine.kind.as_str(), sine.format), ("instrument", PluginFormat::Clap));
    let v = r.plugins.iter().find(|p| p.format == PluginFormat::Vst3).unwrap();
    assert_eq!((v.name.as_str(), v.kind.as_str(), v.source.as_str()), ("Fake Synth", "instrument", "moduleinfo"));
    assert!(cache.is_file());
    // the second scan is served from the cache
    let r2 = scan::scan(&opts).unwrap();
    assert_eq!((r2.probed, r2.cached), (0, 2));
    assert_eq!(r2.plugins.len(), r.plugins.len());
    // resolution by name, id, and path#id; ambiguity and absence are reported
    let p = scan::resolve_plugin_with("Ferraus Test Gain", std::slice::from_ref(&plugins), wd(), &cache).unwrap();
    assert_eq!(p.plugin_id.as_deref(), Some("ferraus.test.gain"));
    let p = scan::resolve_plugin_with("plugin:\"ferraus.test.delay\"", &[], wd(), &cache).unwrap();
    assert_eq!(p.name, "Ferraus Test Delay");
    let bundle = plugins.join("nested/FerrausTest.clap");
    let p = scan::resolve_plugin_with(&format!("{}#ferraus.test.sine", bundle.display()), &[], wd(), &cache).unwrap();
    assert_eq!(p.name, "Ferraus Test Sine");
    let e = scan::resolve_plugin_with(&bundle.display().to_string(), &[], wd(), &cache).unwrap_err();
    assert_eq!(e.code, "plugin_ambiguous");
    let e = scan::resolve_plugin_with("Ferraus Test", &[], wd(), &cache).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage);
    let e = scan::resolve_plugin_with("No Such Synth", &[], wd(), &cache).unwrap_err();
    assert_eq!((e.kind, e.code.as_str()), (ErrorKind::Plugin, "plugin_not_found"));
}

#[test]
fn a_crashing_bundle_does_not_stop_the_scan() {
    let d = TempDir::new("badbundle");
    let plugins = d.path("plugins");
    common::clap_bundle(&plugins, "Good");
    std::fs::write(plugins.join("Broken.clap"), b"this is not a shared library").unwrap();
    let opts = ScanOptions {
        extra: vec![plugins],
        standard: false,
        watchdog: wd(),
        cache_path: d.path("scan.json"),
        ..Default::default()
    };
    let r = scan::scan(&opts).unwrap();
    assert_eq!(r.plugins.len(), 5);
    assert_eq!(r.errors.len(), 1);
    assert!(r.errors[0].path.ends_with("Broken.clap"));
}

#[test]
fn sound_path_makes_clap_sf2_and_sfz_discoverable() {
    let d = TempDir::new("soundpath");
    let env = SoundsEnv::under(&d.path("home"));
    let folder = common::sound_folder(&d.0);
    let sounds = resolve::discover(&env, std::slice::from_ref(&folder), wd()).unwrap();
    let kinds: Vec<(SoundKind, &str)> = sounds.iter().map(|s| (s.kind, s.name.as_str())).collect();
    assert!(kinds.contains(&(SoundKind::Clap, "Ferraus Test Sine")), "{kinds:?}");
    assert!(kinds.contains(&(SoundKind::Sf2, "Warm Strings")), "{kinds:?}");
    assert!(kinds.contains(&(SoundKind::Sfz, "Felt Piano")), "{kinds:?}");
    // sfz samples are not mistaken for impulse responses
    assert!(!kinds.iter().any(|(k, _)| *k == SoundKind::Ir), "{kinds:?}");

    let paths = [folder.clone()];
    let r = resolve::resolve_sound(&env, "Ferraus Test Sine", &paths, wd()).unwrap().unwrap();
    assert_eq!((r.via.as_str(), r.sound.kind), ("name", SoundKind::Clap));
    let r = resolve::resolve_sound(&env, "piano", &paths, wd()).unwrap().unwrap();
    assert_eq!((r.via.as_str(), r.sound.name.as_str()), ("tag", "Felt Piano"));
    assert_eq!(r.sound.category.as_deref(), Some("keys"));
    let r = resolve::resolve_sound(&env, "strings", &paths, wd()).unwrap().unwrap();
    assert_eq!(r.sound.kind, SoundKind::Sf2);
    assert!(resolve::resolve_sound(&env, "tuba", &paths, wd()).unwrap().is_none());
    // explicit sources and aliases
    let sf2 = folder.join("Warm Strings.sf2");
    let r = resolve::resolve_sound(&env, &format!("sf2:{}", sf2.display()), &[], wd()).unwrap().unwrap();
    assert_eq!(r.via, "explicit");
    let mut a = BTreeMap::new();
    a.insert("lead".to_string(), "ferraus.test.sine".to_string());
    resolve::write_aliases(&env, &a).unwrap();
    let r = resolve::resolve_sound(&env, "lead", &paths, wd()).unwrap().unwrap();
    assert_eq!((r.via.as_str(), r.alias_target.as_deref()), ("alias", Some("ferraus.test.sine")));
    // the resolved plugin renders through the public API
    let p = r.sound.plugin_ref().unwrap();
    let audio = render_instrument(&p, &[MidiEvent::note_on(100, 0, 60, 1.0)], 44100, 0.1, &[], None).unwrap();
    assert!(audio.peak() > 0.1);
    // folder listing with categories
    let listed = resolve::discover_folder(&env, &folder, wd()).unwrap();
    assert_eq!(listed.iter().filter(|s| s.kind == SoundKind::Clap).count(), 5);
}

fn entry(id: &str, kind: SoundKind, url: String, archive: &str, sha: Option<String>) -> Entry {
    Entry {
        id: id.into(),
        name: format!("Test {id}"),
        kind,
        formats: vec![],
        version: Some("1.0".into()),
        homepage: String::new(),
        description: String::new(),
        license: License { spdx: "CC0-1.0".into(), text: "Public domain.".into(), url: String::new() },
        platforms: vec!["any".into()],
        downloads: vec![Download {
            platform: "any".into(),
            url,
            mirrors: vec![],
            archive: archive.into(),
            size_bytes: None,
            sha256: sha,
            contents: String::new(),
        }],
        status: "verified".into(),
        category: Some("keys".into()),
        tags: vec!["piano".into()],
        aliases: vec![],
        install_from: None,
        notes: String::new(),
    }
}

fn iopts() -> InstallOptions {
    InstallOptions {
        yes: true,
        offline: false,
        allow_unverified: false,
        dry_run: false,
        os: "linux".into(),
        watchdog: wd(),
    }
}

#[test]
fn install_verifies_hashes_and_places_files() {
    let d = TempDir::new("install");
    let env = SoundsEnv::under(&d.0);
    let body = common::fake_sf2();
    let base = common::http_server(vec![("/gm.sf2".into(), body.clone())], false);
    let url = format!("{base}/gm.sf2");

    // wrong hash: refused, nothing installed, partial download removed
    let bad = entry("gm", SoundKind::Sf2, url.clone(), "sf2", Some("0".repeat(64)));
    let e = install::install_entry(&env, &bad, &iopts(), &mut |_| {}).unwrap_err();
    assert_eq!((e.kind, e.code.as_str()), (ErrorKind::Verification, "checksum_mismatch"), "{e}");
    assert!(resolve::read_installed(&env).is_empty());
    assert!(!env.sounds_dir.join("gm").exists());
    let leftovers: Vec<_> = std::fs::read_dir(env.downloads_dir()).map(|r| r.flatten().collect()).unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // consent, offline, unverified
    let good = entry("gm", SoundKind::Sf2, url.clone(), "sf2", Some(common::sha256_hex(&body)));
    let e = install::install_entry(&env, &good, &InstallOptions { yes: false, ..iopts() }, &mut |_| {}).unwrap_err();
    assert_eq!(e.code, "download_needs_consent");
    assert!(e.hint.unwrap().contains("-y"));
    let e = install::install_entry(&env, &good, &InstallOptions { offline: true, ..iopts() }, &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Network);
    let unv = entry("unv", SoundKind::Sf2, url.clone(), "sf2", None);
    let e = install::install_entry(&env, &unv, &iopts(), &mut |_| {}).unwrap_err();
    assert_eq!((e.kind, e.code.as_str()), (ErrorKind::Verification, "sound_unverified"));
    assert!(
        install::install_entry(&env, &unv, &InstallOptions { allow_unverified: true, ..iopts() }, &mut |_| {}).is_ok()
    );

    // correct hash installs into <sounds>/<id>/ and the sound resolves by registry id
    let rec = install::install_entry(&env, &good, &iopts(), &mut |_| {}).unwrap().unwrap();
    assert_eq!(rec.paths, vec![env.sounds_dir.join("gm")]);
    assert_eq!(std::fs::read(env.sounds_dir.join("gm/gm.sf2")).unwrap(), body);
    let r = resolve::resolve_sound(&env, "gm", &[], wd()).unwrap().unwrap();
    assert_eq!(r.sound.kind, SoundKind::Sf2);
    assert_eq!(r.sound.registry_id.as_deref(), Some("gm"));
    // remove
    let removed = install::remove(&env, "gm", false, wd()).unwrap();
    assert_eq!(removed.len(), 1);
    assert!(!env.sounds_dir.join("gm").exists());
}

#[test]
fn installs_plugin_archives_into_the_plugin_folder() {
    let d = TempDir::new("pluginzip");
    let env = SoundsEnv::under(&d.path("home"));
    let lib = common::ensure_built();
    let mut zipped = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut zipped));
        let o: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        z.add_directory("Ferraus-1.0/", o).unwrap();
        z.start_file("Ferraus-1.0/FerrausTest.clap", o).unwrap();
        std::io::Write::write_all(&mut z, &std::fs::read(&lib).unwrap()).unwrap();
        z.start_file("Ferraus-1.0/README.txt", o).unwrap();
        std::io::Write::write_all(&mut z, b"hi").unwrap();
        z.start_file("../escape.txt", o).unwrap();
        std::io::Write::write_all(&mut z, b"nope").unwrap();
        z.finish().unwrap();
    }
    let base = common::http_server(vec![("/f.zip".into(), zipped.clone())], false);
    let e = entry("ferraus-test", SoundKind::Clap, format!("{base}/f.zip"), "zip", Some(common::sha256_hex(&zipped)));
    let rec = install::install_entry(&env, &e, &iopts(), &mut |_| {}).unwrap().unwrap();
    assert_eq!(rec.paths, vec![env.clap_dir.join("FerrausTest.clap")]);
    assert!(!d.path("home").join("cache/sounds/escape.txt").exists());
    // rescanned: the plugin is in the cache and resolvable
    let sounds = resolve::discover(&env, &[], wd()).unwrap();
    assert!(sounds.iter().any(|s| s.plugin_id.as_deref() == Some("ferraus.test.sine")));
    // --from registers a user-provided bundle
    assert!(install::install_from(&env, &d.path("nonexistent.clap"), None, None, false).is_err());
    let user = d.path("dl/My Synth.clap");
    std::fs::create_dir_all(user.parent().unwrap()).unwrap();
    std::fs::copy(&lib, &user).unwrap();
    let rec = install::install_from(&env, &user, None, None, false).unwrap();
    assert_eq!((rec.kind, rec.source.as_str(), rec.id.as_str()), (SoundKind::Clap, "user", "my-synth"));
    assert!(env.clap_dir.join("My Synth.clap").is_file());
    let e = install::install_from(&env, &d.path("dl/Setup.exe"), None, None, false);
    assert!(e.is_err());
}

#[test]
fn downloads_resume_after_a_dropped_connection() {
    let d = TempDir::new("resume");
    let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let base = common::http_server(vec![("/big.bin".into(), body.clone())], true);
    let dest = d.path("big.part");
    let first = install::download(&format!("{base}/big.bin"), &dest, &mut |_, _| {});
    assert!(first.is_err(), "the first response is truncated");
    let partial = std::fs::metadata(&dest).unwrap().len();
    assert!(partial > 0 && partial < body.len() as u64);
    install::download(&format!("{base}/big.bin"), &dest, &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), body);
}
