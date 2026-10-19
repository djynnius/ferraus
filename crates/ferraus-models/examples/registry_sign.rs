//! Registry manifest signing tool. Signing happens only in CI: the release workflow keeps the
//! Ed25519 private key in the `FERRAUS_REGISTRY_SIGNING_KEY` GitHub Actions secret (64 hex
//! characters) and runs
//!
//! ```text
//! cargo run -p ferraus-models --example registry_sign -- sign models/registry.json
//! ```
//!
//! which writes `models/registry.json.sig` (hex signature over the exact file bytes).
//!
//! Other subcommands:
//!
//! * `keygen <secret-out-file>` — create a keypair; writes the secret (hex) to the given file
//!   (keep it OUTSIDE the repository) and prints the public key for `models/registry.pub`.
//!   The 32-byte seed is read from `/dev/urandom` (Unix) or must be supplied as
//!   `FERRAUS_KEYGEN_SEED_HEX` on other platforms.
//! * `verify <manifest> [<sig>]` — check a signature against the embedded public key.

use ed25519_dalek::{Signer, SigningKey};
use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("keygen") => {
            let out = args.get(1).expect("usage: keygen <secret-out-file>");
            let seed = seed();
            let sk = SigningKey::from_bytes(&seed);
            std::fs::write(out, hex::encode(sk.to_bytes())).expect("write secret");
            println!("{}", hex::encode(sk.verifying_key().to_bytes()));
        }
        Some("sign") => {
            let manifest = args.get(1).expect("usage: sign <manifest.json>");
            let key_hex = std::env::var("FERRAUS_REGISTRY_SIGNING_KEY")
                .expect("FERRAUS_REGISTRY_SIGNING_KEY must hold the hex secret key (CI secret)");
            let bytes: [u8; 32] = hex::decode(key_hex.trim())
                .expect("hex key")
                .try_into()
                .expect("32-byte key");
            let sk = SigningKey::from_bytes(&bytes);
            let body = std::fs::read(manifest).expect("read manifest");
            let sig = sk.sign(&body);
            let sig_path = format!("{manifest}.sig");
            std::fs::write(&sig_path, hex::encode(sig.to_bytes())).expect("write sig");
            eprintln!("wrote {sig_path}");
        }
        Some("verify") => {
            let manifest = args.get(1).expect("usage: verify <manifest> [<sig>]");
            let sig_path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| format!("{manifest}.sig"));
            let body = std::fs::read(manifest).expect("read manifest");
            let sig = std::fs::read_to_string(sig_path).expect("read sig");
            match ferraus_models::manifest::parse_signed(
                &ferraus_models::manifest::embedded_public_key(),
                &body,
                &sig,
            ) {
                Ok(m) => println!("ok: {} models", m.models.len()),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(9);
                }
            }
        }
        _ => {
            eprintln!("usage: registry_sign keygen <secret-out> | sign <manifest> | verify <manifest> [<sig>]");
            std::process::exit(2);
        }
    }
}

fn seed() -> [u8; 32] {
    if let Ok(h) = std::env::var("FERRAUS_KEYGEN_SEED_HEX") {
        return hex::decode(h.trim())
            .expect("hex seed")
            .try_into()
            .expect("32-byte seed");
    }
    let mut s = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut s))
        .expect("read /dev/urandom (or set FERRAUS_KEYGEN_SEED_HEX)");
    s
}
