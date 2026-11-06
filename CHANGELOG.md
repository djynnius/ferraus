# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/)
and the project uses [Semantic Versioning](https://semver.org/) for its CLI contract: a breaking
change to any `--json` schema or exit code is a major version.

## [Unreleased]

### Added
- `analyze sections <mix>`: song structure without a model. Sections on the bar grid
  (downbeats, `--bpm`), labels for repeats (A, B, A, B, C, B), a best-effort role per section
  (intro, verse, pre-chorus, chorus, bridge, instrumental, outro), loudness per section, and
  `repeats` with `offset_s` (where the same bar sits in the twin section). `--vocal` adds the
  vocal activity per section and firmer roles. Boundaries came out exact (within 10 ms) on
  songs rendered with ferraus, including one without drums.
- `ride <vocal> --against <music>`: a vocal rider. It turns the vocal up and down word by
  word so it holds a steady level against the music (the volume automation engineers draw
  before the compressor), and returns the ride as `envelope` keyframes (`result.points`) that
  reproduce it exactly. On the mix template's test vocal the vocal-minus-music spread went
  from 4.9 to 2.5 LU.
- `deresonate`: a dynamic resonance suppressor. It turns down narrow spectral peaks only while
  they stand out above their neighbourhood (a vocal's low-mid build-up that moves with the
  notes, a guitar's ring), reports the bands it worked on in `result.top`, and leaves the
  input bit for bit where nothing is reduced.
- `patch`: replace a time range with audio from elsewhere in the file (a clean repeat of the
  same part) or from another take (`--source`), keeping the length, with crossfades at both
  edges.
- Companion skills, from a study of a full professional mixing session:
  - `ferraus-mix`: a song map from `analyze sections`, picking the anchor per song, printed
    effects and bounced stacks treated as fixed, a vocal rider stage in the pipeline template,
    section contrast, Haas widening and "handoff" panning, solo instruments, bus tape and
    console color with its measured cost, a 10-12 kHz shelf instead of 7.5 kHz for a dull
    top, several references, and a level-matched A/B against the rough mix.
  - `ferraus-vocals`: rides before compression, `deresonate` for moving build-ups, separate
    verse and chorus chains switched at the section boundaries, throws on phrase ends.
  - `ferraus-stems`: an editing pass before the mix (`patch` from a clean repeat, `repair`,
    `remove-sound`).
- MCP prompt `mix-stems` covers the vocal ride and patching from a clean repeat.

### Fixed
- `replay` of a command that reads a side file (`compress --sidechain`, `ride --against`,
  `patch --source`) passed the side file as a second input and failed; it is now passed back
  as its flag.
- Pipeline stages keep the result an effect returns (`ride`'s keyframes, `deresonate`'s
  `top`) in `result.plan[]`.

## [0.1.3] — 2026-11-05

### Added
- `mix balance <stems...>`: measures every stem (BS.1770 gated, in the sum's channel layout)
  and proposes the gains that put a multitrack on a genre balance template (pop, hiphop, edm,
  rock, acoustic, jazz), with roles from `--roles` or the file names, a `mix_command`, and
  `--render` to measure the balanced sum.
- `analyze masking <a> <b>`: where stem b crowds stem a in the same third-octave bands while
  both play, with a runnable EQ cut for b per region. Level-aware by default (a band counts
  only while b is within 10 dB of a there, so cuts and fader moves lower the score and the cut
  suggested is the one that clears it); `--levels ignore` compares spectral shapes only.
- `mix verify <mix>`: the pass/fail gate before mastering (headroom, loudness window, PLR,
  clipping, DC, phase, mono bass, mono loss, tonal balance against the genre curve or a
  reference, mud, harshness, sub rumble, head silence, problems), exit code 9 with a runnable
  `fix` per failed check.
- `compress --sidechain <file>`: external key input for ducking (bass under the kick, a music
  bed under a voice-over).
- `master --relevel` (with `--plan`): searches the final gain and limiter again so an edited
  plan still lands on its target.
- `pipeline prune`: removes cached stage results not used for `--days` days (a cache hit
  counts as use), then the least recently used ones until the cache fits `--max-size`;
  `--dry-run` lists them. A mixing session writes hundreds of megabytes per run.
- MCP prompt `mix-stems`: the measured mixing workflow for clients without skills;
  `master-song` now covers already-limited mixes, plan review, `relevel`, and the
  loudness-matched A/B check.
- Pipeline files: `$name` / `$stage` / `$stage.role` in a stage argument reads a pipeline
  input or another stage's output (sidechain keys, references), ordered and cached by content
  like `in:`. Stages report their measurements in `result.plan[].result` (`metrics`, and
  `chain_metrics` per element of an `fx` chain), and cached stages return the saved result.
- Companion skills for agents: `ferraus-mix` (stems to a mix ready for mastering),
  `ferraus-master`, `ferraus-vocals`, `ferraus-mix-review`, `ferraus-stems`, `ferraus-beats`,
  each with measured decisions, pass/fail gates, references, and `claude plugin eval` cases
  whose inputs `tools/eval/make_session.py` generates. Contract tests check every command and
  pipeline in the skills against the CLI.
- Cookbook: [stems to master](docs/guides/cookbook/stems-to-master.md).
- MCP directory listings: `server.json` for the official MCP Registry
  (`io.github.djynnius/ferraus`, the container image run as `mcp serve --allow /work`) and
  `glama.json`; the container images carry the `io.modelcontextprotocol.server.name` label the
  registry checks.

### Changed
- The global `--bpm` replaces the detected tempo in `master`, `master plan`, and
  `master analyze` (multiband and limiter release times follow it).
- `pipeline validate` (and every run) checks each item of an `fx` chain: the effect exists,
  can be a chain element, and its arguments parse.

## [0.1.2] — 2026-10-20

### Added
- Signed apt (Debian 13+, Ubuntu 24.04+) and dnf (Fedora 39+, RHEL) repositories at
  <https://djynnius.github.io/packages/>, a scoop bucket (`djynnius/scoop-bucket`), and container
  images `ghcr.io/djynnius/ferraus` (x86-64 and ARM64) and `ghcr.io/djynnius/ferraus:latest-cuda`
  (NVIDIA GPUs). See Getting started for the commands.
- Release builds for Intel Macs (`x86_64-apple-darwin`, macOS 13.4+), Windows on ARM64
  (`aarch64-pc-windows-msvc`, zip and MSI; no longer the x64 build under emulation), and fully
  static musl Linux on x86-64 and ARM64 (`*-unknown-linux-musl`) for Alpine and for distributions
  whose glibc is older than the glibc build needs; the shell installer picks the musl build on
  those systems. ONNX Runtime 1.31 is built from source in CI for the Intel Mac and musl targets,
  for which ort ships no prebuilt library. The static musl build cannot load audio plugins
  (CLAP/VST3), so `ferraus-plughost-vst3` has no musl archive.

### Changed
- `separate --tier auto` (the default) picks `quality` again (Mel-Band RoFormer vocals): the
  maintainer accepted the weights' MIT license (ADR 0008). Use `--tier balanced` for the
  faster Demucs-only separation.
- `ferraus-plughost-vst3` is licensed Apache-2.0 OR MIT (was GPL-3.0-only): Steinberg released
  the VST3 SDK under MIT with version 3.8 (ADR 0008).

### Fixed
- `score read` works on Linux and Windows x86-64: ONNX Runtime's x86 layout optimizations
  rejected the oemer model (a transposed convolution with negative pads), so every page failed
  with `processing_failed`. Sessions that fail shape inference now load at the extended
  optimization level.
- The `.deb`/`.rpm` packages and the container image find the CLAP plugin host in
  `/usr/lib/ferraus/` (`/usr/local/lib/ferraus/` in the image); it was looked for only next to
  `ferraus` and on `PATH`, so `plugin run` failed there and `doctor` reported no host.
- `speed` is about 3x faster (polyphase resampling), above the 50x real-time floor on CI runners.

## [0.1.1] — 2026-10-20

### Fixed
- The `ferraus` crate is publishable: `ferraus docs troubleshooting` embeds a copy of the page
  that lives inside the crate (0.1.0 reached `docs/`, outside the package, so it never reached
  crates.io).

### Changed
- Banner on the README and the docs site home page, the wordmark on Getting started, and the
  logo as the docs site favicon.

## [0.1.0] — 2026-10-20

First implementation of the PRD, milestones M0–M9 (see `docs/decisions/` for decisions taken
along the way).

### Added
- Core: planar f32 `AudioBuffer`, the shared unit parser, op specs from which the CLI, `fx`
  stages, pipeline stages, JSON Schemas, MCP tools, and docs are generated; the executor with
  batches, dry runs, `--explain`, idempotent outputs, provenance sidecars, `replay`, `verify`.
- Editing: info, convert (streaming), trim, cut, split, concat, resample, gain, normalize
  (EBU R128 with true-peak limiting), channels, silence detect/trim/pad, tags, mix, waveform,
  spectrogram; Symphonia decoding including audio tracks of MP4/MKV/WebM/MOV; WAV, AIFF, FLAC,
  Vorbis, Opus, MP3 (runtime LAME) encoders; the FFmpeg bridge (`ffmpeg`, `ffprobe`, `media *`,
  `doctor --install-ffmpeg`).
- Effects: 31 effects (fades, dynamics, EQ, reverb, delay, modulation, saturation, vocoder,
  pan and automation) and 28 Audition-parity restoration and utility commands including
  `dereverb`.
- Noise reduction: profile spectral gating, adaptive mode, RNNoise and DeepFilterNet 3.
- Separation: ONNX Runtime with execution-provider fallback, a signed model registry with
  resumable verified downloads; tiers `fast`, `balanced` (fine-tuned Demucs), `quality`
  (Mel-Band RoFormer vocals; held back from `auto` until its license review, so `auto` picks
  `balanced`), `max`, `--stems 6`, `--drum-split` (DrumSep); `--vocal-split` (lead and
  backing) with a built-in melody-guided harmonic mask (no model; best effort: on synthetic lead
  + harmony mixes the lead's SI-SDR rose about 1 dB on mono input, more on stereo input where
  panning helps) and the UVR karaoke model, which has no stated license, as an opt-in
  `--cascade-models` override;
  ensembles, TTA, sum-to-mix, cascades; streaming separation with temp-file spill under
  `--max-memory` and block-by-block decode and encode (60-minute song: 1.6 GB on `fast` and
  `balanced`, 2.8 GB on `quality`); htdemucs STFT on the host and all cores on Apple silicon
  (4-minute song on a laptop CPU: fast 47 s, balanced 3.0 min, quality 7.0 min).
- Pitch and time: pYIN, CREPE, SwiftF0, key detection, tune, pitch, stretch.
- MIDI: lossless JSON model, quantize, groove, editing utilities, transcribe (mono and Basic
  Pitch polyphonic), hum, render through the built-in synth, SF2, SFZ, or CLAP/VST3 plugins.
- Scores: lead sheets, grand staff, drum scores, solfa; MusicXML/ABC/LilyPond/MEI writers and
  readers; score play and perform; Verovio engraving helper; Audiveris bridge; builtin
  optical music recognition (`score read` on PNG/JPG/PDF) on oemer's MIT segmentation networks,
  100% pitch-plus-duration on the engraved test set (`tests/omr_accuracy.rs`).
- Plugins: out-of-process CLAP and VST3 hosting with a watchdog, sounds registry.
- Mastering: master, master analyze/plan/verify/deliver.
- Agents: JSON envelope and schemas, MCP server (stdio and HTTP) with jobs, resources, and
  prompts, `describe --agent`, AGENTS.md and a Claude skill.
- Examples: `examples/` holds runnable shell scripts, pipeline files, an MCP client, and agent
  scripts that work on test audio generated by ferraus itself; CI runs them all
  (`examples/run-all.sh`).
- `pipeline run --dry-run` prints the resolved plan and the output paths without computing
  anything (it used to run each stage as a dry run, which failed at the first stage that read
  an earlier stage's audio and could leave empty entries in the stage cache).
- Release engineering: cargo-dist workflow (shell/PowerShell installers, Homebrew, MSI),
  `.deb`/`.rpm` jobs, containers, publish job behind an approval gate, docs site from op specs.
