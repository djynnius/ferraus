# ADR 0006: `denoise --mode ml` defaults to RNNoise until DeepFilterNet 3 has a runtime

Status: accepted (2026-10-19)

The PRD names DeepFilterNet 3 as the default ML denoiser. Its official Rust runtime
(`deep_filter`) is not usable from crates.io: version 0.2.5 contains no inference code, and the
upstream git revision only builds with tract pinned to exactly 0.21.4. A git dependency would
block crates.io publishing and violates `deny.toml` (`unknown-git = "deny"`).

Decision: `--model` defaults to `rnnoise` (pure-Rust `nnnoiseless`, weights built in, works
offline with no download). `--model dfn3` remains, and fails fast with exit code 5 and
`ferraus models pull denoise/dfn3`, never silently substituting another model.

Follow-up (tracked in the roadmap): run the DeepFilterNet 3 ONNX export (encoder, ERB decoder,
DF decoder) through `ferraus-ml`'s ONNX Runtime and switch the default back to `dfn3` once the
VoiceBank-DEMAND PESQ parity test (±0.05) passes.
