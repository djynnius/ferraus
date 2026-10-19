//! Render one note through an instrument plugin with the public API (reference for
//! `ferraus-synth`'s `InstrumentSource::Plugin`).
//!
//! ```text
//! cargo run -p ferraus-plugins --example render_note -- "<name, id, or bundle path>" out.wav [key] [param=value ...]
//! ```

use ferraus_plugins::{render_instrument_with, MidiEvent, PluginOptions, PluginRef};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: render_note <plugin> <out.wav> [midi key] [param=value ...]");
        std::process::exit(2);
    }
    let plugin = PluginRef::resolve(&args[0], &[]).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(e.exit_code)
    });
    let key: u8 = args.get(2).and_then(|k| k.parse().ok()).unwrap_or(60);
    let params =
        args.iter().skip(3).filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
    let sr = 48000u32;
    let events = [MidiEvent::note_on(sr as u64 / 10, 0, key, 0.8), MidiEvent::note_off(sr as u64, 0, key)];
    let opts = PluginOptions { params, ..Default::default() };
    match render_instrument_with(&plugin, &events, sr, 2.0, &opts) {
        Ok(run) => {
            let a = &run.audio;
            let onset = a.data[0].iter().position(|v| v.abs() > 1e-4);
            println!(
                "{} ({}): {} frames, peak {:.3}, first sound at frame {:?} (note at {}), latency {}",
                plugin.name,
                plugin.format,
                a.frames(),
                a.peak(),
                onset,
                sr / 10,
                run.latency
            );
            std::fs::write(&args[1], ferraus_core::wav::encode(a, 32, true).expect("encode")).expect("write");
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(e.exit_code);
        }
    }
}
