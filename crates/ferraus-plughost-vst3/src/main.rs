//! `ferraus-plughost-vst3`: the out-of-process VST3 host for ferraus. It speaks the same
//! protocol as the CLAP host (`crates/ferraus-plugins/PROTOCOL.md`), loads exactly one
//! plugin, and never opens a GUI. Apache-2.0 OR MIT like the rest of ferraus: the VST3 SDK is
//! MIT since 3.8, and the `vst3` bindings (generated from the 3.8 headers) are MIT OR Apache-2.0
//! (ADR 0008).

mod vst3_backend;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("--version") | Some("-V") => {
            println!(
                "ferraus-plughost-vst3 {} (protocol {})",
                env!("CARGO_PKG_VERSION"),
                ferraus_plugins::protocol::PROTOCOL_VERSION
            );
            return;
        }
        Some("--help") | Some("-h") => {
            println!(
                "ferraus-plughost-vst3 — out-of-process VST3 plugin host for ferraus (Apache-2.0 OR MIT)\n\n\
                 Started by ferraus; speaks the ferraus plugin-host protocol on stdin/stdout.\n\
                 Options: --version, --help"
            );
            return;
        }
        Some(other) => {
            eprintln!("unknown argument '{other}'");
            std::process::exit(2);
        }
        None => {}
    }
    let mut backend = vst3_backend::Vst3Backend::new();
    let code = ferraus_plugins::host_rt::serve(&mut backend);
    drop(backend);
    std::process::exit(code);
}
