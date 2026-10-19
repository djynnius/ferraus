//! `ferraus-plughost`: the out-of-process CLAP host. ferraus spawns one per plugin instance
//! and talks to it over stdin/stdout (length-prefixed JSON frames) and a shared-memory audio
//! ring; see `crates/ferraus-plugins/PROTOCOL.md`. It never opens a GUI.

mod clap_backend;

const HELP: &str = "ferraus-plughost — out-of-process CLAP plugin host for ferraus

This program is started by ferraus; it is not meant to be run by hand. It loads exactly one
plugin, speaks the ferraus plugin-host protocol on stdin/stdout, and exchanges audio through
shared memory. It never opens a plugin GUI.

Options:
  --version   Print the version and protocol version
  --help      Print this help
  --probe <path>  Print the plugin descriptors of a .clap bundle as JSON (debugging aid)";

/// Entry point of the `ferraus-plughost` binary (shipped by the `ferraus` package).
pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("--version") | Some("-V") => {
            println!(
                "ferraus-plughost {} (protocol {})",
                env!("CARGO_PKG_VERSION"),
                ferraus_plugins::protocol::PROTOCOL_VERSION
            );
            return;
        }
        Some("--help") | Some("-h") => {
            println!("{HELP}");
            return;
        }
        Some("--probe") => {
            use ferraus_plugins::host_rt::Backend;
            let Some(path) = args.get(1) else {
                eprintln!("--probe needs a path");
                std::process::exit(2);
            };
            let mut b = clap_backend::ClapBackend::new();
            match b.probe(std::path::Path::new(path)) {
                Ok(d) => println!("{}", serde_json_pretty(&d)),
                Err(e) => {
                    eprintln!("{}: {}", e.code, e.message);
                    std::process::exit(1);
                }
            }
            return;
        }
        Some(other) => {
            eprintln!("unknown argument '{other}'\n\n{HELP}");
            std::process::exit(2);
        }
        None => {}
    }
    let mut backend = clap_backend::ClapBackend::new();
    let code = ferraus_plugins::host_rt::serve(&mut backend);
    drop(backend);
    std::process::exit(code);
}

fn serde_json_pretty(d: &[ferraus_plugins::protocol::Descriptor]) -> String {
    let mut s = String::from("[");
    for (i, x) in d.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "\n  {{\"id\": {:?}, \"name\": {:?}, \"vendor\": {:?}, \"version\": {:?}, \"kind\": {:?}}}",
            x.id, x.name, x.vendor, x.version, x.kind
        ));
    }
    s.push_str("\n]");
    s
}
