# Performance against the PRD hardware tiers

Measured 2026-10-19 on an Apple Silicon laptop (10 cores) with a `--release` build, CPU only,
on a 60-second 44.1 kHz stereo synthetic mix; 4-minute figures are extrapolated linearly.
`ferraus bench` reproduces the per-effect real-time factors on any machine.

| Command | 60 s input | 4-minute song (extrapolated) | PRD target |
| --- | --- | --- | --- |
| `fx` (compress, eq, reverb, limit) | 0.81 s | ~3.2 s | under 5 s |
| `denoise --auto` (profile gating) | 0.88 s | ~3.5 s | under 5 s |
| `tune` (pYIN + TD-PSOLA) | 0.71 s | ~2.8 s | under 60 s |
| `separate --tier fast` (htdemucs ONNX) | 23.9 s | ~1.6 min | under 2 min (fast), under 4 min (4-stem Demucs class) |

Other measurements reported by the crates' release benchmarks:

* Every streaming effect in `ferraus-dsp` runs at ≥ 86x real time single-threaded (slowest:
  vocoder `--follow`); most exceed 300x.
* Restoration effects run at ≥ 10x real time (slowest: `guitar` with 4x oversampling).
* Profile denoise runs at 83–91x real time single-threaded at 44.1/48 kHz stereo.
* DeepFilterNet 3 (`denoise --mode ml`) processes audio about 33x faster than real time on CPU
  (0.03 s per second of audio).
* `convert` of a 3-hour 48 kHz stereo WAV to FLAC peaks at 0.7 MB of heap (streaming path).

Release archive size (macOS arm64, `ferraus` + `ferraus-plughost`, tar.xz): 9.9 MB;
uncompressed binary 37.6 MB including ONNX Runtime. Target: under 60 MB.

Not yet measured: the `quality` and `max` separation tiers (their RoFormer checkpoints are not
license-cleared; see `crates/ferraus-models/registry/registry.json`) and GPU timings.
