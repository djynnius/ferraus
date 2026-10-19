# ADR 0006: `denoise --mode ml` model default

Status: accepted (2026-10-19); amended 2026-10-19: the default is DeepFilterNet 3 again,
running on ONNX Runtime.

## Original decision (superseded)

The PRD names DeepFilterNet 3 as the default ML denoiser. Its official Rust runtime
(`deep_filter`) is not usable from crates.io: version 0.2.5 contains no inference code, and the
upstream git revision only builds with tract pinned to exactly 0.21.4. A git dependency would
block crates.io publishing and violates `deny.toml` (`unknown-git = "deny"`).

So `--model` defaulted to `rnnoise` (pure-Rust `nnnoiseless`, weights built in, works offline
with no download), and `--model dfn3` failed fast with exit code 5 and
`ferraus models pull denoise/dfn3`, never silently substituting another model. The follow-up
was to run the DeepFilterNet 3 ONNX export through `ferraus-ml` and switch back once it
matched the reference.

## Amendment: DeepFilterNet 3 is the default

`ferraus-denoise` now runs the upstream ONNX export (`denoise/dfn3`, MIT OR Apache-2.0:
`enc.onnx`, `erb_dec.onnx`, `df_dec.onnx`, `config.ini`) on ONNX Runtime through `ferraus-ml`
(no git dependency, no tract). The STFT, ERB/complex feature normalization, ERB gains, deep
filter and ISTFT are implemented in Rust after `libDF` and `df/enhance.py`
(`crates/ferraus-denoise/src/dfn3.rs`).

Validation (2026-10-19):

* Parity: on synthetic speech in fan noise, speech in hum, and clean speech, the output
  matches both the ONNX graphs driven by upstream `libdf` features and the official PyTorch
  `df.enhance.enhance` (DeepFilterNet3 checkpoint) to a maximum absolute difference of
  about 1e-6 (SI-SDR between them > 110 dB). Identical output implies identical PESQ, which
  supersedes the planned VoiceBank-DEMAND PESQ parity check (±0.05).
* Quality on synthetic speech + noise (`tests/dfn3.rs`, source-filter voice with jitter,
  formant glides and fricatives; hiss, fan, 60 Hz hum, street at 0/5/10 dB SNR): DFN3 wins
  all 12 conditions; mean SI-SDR improvement +12.4 dB vs +6.8 dB for RNNoise (hum alone
  +8.1…+10.4 vs +3.6…+5.0).
* On a real speech recording (DeepFilterNet's `clean_freesound_33711.wav`, used locally
  only) with the same noises: +12.3 dB vs +6.6 dB mean, better in every condition; clean
  speech passes at 40 dB SI-SDR.
* Known weakness: on the older purely harmonic `speech_like` fixture (phase-locked sine
  partials, no jitter or noise) under a 60 Hz hum series stronger than the voice, DFN3
  treats the synthetic voice as tonal noise (−5 to −14 dB). Real speech in the same hum
  improves by 14–17 dB, so this is a property of the fixture, not a regression users hit.
* Long inputs run in 30 s segments (1 s warm-up, 0.5 s crossfade of gains and filters);
  local SI-SDR at the boundaries equals the surroundings. CPU real-time factor ≈ 0.03.

Decision: `--model` defaults to `dfn3` (builds without the `dfn3` feature default to
`rnnoise`). The model downloads under ADR 0004: without `-y`/`models.auto_download` a missing
model is exit code 5 with `ferraus models pull denoise/dfn3`; it never falls back to RNNoise
silently. `--model rnnoise` stays the built-in, download-free choice.
