//! Sign the sounds registry manifest (run in CI, where the private key is a secret).
//!
//! ```text
//! # print a fresh key pair (base64 seed for the CI secret, base64 public key for the repo)
//! cargo run -p ferraus-plugins --example sign_registry -- --generate
//! # write crates/ferraus-plugins/sounds/registry.json.sig
//! FERRAUS_REGISTRY_SIGNING_KEY=<base64 seed> \
//!   cargo run -p ferraus-plugins --example sign_registry -- crates/ferraus-plugins/sounds/registry.json
//! ```

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use std::io::Read;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("--generate") {
        let mut seed = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut seed))
            .expect("read /dev/urandom (key generation needs a Unix-like OS)");
        let key = SigningKey::from_bytes(&seed);
        println!("private seed (keep secret): {}", b64().encode(seed));
        println!("public key  (commit):       {}", b64().encode(key.verifying_key().to_bytes()));
        return;
    }
    let Some(path) = args.first() else {
        eprintln!("usage: sign_registry <registry.json> | --generate");
        std::process::exit(2);
    };
    // the key comes from FERRAUS_REGISTRY_SIGNING_KEY, or `--key-file <file>` holding the
    // seed (or the output of --generate)
    let seed = match args.iter().position(|a| a == "--key-file") {
        Some(i) => {
            let text = std::fs::read_to_string(&args[i + 1]).expect("read key file");
            text.lines()
                .find_map(|l| l.strip_prefix("private seed (keep secret):"))
                .unwrap_or(text.lines().next().unwrap_or(""))
                .trim()
                .to_string()
        }
        None => std::env::var("FERRAUS_REGISTRY_SIGNING_KEY").expect("set FERRAUS_REGISTRY_SIGNING_KEY"),
    };
    let seed: [u8; 32] =
        b64().decode(seed.trim()).expect("signing key is base64").try_into().expect("signing key is a 32-byte seed");
    let key = SigningKey::from_bytes(&seed);
    let bytes = std::fs::read(path).expect("read manifest");
    serde_json::from_slice::<serde_json::Value>(&bytes).expect("manifest is valid JSON");
    let sig = key.sign(&bytes);
    let out = format!("{path}.sig");
    std::fs::write(&out, format!("{}\n", b64().encode(sig.to_bytes()))).expect("write signature");
    println!("signed {path} -> {out} (public key {})", b64().encode(key.verifying_key().to_bytes()));
}
