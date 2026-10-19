//! Fuzz the decoder entry point: arbitrary bytes, with and without a format hint, must
//! never panic or hang. Run: `cargo +nightly fuzz run decode` (from crates/ferraus-codec).
#![no_main]

use ferraus_core::ctx::AudioIo;
use libfuzzer_sys::fuzz_target;

const HINTS: &[Option<&str>] = &[
    None,
    Some("wav"),
    Some("aiff"),
    Some("flac"),
    Some("mp3"),
    Some("ogg"),
    Some("opus"),
    Some("m4a"),
    Some("mkv"),
    Some("caf"),
];

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let hint = HINTS[data[0] as usize % HINTS.len()];
    let _ = ferraus_codec::CodecIo.read_bytes(&data[1..], hint);
});
