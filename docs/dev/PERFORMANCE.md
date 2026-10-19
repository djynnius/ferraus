# Performance against the PRD hardware tiers

Measured 2026-10-19 on an Apple Silicon laptop (10 cores) with a `--release` build, CPU only,
on a 60-second 44.1 kHz stereo synthetic mix; 4-minute figures are extrapolated linearly.
`ferraus bench` reproduces the per-effect real-time factors on any machine.

| Command | 60 s input | 4-minute song (extrapolated) | PRD target |
| --- | --- | --- | --- |
| `fx` (compress, eq, reverb, limit) | 0.81 s | ~3.2 s | under 5 s |
| `denoise --auto` (profile gating) | 0.88 s | ~3.5 s | under 5 s |
| `tune` (pYIN + TD-PSOLA) | 0.71 s | ~2.8 s | under 60 s |
| `separate --tier fast` (htdemucs ONNX) | — | 47 s (measured, see below) | under 2 min (fast), under 4 min (4-stem Demucs class) |

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

Measured 2026-10-19 on an M1 Pro laptop (8 performance + 2 efficiency cores, 16 GB), CPU only,
`--release` build, with a 4-minute 44.1 kHz stereo song made of real music (the 50 MUSDB18
7-second test previews back to back, CC BY 4.0), `/usr/bin/time -l`, default
`--max-memory 2GB` unless noted. "Before" is the build this work started from (`59039ae`)
measured the same way on the same machine and input, back to back; "peak" is the macOS peak
memory footprint.

| Run | Before | After | Peak before | Peak after | Plan after (`result.memory`) | PRD target |
| --- | --- | --- | --- | --- | --- | --- |
| `--tier fast` | 90 s | 47 s | 2.2 GB | 1.9 GB | in memory, lean sessions | under 2 min |
| `--tier balanced` | 351 s | 182 s | 2.0 GB | 1.9 GB | in memory, lean | under 4 min |
| `--tier quality` | 712 s | 421 s | 3.3 GB | 3.0 GB | in memory, lean (`fits: false`: the RoFormer's own session needs 2.8 GB) | under 15 min |
| `--tier fast --stems 6` | | 43 s | | 1.5 GB | spilled, lean | |
| `--tier max` | | 2034 s | | 2.9 GB | spilled, lean, `fits: false` | |
| `--tier fast --max-memory 8GB` | | 47 s | | 5.0 GB | in memory, full sessions | |
| `--tier quality --max-memory 8GB` | | 429 s | | 5.8 GB | in memory, full sessions | |

(The previous version of this page reported 117 s, 430 s, and 937 s for the same three tiers
on a busier machine; the before column above was re-measured next to the after column.)

Where the time went, and what changed (per 7.8-second htdemucs window / 8-second RoFormer
window, `cargo run --release -p ferraus-ml --example ortbench` on the cached models):

1. **Threads.** ONNX Runtime's default intra-op thread count leaves cores idle on Apple
   silicon: htdemucs ran 1.94 s per window by default, 1.59 s on 8 threads, 1.55 s on all 10;
   the Mel-Band RoFormer 10.0 s, 6.6 s, and 6.0–6.5 s. Sessions now use every core on Apple
   silicon (`ferraus_ml::session::default_threads`); elsewhere ONNX Runtime's default (the
   physical cores) is kept. `--threads` and `--deterministic` still override it.
2. **STFT on the host.** The registry's htdemucs exports compute the spectrogram branch's
   STFT as a 4096-tap convolution with an fp16 DFT basis and the iSTFT as a transposed
   convolution plus an overlap-add unrolled into about 16 000 nodes (680 `ScatterND` that
   rebuild the window envelope in `f64` on every call). An ONNX Runtime profile put 27% of
   each window there (0.42 s of 1.55 s) and most of the 6.5 s session load. The registry now
   names the tensors at the edges of those subgraphs (`io.params.host_stft`); ferraus cuts the
   graph there at load time (`ferraus_ml::onnx_edit`, protobuf-level, no ONNX library) and
   runs the same transforms with an FFT (`ferraus_separate::host_stft`). htdemucs: 1.05 s per
   window, 0.5 s load. The stems match the uncut graph to 67–91 dB SI-SDR (the graph's fp16
   basis is the difference) and the MUSDB18 scores below are unchanged.
3. **Memory arena and lean sessions.** `ort`'s CPU provider turns ONNX Runtime's memory arena
   off unless asked; with it on, a lean htdemucs window peaks at 1.4 GB instead of 1.9 GB at the
   same speed. Lean sessions also stop at `ORT_ENABLE_EXTENDED` (the layout-level rewrites of
   `ORT_ENABLE_ALL` cost memory and no speed here): the RoFormer's lean session needs 2.8 GB
   instead of 3.5–3.7 GB and runs faster than its full session (6.0 s against 7.1–7.6 s per
   window). With the cut graph, full sessions (constant folding, memory pattern, prepacking)
   are no faster than lean ones on this CPU, only larger (4.6 GB for htdemucs).

