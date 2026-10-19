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

## Separation tiers and memory

Measured 2026-10-19 on the same laptop (CPU only) with a 4-minute 44.1 kHz stereo synthetic
song, `/usr/bin/time -l`, default `--max-memory 2GB` unless noted. "Peak" is the macOS peak
memory footprint (RSS is similar or lower).

| Run | Time | Peak | Memory plan (`result.memory`) | PRD target |
| --- | --- | --- | --- | --- |
| `--tier fast` | 117 s | 2.3 GB | in memory, lean sessions | under 2 min |
| `--tier fast --max-memory 8GB` | 100 s | 4.9 GB | in memory, full sessions | |
| `--tier fast --stems 6` | 115 s | 1.9 GB | spilled, lean | |
| `--tier balanced` | 430 s | 2.2 GB | in memory, lean | under 4 min (missed: 4 full Demucs passes) |
| `--tier quality` | 937 s | 3.4 GB | spilled, lean (`fits: false`: the RoFormer alone needs ~3.7 GB) | under 15 min (missed by ~4%) |
| `--tier quality --max-memory 8GB` | 942 s | 5.3 GB | in memory, full sessions | |

* The RoFormer dominates `quality`: about 12 s per 8-second window on this CPU (47 windows for
  4 minutes); the three fine-tuned Demucs models take about 2 minutes each.
* `max` runs 6 models × 4 passes (2 shifts, each also channel-swapped), about 4× `quality`.
* Lean sessions (`ferraus_ml::SessionOptions::lean`: no constant folding, no memory pattern,
  no weight prepacking) cut htdemucs from 5.1 GB to 1.3–1.5 GB for about 17% more time, and the
  RoFormer from 6.0 GB to 3.7 GB at the same speed. ferraus picks them when a model's measured
  peak (`io.params.ort_peak_mb` in the registry) does not fit `--max-memory`.

**60-minute input.** Separation streams: models run one at a time, window by window, and the
mix, member estimates, and final stems spill to temp files when they do not fit the budget, so
peak memory is the model plus a few blocks, independent of length. The remaining whole-song
buffers are the decoded input (file bytes + samples) and the one stem being encoded, because
the `AudioIo` API reads and writes whole buffers.

| 60-minute 44.1 kHz stereo input, default `--max-memory 2GB` | Time | Peak RSS | Peak footprint | Plan |
| --- | --- | --- | --- | --- |
| `separate --tier fast` (CLI, real htdemucs) | 20.9 min | 2.10 GB | 3.28 GB | spilled, lean |
| tiny generated 4-stem model (`long_input` test) | 53 s | 2.6–3.6 GB | — | spilled; peak heap 2.54 GB |
| whole-buffer pipeline before this change (estimated, not run) | — | — | > 15 GB | the song × (4 copies of the mix, 3–4 copies of every stem) |

The tiny-model heap peak is two full-length stereo `f32` buffers (2 × 1.27 GB): the test's
`SimpleIo` encodes a stem as 32-bit float while the samples are held; the CLI's 24-bit
encoder needs 0.95 GB for the bytes. For `quality` and `max` at 60 minutes the separation
phase is bounded by the RoFormer's lean session (3.7 GB measured on short inputs) plus a few
blocks; a full 60-minute `quality` run (about 4 hours on this CPU) was not timed.

`cargo test --release -p ferraus-separate --test long_input -- --ignored --nocapture` repeats
the tiny-model measurement (a counting allocator reports the peak heap); set
`FERRAUS_LONG_MINUTES` for a shorter input. Temporary disk use for a spilled 60-minute run is
about 1.3 GB per stem estimate plus 1.3 GB for the mix (6.4 GB for `fast`).

Not yet measured: GPU timings.