Time per tier is now models × windows × time per window: 42 htdemucs windows for 4 minutes
(about 44 s per model plus 0.5 s load) and 47 RoFormer windows (about 6 s each). `balanced`
is four fine-tuned htdemucs passes (182 s); `quality` is the RoFormer (about 290 s) plus three
of them. In an htdemucs window the cross-domain transformer is now about half the time, the
decoders 28%, the encoders 15%; the RoFormer is 83% matrix multiplication
(MLAS `MatMul`/`Gemm`). The Rust side (resampling, overlap-add, host STFT, ensembling, sum to
mix) is under 5% of the run.

Tried and not adopted:

* **CoreML** (`--device coreml`): htdemucs fails to compile in CoreML's NeuralNetwork format,
  and in ML Program format crashes the GPU command buffer or the CPU compiler after 27–67 s of
  compilation; the RoFormer compiles in 103 s and then runs 7–8 s per window on CPU only, no
  faster than ONNX Runtime. The registry keeps CoreML out of these models' provider hints.
* **Parallel execution mode** (`ORT_PARALLEL`, 2–4 inter-op threads): 1.72–2.03 s per htdemucs
  window, slower than sequential.
* **Two windows at once** (two sessions of 5 threads): 0.85 s per htdemucs window instead of
  1.05 s, but 2.4 GB, over the default budget. A possible `--max-memory 4GB` mode.
* **Constant folding / a pre-optimized model saved to disk**: loads in 0.25 s but runs
  1.36–1.40 s per window at 2.8–4.6 GB (folding materializes the window envelope chain), so the
  host STFT is both faster and smaller.
* **Lower `--overlap` or fewer windows**: not done; it would change the output (the 7-second
  quality test cannot see it, so it would not show up in the SI-SDR check either).

**Quality check.** Median SI-SDR (dB) on the first 12 MUSDB18 7-second test excerpts
(`--max-memory 8GB`, the method of [Stems and separation tiers](../guides/concepts/stems.md)),
before and after:

| Tier | Vocals | Drums | Bass | Other |
| --- | --- | --- | --- | --- |
| `fast` | 8.56 → 8.56 | 7.86 → 7.86 | 7.79 → 7.79 | 3.31 → 3.31 |
| `balanced` | 9.01 → 9.01 | 7.66 → 7.66 | 8.11 → 8.11 | 3.34 → 3.34 |
| `quality` | 11.88 → 11.88 | 7.66 → 7.66 | 8.11 → 8.11 | 4.76 → 4.76 |

The largest change in any median is 0.001 dB (`balanced` other, 3.338 → 3.337).

**60-minute input.** The input now streams from the decoder into its track (in memory, or a
spill file when the plan spills) and every stem streams from its track into the encoder
(`AudioIo::open_reader` / `open_writer`, implemented by `ferraus-codec` for every format it
decodes natively and for every native encoder that needs no resampling; inputs that need
FFmpeg and outputs such as Opus at 44.1 kHz or FFmpeg-encoded `.m4a` fall back to whole
buffers). The files are
byte-identical to the whole-buffer path (`ferraus-codec/tests/stream_io.rs`,
`streamed_input_and_stems_are_identical_to_whole_buffer_io`). Nothing in a separation run
holds the song any more: peak memory is the model plus a few blocks.

| 60-minute 44.1 kHz stereo input (real music: the 50 MUSDB18 test previews looped), default `--max-memory 2GB` | Time | Peak RSS | Peak footprint | Plan |
| --- | --- | --- | --- | --- |
| `--tier fast`, before (previous build, from the earlier version of this page) | 20.9 min | 2.10 GB | 3.28 GB | spilled, lean |
| `--tier fast` | 11.5 min | 1.56 GB | 1.54 GB | spilled, lean |
| `--tier balanced` | 43.4 min | 1.59 GB | 1.56 GB | spilled, lean |
| `--tier quality` | 105.8 min | 2.83 GB | 2.82 GB | spilled, lean, `fits: false` (the RoFormer's session) |
| tiny generated 4-stem model (`long_input` test), peak heap | 42 s | — | — | spilled; 140 MB (before: 2.54 GB) |

This is the PRD acceptance criterion "a 60-minute input runs with peak memory under 4 GB on
CPU in every tier", measured for `fast`, `balanced`, and `quality`: all three stay under 4 GB,
and `fast` and `balanced` stay under the default 2 GB budget. `max` runs the same models (one
at a time, the RoFormer being the largest), so its peak is the RoFormer's, as in `quality`;
a 60-minute `max` run (about 8.5 hours on this CPU, 15 times the 4-minute run) was not timed. A spilled 60-minute `quality`
run used 6.4 GB of temporary disk (the mix and four stem estimates as `f32`).

`cargo test --release -p ferraus-separate --test long_input -- --ignored --nocapture` repeats
the tiny-model measurement (a counting allocator reports the peak heap); set
`FERRAUS_LONG_MINUTES` for a shorter input. Temporary disk use for a spilled 60-minute run is
about 1.3 GB per stem estimate plus 1.3 GB for the mix (6.4 GB for `fast`).

Not yet measured: GPU timings; x86 laptops (the thread default, the lean sessions'
`ORT_ENABLE_EXTENDED` level, and full against lean sessions were only measured on Apple
silicon).
