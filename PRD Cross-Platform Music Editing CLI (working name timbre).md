# PRD: ferraus, a Cross-Platform Music Editing CLI

Oct 18, 2026 · @Sunday

## Summary and vision

**ferraus** (the au is for audio) is one cross-platform binary that gives people and AI agents a complete music-editing toolkit from the command line. It covers the FFmpeg class of jobs (convert, trim, concatenate, resample, normalize), studio effects, Audition-class restoration filters, panning and automation, profile-based noise removal, stem separation aimed at better-than-Demucs quality, pitch correction, MIDI quantization, hum-to-MIDI, notation in both directions (audio to a drum score, lead sheet, grand staff, or solfa, and a sheet-music PDF to a performed song), CLAP/VST3 instrument hosting with switchable sound folders, and one-command or agent-driven mastering.

The product bet is that audio tooling today is split between heavy DAWs built for mouse-driven humans and scattered research repos that need a Python environment per feature. Nothing offers the whole chain as a single installable tool that an agent can discover, call, and chain without a GUI. ferraus fills that gap.

Two audiences, one interface:

- **People**: musicians, podcasters, producers, and developers who want `ferraus separate song.mp3` to work on a laptop with sensible defaults and no setup beyond `brew install`, `apt install`, or a Windows installer.
- **AI agents**: every command accepts and returns structured JSON, publishes its own schema, writes a provenance sidecar next to each output, and is also exposed as an MCP server so Claude, Claude Code, and other agents can call ferraus as native tools.

What ships: a Rust CLI plus library crates, pure-Rust DSP for effects and editing, and an ONNX-based model runtime for the ML features (separation, denoising, pitch tracking, transcription). Model weights download on first use and are cached; the binary itself stays small.

Success looks like this: an agent can take a phone recording of someone humming over street noise, clean it, extract the melody to MIDI, quantize it to the beat, render it with an installed instrument, tune a recorded vocal to the same key, and master the mix to streaming loudness, using only ferraus commands, with no human in the loop and every step reproducible from the sidecar files.

## Goals, non-goals, and target users

The v1 goal is one installable binary per platform that performs every capability below with sensible defaults, and that an agent can drive end to end through JSON or MCP.

**Goals (v1)**

- Editing and conversion for every common audio format, including extracting audio tracks from video containers.
- Effects with musically sensible defaults and full parameter control: fades, dynamics, EQ, reverb, delay, modulation, vocoder, saturation.
- Noise reduction from a user-selected noise profile, plus model-based denoising when no clean noise sample exists.
- Stem separation at 2, 4, and 6 stems with quality at or above Demucs v4 fine-tuned, with an ensemble mode that targets the best open models.
- Pitch correction that snaps a vocal to a key without audible artifacts, plus pitch shift and time stretch.
- MIDI quantization, hum-to-MIDI transcription, and rendering MIDI through soundfonts or hosted instruments.
- CLAP and VST3 instrument and effect hosting, with a registry of installable free instruments and sound libraries.
- Agent-first interface: JSON in and out, self-published schemas, provenance sidecars, and an MCP server mode.
- Packages for Debian/Ubuntu (.deb), Fedora/RHEL (.rpm), macOS (Homebrew), and Windows (installer plus winget).
- Restoration and utility filters matching Adobe Audition's effects reference (declip, dehum, declick, dehiss, spectral repair, phase alignment, leveler, scientific filters, envelopes), plus panning and pan automation.
- Mastering: a one-command master to platform loudness targets, and the analyze, plan, and verify commands an AI model needs to master a song on its own.
- Instrument switching from the user's own sound folders (`--sound-path`), so a MIDI part renders as piano, strings, ensemble, or guitar by name.
- Notation both ways: a drum score, a lead sheet or grand-staff piano score with guitar chord symbols, or solfa-only text from audio or MIDI; and a sheet-music PDF read into MIDI first, then performed as audio.
- One-URL installation on every platform (`curl ... | sh`, a PowerShell one-liner) alongside `.deb`, `.rpm`, Homebrew, winget, and MSI packages.
- Every FFmpeg capability kept reachable through a first-class bridge, so version 2 can add native video commands without re-architecting.
- Documentation generated from the command definitions and tested in CI, written in full depth for both AI models and people.

**Non-goals (v1)**

- A graphical editor or DAW-style timeline. ferraus is a batch and pipeline tool.
- Real-time monitoring or live performance use. All processing is offline; low-latency live paths are a later phase.
- Training models. ferraus runs exported weights; training and export scripts live in a companion repo.
- Native video commands. v1 limits ferraus commands to audio, but the FFmpeg bridge keeps every FFmpeg function reachable today and the architecture reserves the v2 video path (see FFmpeg retention and the v2 video path).
- Hosting plugin GUIs. Plugins run headless with parameters set from the command line or presets.
- Mobile platforms.

**Target users**

| User | What they want | What they need from ferraus |
| --- | --- | --- |
| AI agents (Claude, Claude Code, custom agents) | Reliable tools they can discover and chain without a human | Stable JSON contracts, schemas, deterministic output, non-interactive runs, MCP server |
| Musicians and vocalists | Clean a take, tune a vocal, pull stems from a reference track | One-line commands with good defaults, quality on par with paid tools |
| Podcasters and video creators | Remove room noise, level loudness, trim and convert | Profile-based denoise, LUFS normalize, batch processing |
| Developers and researchers | Embed audio processing in apps and scripts | Library crates, pipeline files, stable exit codes, no Python dependency |
| Hobbyist producers | Turn a hummed idea into a playable MIDI part with a real instrument sound | Hum-to-MIDI, quantize, installable instruments, render |

## Design principles

Every design decision in this PRD follows from one rule: a command that works for an agent with no human present must also feel natural to a person at a terminal. Where the two conflict, the agent contract wins on output format and the human wins on defaults.

1. **Agent-first contracts.** Every command has a `--json` mode with a stable, versioned schema; logs and progress go to stderr; results go to stdout. `ferraus schema <command>` prints the JSON Schema for its arguments and its result, so an agent can discover the tool without documentation.
2. **Defaults that sound right.** Every effect works with zero parameters and produces a result a producer would accept as a starting point. Parameters exist to refine, never to make the command work.
3. **Non-interactive by default.** ferraus never prompts, never opens a GUI, and never blocks on a model download without `--yes` or a config opt-in. Missing models fail fast with a machine-readable error that names the exact `ferraus models pull` command.
4. **Deterministic and reproducible.** Same input, same parameters, same model version, same output, bit-for-bit on CPU. Every output can carry a sidecar JSON recording the full command, parameter values after defaults, model hashes, and ferraus version.
5. **Offline-capable.** After models are cached, no network access is needed. `--offline` turns any network attempt into an error.
6. **Pure Rust where it is good enough, native bindings where quality demands it.** Editing, effects, spectral denoising, pitch detection, and MIDI are pure Rust. ML inference uses ONNX Runtime. Plugin hosting uses the CLAP and VST3 C ABIs behind a subprocess boundary.
7. **Composable.** Commands chain in one invocation or in a pipeline file, and every command reads from a file, stdin, or the previous stage and writes to a file or stdout.
8. **Small binary, big models on demand.** The CLI stays under roughly 60 MB per platform. Model weights (tens of MB to about 1 GB each) download on first use into a per-user cache with checksums.
9. **Honest about quality.** Features whose open models lag paid tools (strings, piano, guitar stems; polyphonic transcription) ship labeled as best-effort tiers and report a confidence or quality estimate in their JSON result.
10. **Safe by construction.** Inputs are untrusted. Decoders are fuzzed, plugins run in a separate process, and no command ever writes outside the paths given to it.
11. **Built in the open, on GitHub.** Release binaries, installers, and packages are built only by the GitHub Actions workflows in the project repository, never on a developer's or an agent's device, so every artifact has verifiable provenance and no signing key ever leaves CI.

## Platforms, distribution, and hardware tiers

ferraus ships as a native binary for six targets, through the package manager each platform's users expect, with CPU inference everywhere and GPU acceleration as an optional add-on.

| Platform | Targets | Install channel | GPU acceleration (optional) |
| --- | --- | --- | --- |
| Linux (Debian, Ubuntu) | x86\_64, aarch64 | `.deb` from GitHub Releases and an apt repo; shell installer | CUDA via ONNX Runtime CUDA provider |
| Linux (Fedora, RHEL, openSUSE) | x86\_64, aarch64 | `.rpm` from GitHub Releases and a yum/dnf repo | CUDA |
| macOS 13+ | Apple Silicon, Intel | Homebrew tap (`brew install djynnius/tap/ferraus`), later core formula; signed and notarized | CoreML / Metal via ONNX Runtime CoreML provider |
| Windows 10+ | x86\_64, arm64 | `winget`, `scoop`, MSI installer, PowerShell installer | DirectML (any GPU) or CUDA (NVIDIA) |
| Containers | linux/amd64, linux/arm64 | `ghcr.io/djynnius/ferraus` CPU image and a CUDA image | CUDA |
| From source | any Rust tier-1 target | `cargo install ferraus-cli` | per features flags |

**One-line installs** (GitHub org `djynnius`; every command below installs the same signed release and never needs sudo)

| Platform | Command |
| --- | --- |
| macOS and Linux | `curl -fsSL https://github.com/djynnius/ferraus/releases/latest/download/ferraus-installer.sh \| sh` |
| Short alias from the repo | `curl -fsSL https://raw.githubusercontent.com/djynnius/ferraus/main/install.sh \| sh` (a shim that fetches the installer above; GitHub does not serve raw files at `github.com/djynnius/ferraus/install.sh`, so a vanity domain redirect can be added later) |
| Windows PowerShell | `powershell -ExecutionPolicy ByPass -c "irm https://github.com/djynnius/ferraus/releases/latest/download/ferraus-installer.ps1 \| iex"` |
| Homebrew | `brew install djynnius/tap/ferraus` |
| Debian and Ubuntu | `sudo apt install ./ferraus_<version>_amd64.deb`, or add the apt repo and `apt install ferraus` |
| Fedora and RHEL | `sudo dnf install ./ferraus-<version>.x86_64.rpm`, or add the dnf repo and `dnf install ferraus` |
| Windows package managers | `winget install djynnius.ferraus` or `scoop install ferraus` (MSI available on the release page) |
| From source | `cargo install ferraus-cli` |

**Hardware tiers.** Every feature must run on CPU-only machines; the separation and transcription models are the only ones where CPU time is a real cost. Targets on a 2023-class 8-core laptop CPU for a 4-minute stereo song:

- Editing, effects, spectral denoise, normalize: under 5 seconds each.
- Model denoise (DeepFilterNet-class): under 30 seconds.
- Pitch tracking and auto-tune: under 60 seconds.
- 4-stem separation, `fast` tier (Demucs class): under 4 minutes.
- 4-stem separation, `quality` tier (RoFormer class): under 15 minutes on CPU, under 1 minute on a mid-range GPU.

**GPU add-on.** The base package is CPU-only to keep installs small and dependency-free. `ferraus accelerate install` downloads the matching ONNX Runtime execution-provider libraries (CUDA, CoreML is built in on macOS, DirectML is built in on Windows) into the user cache, verifies them, and `ferraus doctor` reports which provider is active. `--device auto|cpu|cuda|coreml|directml` overrides per run.

**Model download policy.** Packages contain no model weights. The first command that needs a model prints the download size and fetches it unless `--offline` is set or the config disables auto-download, in which case the error names the `ferraus models pull` command to run.

## Architecture

ferraus is a Cargo workspace of focused library crates behind one CLI binary: DSP and editing are pure Rust, ML inference goes through ONNX Runtime, and third-party plugins run in a separate host process so a plugin crash never takes down a job.

&#91;embedded content: architecture · 2 callers, 1 CLI, 6 library crates, 2 runtimes, 1 isolated host process\]

Both kinds of caller reach the same command tree; every feature is a library crate under `ferraus-core`, models run through `ferraus-ml`, and only `ferraus-plughost` runs outside the main process.

**Workspace crates**

| Crate | Responsibility | Key dependencies |
| --- | --- | --- |
| `ferraus-cli` (binary) | Command tree, argument parsing, JSON and human output, progress, config, job runner, MCP entry point | `clap` (derive), `serde_json`, `schemars`, `tracing`, `indicatif` |
| `ferraus-core` | `AudioBuffer`, units and parsing, `Processor` traits, pipeline graph and executor, error types, provenance sidecar | `dasp`, `thiserror`, `rayon` |
| `ferraus-codec` | Decode WAV, FLAC, MP3, AAC, Vorbis, Opus, ALAC, and audio tracks inside MP4/MKV/WebM; encode WAV, FLAC, Vorbis, Opus, MP3; optional FFmpeg bridge for anything else | `symphonia`, `hound`, `flacenc`, `opus`, `vorbis_rs`, `mp3lame-encoder` (feature-gated) |
| `ferraus-dsp` | Biquad filters and parametric EQ, dynamics, fades, resampling, algorithmic and convolution reverb, delay, modulation, saturation, channel vocoder, STFT utilities, loudness, restoration (declip, dehum, declick, dehiss, spectral repair), panning and envelopes | `rustfft`/`realfft`, `rubato`, `ebur128` |
| `ferraus-pitch` | pYIN pitch tracker (pure Rust), ML tracker adapters, note segmentation, key detection, TD-PSOLA and phase-vocoder shifting, time stretch | `ferraus-ml`, `ssstretch` or a pure-Rust stretcher (decision in Open questions) |
| `ferraus-ml` | Model runtime abstraction: session cache, execution-provider selection with CPU fallback, chunking and overlap-add, resampling to model rate, deterministic mode | `ort` (ONNX Runtime), `ndarray` |
| `ferraus-separate` | Stem separation pipelines, model tiers, ensembles, test-time augmentation, residual reassignment, cascaded vocal splitting | `ferraus-ml` |
| `ferraus-denoise` | Noise profile format, spectral gating engine, DeepFilterNet adapter | `ferraus-dsp`, `deep_filter` (feature-gated) |
| `ferraus-midi` | MIDI file I/O, JSON representation, quantize, groove extraction, transpose, velocity ops, tempo maps | `midly` |
| `ferraus-synth` | SF2 and SFZ rendering, built-in preview synth | `oxisynth`, `sfizz` via FFI (feature-gated) |
| `ferraus-plugins` | CLAP and VST3 scanning, out-of-process host protocol, parameter and preset model, sounds registry | `clack` (CLAP host), VST3 bindings isolated in the host binary |
| `ferraus-plughost` (binary) | The plugin host subprocess: loads one plugin, streams audio and MIDI over shared memory and a framed pipe protocol | `clack`, VST3 SDK bindings |
| `ferraus-models` | Model registry manifest, download with resume, SHA-256 verification, cache layout, license display | `reqwest` (rustls), `sha2` |
| `ferraus-mcp` | MCP server exposing every command as a tool, generated from the same clap definitions | `rmcp`, `schemars` |
| ferraus-score | Notation model, MusicXML/MEI/ABC/LilyPond/solfa writers and readers, drum map, chord symbols and guitar diagrams, score JSON, OMR adapters, score-to-MIDI performance | ferraus-ml, pdfium-render; Verovio in the ferraus-engrave helper |
| ferraus-media | FFmpeg bridge (managed binary, pass-through, media commands), container stream inventory, MediaBuffer types reserved for v2 video | ferraus-codec, managed FFmpeg binary |

**Processing model**

- `AudioBuffer` is planar `f32` with a sample rate and channel layout; `f32` is the internal format for every stage, and dither is applied only when writing integer formats.
- Streaming stages (EQ, dynamics, fades, gain, modulation, delay, spectral gate) implement `Processor::process_block` and report latency so the executor compensates automatically. Offline stages (separation, time stretch, auto-tune, convolution reverb on long IRs) implement `OfflineProcessor::process(&AudioBuffer)` and may spill to temp files when the input exceeds the memory budget (`--max-memory`, default 2 GB).
- A pipeline is an ordered list of stages. The executor fuses adjacent streaming stages into one pass over the audio and materializes buffers only at offline boundaries.
- Long inputs are decoded in chunks; nothing assumes the whole file fits in memory except offline stages that need global analysis (key detection, loudness measurement), which run a streaming analysis pass first.

**ML runtime**

- Every model is an ONNX file plus a `ModelSpec` (name, version, SHA-256, sample rate, expected shapes, chunk length, overlap, provider hints, license). Pre- and post-processing (STFT, band splitting) are exported inside the ONNX graph wherever the architecture allows, so `ferraus-ml` only handles resampling, chunking, and overlap-add.
- Execution providers are tried in order `cuda`, `coreml`, `directml`, `cpu` unless `--device` pins one; a provider that fails to initialize logs a warning and falls back.
- `--deterministic` disables nondeterministic kernels and fixes thread counts so CPU results are bit-exact across runs.

**Plugin isolation**

- `ferraus-cli` never loads a plugin into its own process. It spawns `ferraus-plughost`, which loads exactly one plugin, and exchanges audio through a shared-memory ring buffer and control messages through a length-prefixed JSON protocol on pipes.
- A host crash or a watchdog timeout (default 60 s with no progress) fails the job with the plugin's name and path in the error, and leaves the main process intact.
- VST3 hosting code, which depends on the GPL-licensed VST3 SDK, lives only in a separate ferraus-plughost-vst3 binary, so the CLAP host and the core crates stay permissively licensed (see Licensing).

**Configuration and paths**

- Config: `~/.config/ferraus/config.toml` (Linux), `~/Library/Application Support/ferraus/config.toml` (macOS), `%APPDATA%\\ferraus\\config.toml` (Windows). `FERRAUS_CONFIG` overrides.
- Cache (models, execution providers, plugin scan results): the platform cache directory under `ferraus/`. `FERRAUS_HOME` overrides both.
- Concurrency: `rayon` for DSP and chunk parallelism; `tokio` only in the MCP server and downloader. `--threads N` caps both.

## CLI conventions

One grammar covers every command: `ferraus <command> [subcommand] [options] <inputs...> [-o <output>]`, with audio operations as top-level verbs and domain groups (`midi`, `plugin`, `sounds`, `models`, `jobs`, `pipeline`, `mcp`) as nouns.

**Command shapes**

- Audio verbs: `ferraus convert`, `trim`, `concat`, `fade`, `normalize`, `compress`, `eq`, `reverb`, `denoise`, `separate`, `tune`, `stretch`, `pitch`, `vocoder`, and the rest of the effects. Each takes one input and writes one output (or several for `separate`).
- Domain groups: `ferraus midi quantize`, `ferraus plugin run`, `ferraus sounds install`, `ferraus models pull`, `ferraus jobs status`, `ferraus pipeline run`, `ferraus mcp serve`.
- Chains: `ferraus fx <input> -o <output> --fx "denoise:auto" --fx "fade:in=1s,out=2s" --fx "normalize:lufs=-14"` applies stages in order. Every effect is both a top-level verb and an `fx` stage, generated from the same stage definition, so parameter names never drift.
- Pipelines: `ferraus pipeline run job.yaml` executes a multi-input, multi-output graph (see Analysis and pipelines).

**Global flags** (accepted by every command)

| Flag | Meaning | Default |
| --- | --- | --- |
| `-o, --output <path>` | Output path; `-` for stdout; templates allowed (`{name}`, `{stem}`, `{index}`, `{ext}`) | `<input>.<command>.<ext>` beside the input |
| `--outdir <dir>` | Output directory for batch runs | none |
| `--format <fmt>` | Output container/codec when the extension is ambiguous or output is stdout | inferred from extension |
| `--json` | Structured result on stdout, nothing else on stdout | off |
| `--progress auto\|none\|bar\|json` | Progress on stderr; `json` emits one NDJSON event per update | `auto` (bar on a TTY, none otherwise) |
| `-q, --quiet` / `-v, -vv` | Log verbosity on stderr | warnings only |
| `--threads <n>` | CPU threads for DSP and inference | all cores |
| `--device auto\|cpu\|cuda\|coreml\|directml` | Inference device | `auto` |
| `--deterministic` | Bit-exact CPU results; implies fixed thread count | off |
| `--seed <n>` | Seed for any stochastic step (dither, humanize) | 0 |
| `--offline` | Fail instead of downloading anything | off |
| `-y, --yes` | Allow model downloads and overwrites without prompting | off |
| `--overwrite` | Replace an existing output | off (error) |
| `--in-place` | Write back to the input path, keeping `<input>.bak` | off |
| `--sidecar / --no-sidecar` | Write `<output>.ferraus.json` provenance file | on |
| `--dry-run` | Resolve parameters and print the plan without processing | off |
| `--sample-rate`, `--bit-depth`, `--channels`, `--bitrate` | Output encoding overrides | match input; MP3/Opus 192 kbps |
| `--config <path>` | Alternate config file | platform default |
| --sound-path \<dir> | Extra folder of instruments and sound libraries (CLAP, VST3, SF2, SFZ, impulse responses) to scan; repeatable; FERRAUS\_SOUND\_PATH sets the same | registry install directory plus the standard plugin directories |

**Unit parsing.** Values accept natural units and the parser is shared by every command:

- Time: `1.5s`, `1500ms`, `01:23.5`, `1:02:03.250`, `44100smp`, `4b` (beats at the detected or `--bpm` tempo), `4b@120`, negative values count from the end (`-10s`), and `start`/`end` are keywords. Ranges use `..`: `--range 0s..2s`, `--range 1:00..end`.
- Gain: `-6dB`, `+3dB`, `0.5x`, `-14LUFS` (normalize only), `-1dBTP` (true peak).
- Frequency: `120Hz`, `2.5kHz`, note names `C4`, `A#3`, MIDI numbers `midi:60`.
- Ratios and percentages: `4:1` or `4`; `80%` or `0.8`.
- Musical grid: `1/16`, `1/8T` (triplet), `1/8D` (dotted).

**Inputs and outputs**

- Inputs are positional and may be files, globs, or `-` for stdin (`--input-format` required for stdin). Several inputs with `--outdir` run as a batch with per-file results in the JSON array.
- Output format is inferred from the extension: `.wav`, `.flac`, `.mp3`, `.ogg`, `.opus`, `.m4a` (requires the FFmpeg bridge for AAC encoding), `.mid`, `.json`. `-o -` writes WAV (or `--format`) to stdout for piping into another ferraus command or any other tool.
- Video inputs (`.mp4`, `.mkv`, `.mov`, `.webm`) are demuxed for their audio track; writing a video output with replaced audio requires the FFmpeg bridge and is otherwise a clear error.
- ferraus never overwrites an existing file without `--overwrite` or `--in-place`, and never writes outside the paths it was given.

**Result envelope** (`--json`)

```json
{
  "ok": true,
  "command": "separate",
  "ferraus_version": "1.0.0",
  "schema_version": 1,
  "inputs": [{"path": "song.mp3", "duration_s": 214.3, "sample_rate": 44100, "channels": 2}],
  "outputs": [{"path": "song.vocals.wav", "role": "vocals"}, {"path": "song.drums.wav", "role": "drums"}],
  "params": {"stems": 4, "tier": "quality", "overlap": 0.25, "shifts": 2},
  "models": [{"name": "separate/bs-roformer-4s", "version": "2026.07", "sha256": "..."}],
  "metrics": {"estimated_sdr_db": {"vocals": 11.2}},
  "warnings": [],
  "timing_ms": 48210
}
```

Errors use the same envelope with `ok: false` and an `error` object carrying `code`, `message`, `hint` (a runnable command when one exists), and `exit_code`.

**Exit codes**

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Processing failed |
| 2 | Usage or parameter error |
| 3 | Input missing or unreadable |
| 4 | Unsupported format or codec |
| 5 | Model not cached and downloads disabled |
| 6 | Model download or verification failed |
| 7 | Plugin crashed, timed out, or was not found |
| 8 | Cancelled |
| 9 | Output verification failed (checksum, clipping guard, duration mismatch) |
| 10 | Network error |
| 70 | Internal error (bug); the message includes a report hint |

## Feature spec: core audio operations

The core operations cover what people reach for FFmpeg or SoX to do with audio, implemented natively so no external binary is required for any common format.

| Command | What it does | Key parameters (defaults) |
| --- | --- | --- |
| `info` | Probe a file: container, codec, duration, sample rate, channels, bit depth, bitrate, tags, loudness summary, embedded art | `--loudness` (off, runs EBU R128 analysis), `--spectrum` (off, 1/3-octave averages) |
| `convert` | Decode and re-encode between formats | `--sample-rate` (match), `--bit-depth` (match or 16 for lossy sources), `--channels` (match), `--bitrate 192k`, `--vbr` (Opus/Vorbis on), `--dither triangular` when reducing bit depth |
| `trim` | Keep a range | `--range <a>..<b>`; `--pad <dur>` to add silence; `--snap zero` crosses at zero-crossings to avoid clicks (on) |
| `cut` | Remove one or more ranges and join the rest | `--range` (repeatable), `--crossfade 10ms` |
| `split` | Split at times, at silences, or into equal chunks | `--at` (repeatable), `--every 30s`, `--on-silence` with `--silence-threshold -40dB --min-silence 500ms` |
| `concat` | Join files in order | `--crossfade 0ms`, `--gap 0ms`, `--match` resamples and remixes inputs to the first file's spec (on) |
| `resample` | Change sample rate with a high-quality sinc resampler | `--to 48000`, `--quality high\|medium\|fast` |
| `gain` | Apply fixed gain | `--by -6dB` or `0.5x` |
| `normalize` | Peak, RMS, or loudness normalization | `--lufs -14` (default mode, integrated loudness), `--peak -1dBTP`, `--rms -20dB`, `--true-peak-limit on` |
| `channels` | Mix down, split, swap, or extract channels | `--to mono\|stereo`, `--extract L\|R\|n`, `--swap`, `--mid-side encode\|decode` |
| `silence` | Detect, remove, or insert silence | `detect --threshold -40dB --min 300ms`, `trim --edges` (leading and trailing), `pad --start 1s --end 2s` |
| `tags` | Read and write metadata | `--set title=...`, `--set artist=...`, `--art cover.jpg`, `--clear` |
| `waveform` | Export a waveform as PNG, SVG, or a JSON array of peaks for UIs and agents | `--width 1200 --height 200`, `--peaks-per-second 50` |
| `spectrogram` | Export a spectrogram as PNG or a JSON matrix | `--fft 2048`, `--hop 512`, `--mel 128`, `--db-range 80` |

**Format support.** Decoding is native through Symphonia for WAV, AIFF, FLAC, MP3, AAC (in MP4/M4A), ALAC, Vorbis, Opus, and audio tracks inside MP4, MKV, WebM, and MOV. Native encoders cover WAV (8/16/24/32-bit and float), FLAC, Vorbis, Opus, and MP3 (feature-gated on the LGPL LAME encoder; builds can swap in a pure-Rust encoder at lower quality). AAC encoding, exotic containers, and video remuxing go through the optional FFmpeg bridge, which uses an FFmpeg binary found on `PATH` or installed with `ferraus doctor --install-ffmpeg`.

**Acceptance criteria**

- Round-tripping WAV through `convert` is bit-exact at the same bit depth; FLAC round trip is bit-exact.
- `normalize --lufs -14` lands within 0.3 LU of target on the EBU R128 test vectors and never exceeds the true-peak ceiling.
- `trim` and `cut` produce no audible clicks at boundaries (verified by a click detector test on tonal fixtures).
- A 3-hour 48 kHz stereo file converts with peak memory under 300 MB.
- Every command above is covered by golden-file tests with a tolerance of 1e-6 RMS difference for lossless paths.

## FFmpeg retention and the v2 video path

Every FFmpeg function stays reachable from day one through a first-class bridge, and the codec layer is built as a media backend so version 2 can add native video commands without re-architecting; v1 commands themselves stay audio-only.

**The bridge in v1**

- `ferraus doctor --install-ffmpeg` downloads a pinned, checksummed static FFmpeg build for the platform into the ferraus cache and shows its license (LGPL or GPL build, stated per download). `ffmpeg.path` in the config points at a user-installed binary instead. `ferraus-full` package variants and the container images include FFmpeg so nothing has to be fetched later.
- `ferraus ffmpeg -- <any ffmpeg arguments>` and `ferraus ffprobe -- <arguments>` pass straight through to the managed binary, so any FFmpeg capability, audio or video, is available today without ferraus knowing about it. The exact command line is recorded in the sidecar and returned in the JSON envelope.
- Common container jobs are exposed as proper commands so an agent does not need FFmpeg syntax: `media info` (ffprobe as JSON), `media extract-audio`, `media replace-audio`, `media remux`, `media streams`. These are the retained FFmpeg functions in v1 shape; `ferraus ffmpeg` covers the rest.
- Native codecs remain the default for audio; the bridge is used only when a format needs it (AAC and ALAC encoding, exotic containers, video containers) or when the person calls it directly.
- Documentation includes an "FFmpeg equivalents" table (ffmpeg command → ferraus command) so a model that already knows FFmpeg can translate, and `ferraus ffmpeg -- -h` prints FFmpeg's own help.

**Reserved for v2**

- The `ferraus video` namespace (cut, concat, transcode, overlay, subtitles, frame extraction, thumbnails, filters) and `ferraus-media` as the crate that owns demux, mux, and stream types for both audio and video.
- `MediaBuffer` in `ferraus-core` is an enum with only an `Audio` variant in v1; the pipeline executor, sidecar, and schema generator are written against that enum so adding `Video` and `Subtitle` variants is additive, not a rewrite.
- `ferraus-codec` already reports every stream in a container (video, audio, subtitle, data) from `media streams`, even though v1 only processes the audio ones.
- MCP tools, schemas, and docs for video will be generated by the same machinery as the audio commands; `schema_version` increments when the first video command ships.

**Architecture rule for the building agent:** no v1 code may assume a file holds only audio at the container level, and no command may bypass the bridge abstraction to call FFmpeg directly.

## Feature spec: effects and dynamics

Every effect runs with no parameters and gives a usable result; every parameter below is exposed with the unit shown, and `--mix` (dry/wet) exists on all of them. Effects are pure Rust in `ferraus-dsp`, run as streaming stages, and are available both as verbs and as `fx` stages.

**Fades**

| Command | Parameters (defaults) | Notes |
| --- | --- | --- |
| `fade` | `--in 0s`, `--out 0s`, `--curve linear\|log\|exp\|scurve\|equal-power` (`linear`), `--in-start start`, `--out-end end` | Fades are computed in gain, not dB, so `linear` is a straight amplitude ramp; `log` matches the ear's loudness curve and is the recommended choice for musical fades |
| `concat --crossfade` | `--crossfade 0ms`, `--crossfade-curve equal-power` | Equal-power keeps perceived loudness constant through the overlap |

**Dynamics**

| Command | Parameters (defaults) | Algorithm |
| --- | --- | --- |
| `compress` | `--threshold -18dB`, `--ratio 3:1`, `--attack 10ms`, `--release 100ms`, `--knee 6dB`, `--makeup auto`, `--lookahead 5ms`, `--detector rms\|peak` (`rms`), `--sidechain-hpf 80Hz`, `--mix 100%`, `--preset vocal\|drums\|bus\|master\|tighten` | Feed-forward log-domain compressor with soft knee; `auto` makeup restores the gain lost at threshold; `--mix` below 100% gives parallel compression |
| `limit` | `--ceiling -1dBTP`, `--release 50ms`, `--lookahead 5ms`, `--oversample 4x` | True-peak brickwall limiter with lookahead; never exceeds the ceiling after 4x oversampled measurement |
| `gate` | `--threshold -40dB`, `--attack 1ms`, `--hold 50ms`, `--release 100ms`, `--range -80dB`, `--sidechain-hpf 0Hz` | Hysteresis of 3 dB between open and close thresholds to avoid chatter |
| `expand` | `--threshold -40dB`, `--ratio 2:1`, `--attack 5ms`, `--release 100ms` | Downward expander, the gentle alternative to a gate |
| `deess` | `--center 6.5kHz`, `--bandwidth 1oct`, `--threshold -20dB`, `--range 6dB`, `--mode split\|wideband` (`split`) | Band-limited detector drives a dynamic shelf (`split`) or the whole signal (`wideband`) |
| `multiband` | `--crossovers 150Hz,3kHz`, per-band `--threshold`, `--ratio`, `--attack`, `--release` | Linkwitz-Riley 4th-order crossovers; defaults are a gentle 2:1 on each band |
| `transient` | `--attack +0dB`, `--sustain +0dB` | Attack/sustain envelope shaper for drums and plucked instruments |
| `clip` | `--ceiling -0.3dB`, `--mode soft\|hard` (`soft`), `--knee 3dB` | Clipper; the right tool when "make it louder and tighter" is the goal and a limiter sounds too smooth |

The request to "compress, clip high frequencies, and make the sound tighter" maps to the `tighten` preset: high-pass at 60 Hz, `compress --ratio 4:1 --attack 5ms --release 60ms`, `deess`, and an optional `--lowpass 16kHz`. The preset is a named stage chain, visible with `ferraus presets show compress/tighten`, and every parameter in it can be overridden.

**Equalization and filters**

| Command | Parameters (defaults) | Notes |
| --- | --- | --- |
| `eq` | `--band <type>:<freq>:<gain>[:q=<q>][:order=<n>]` repeatable; types `peak`, `lowshelf`, `highshelf`, `lowpass`, `highpass`, `bandpass`, `notch`; `--linear-phase` (off) | Cascaded biquads (RBJ cookbook); linear-phase mode uses FFT convolution with a designed FIR for mastering |
| `highpass`, `lowpass` | `--freq`, `--order 2` (12 dB/oct), `--q 0.707` | Shortcuts for the two most common filters |
| `tilt` | `--pivot 1kHz`, `--gain +0dB` | One-knob brightness/darkness control |
| `match-eq` | `--reference <file>`, `--amount 100%`, `--smoothing 1/3oct` | Fits the input's long-term spectrum to the reference; useful for agents matching takes |

**Reverb and delay**

| Command | Parameters (defaults) | Algorithm |
| --- | --- | --- |
| `reverb` | `--type plate\|room\|hall\|chamber` (`plate`), `--size 50%`, `--decay 1.8s`, `--damping 50%`, `--predelay 20ms`, `--lowcut 120Hz`, `--highcut 8kHz`, `--width 100%`, `--mix 25%` | Algorithmic: Dattorro plate for `plate`, feedback-delay-network (8 lines, Hadamard mixing) for `room`/`hall`/`chamber` |
| `reverb --ir <file>` | `--ir`, `--predelay 0ms`, `--trim-ir -60dB`, `--mix 25%` | Partitioned FFT convolution; IR sample rate is matched automatically; a small set of CC0 impulse responses is installable via `ferraus sounds install ir-pack-basic` |
| `delay` | `--time 1/4` (beat-synced when tempo is known, else `375ms`), `--feedback 35%`, `--mix 25%`, `--pingpong` (off), `--filter lowpass:6kHz`, `--bpm` | Feedback delay with filter in the loop; beat values resolve against `--bpm` or `analyze tempo` |

**Modulation and character**

| Command | Parameters (defaults) |
| --- | --- |
| `chorus` | `--rate 0.8Hz`, `--depth 3ms`, `--voices 2`, `--mix 50%` |
| `flanger` | `--rate 0.3Hz`, `--depth 2ms`, `--feedback 50%`, `--mix 50%` |
| `phaser` | `--stages 4`, `--rate 0.5Hz`, `--depth 70%`, `--feedback 30%`, `--mix 50%` |
| `tremolo` | `--rate 5Hz`, `--depth 50%`, `--shape sine\|triangle\|square` |
| `vibrato` | `--rate 5Hz`, `--depth 20cents` |
| `saturate` | `--drive 6dB`, `--type tape\|tube\|soft\|hard` (`tape`), `--tone 0`, `--mix 100%` |
| `bitcrush` | `--bits 8`, `--rate 11025Hz`, `--mix 100%` |
| `widen` | `--amount 130%`, `--mono-below 120Hz` |
| `pan` | `--position 0` (-100 left to +100 right), `--law -3dB` |
| `reverse`, `speed` (`--factor 1.0`, varispeed), `dc-remove`, `invert-phase` | utilities |

**Vocoder**

`ferraus vocoder <modulator> --carrier <source>` produces the classic robot-voice effect and is also the building block for "sing through a synth" workflows.

- `--carrier`: an audio file, a built-in oscillator (`saw`, `square`, `pulse`, `noise`) at `--note C3` or following the modulator's detected pitch (`--carrier saw --follow`), a MIDI file driving the oscillator (`--carrier saw --midi part.mid`), or an installed instrument (`--carrier plugin:"Surge XT" --preset ...`).
- `--bands 24` (8 to 64), `--range 80Hz..8kHz` on a log scale, `--attack 5ms`, `--release 30ms` envelope followers, `--formant-shift 0 semitones`, `--unvoiced noise\|off` (`noise`: sibilants detected by high-band energy ratio are re-synthesized from a noise carrier so consonants stay intelligible), `--mix 100%`.
- `--mode classic` (filter-bank channel vocoder) or `--mode spectral` (STFT-based cross-synthesis, smoother, higher band counts).

**Presets.** Every effect accepts `--preset <name>`; built-in presets ship as TOML in the binary, user presets live in `<config>/presets/<effect>/*.toml`, and `ferraus presets list|show|save` manages them. A preset is just a saved parameter set, so `--json --dry-run` prints the resolved values.

**Acceptance criteria**

- Null test: every effect at `--mix 0%` returns the input bit-exactly, and `fade --in 0s --out 0s` is the identity.
- `compress` gain reduction matches an analytic reference on step signals within 0.5 dB; `limit` never exceeds its ceiling as measured by `info --loudness` true peak.
- Reverb tail reaches -60 dB within the configured decay time ±10%.
- Every streaming effect processes at least 50x real time single-threaded on the reference laptop CPU; denormal protection is verified by a silence test that must not slow down.
- Vocoder intelligibility: a word-list fixture through the default vocoder remains intelligible in a listening check, and the `--unvoiced noise` path raises high-band energy on sibilants in a measurable test.

## Feature spec: restoration, panning, and Audition-parity filters

ferraus matches every category in Adobe Audition's effects reference with a native command, and adds the panning and envelope tools Audition keeps in its multitrack editor, so a person coming from Audition finds the same repairs and an agent can run a full restoration pass unattended.

**Coverage map** (Audition effect → ferraus command)

| Audition effect | ferraus command | Notes |
| --- | --- | --- |
| Amplify | `gain` | per-channel gains via `--by L=-3dB,R=0dB` |
| Channel Mixer | `channels mix` | per-output weights and phase invert, see Panning below |
| DeEsser | `deess` | specified above |
| Dynamics Processing (transfer curve) | `dynamics --curve` | new, below: compressor, expander, and gate from one curve |
| Dynamics (auto gate, compressor, expander, limiter) | `gate`, `compress`, `expand`, `limit` | above; `dynamics --preset` chains them |
| Fade Envelope, Gain Envelope | `envelope` | new, below: keyframed gain |
| Hard Limiter | `limit` | above |
| Multiband Compressor (4 bands plus limiter) | `multiband --bands 4 --limiter` | above, extended to 4 bands and an output limiter |
| Normalize (peak, DC bias adjust) | `normalize --peak`, `dc-remove` | above |
| Single-band Compressor | `compress` | above |
| Speech Volume Leveler | `leveler` | new, below |
| Tube-modeled Compressor | `compress --character tube` | new flag: saturation stage in the compressor |
| Analog Delay, Delay, Echo | `delay --analog`, `delay`, `echo` | `echo` is new, below: multi-tap with per-repeat EQ |
| DeClicker, Automatic Click Remover, Click/Pop Eliminator | `declick` | new, below |
| DeClipper | `declip` | new, below |
| Delete Silence, Mark Audio | `silence trim`, `silence detect --markers` | above |
| FFT Filter | `fft-filter` | new, below: drawn magnitude curve |
| Graphic Equalizer (10, 20, 30 bands) | `graphic-eq` | new, below: linear-phase FIR |
| Notch Filter | `notch` | new, below; `dehum` is the tuned preset |
| Parametric Equalizer | `eq` | above |
| Scientific Filter | `filter` | new, below: Bessel, Butterworth, Chebyshev, elliptic designs |
| Chorus, Chorus/Flanger, Flanger, Phaser | `chorus`, `flanger`, `phaser` | above; `chorus --mode flanger` blends the two |
| Noise Reduction (noise print) | `denoise --profile` | noise reduction section |
| Adaptive Noise Reduction | `denoise --adaptive` | new mode, below |
| Automatic Phase Correction | `phase-align` | new, below |
| DeHummer | `dehum` | new, below |
| DeReverb | `dereverb` | noise reduction section, best-effort |
| DeNoise (single slider) | `denoise --auto` | noise reduction section |
| Hiss Reduction | `dehiss` | new, below |
| Sound Remover | `remove-sound` | new, below, best-effort |
| Convolution Reverb, Full Reverb, Reverb, Studio Reverb | `reverb --ir`, `reverb --type` | above; Surround Reverb is after 1.0 |
| Distortion (drawn curves) | `distort` | new, below; `saturate` stays the musical default |
| Doppler Shifter | `doppler` | new, below |
| Guitar Suite | `guitar` | new, below |
| Mastering | `master` | mastering section |
| Loudness Meter, Loudness Radar | `analyze loudness --timeline` | above, with momentary and short-term curves added |
| Vocal Enhancer | `enhance-voice` | new, below |
| Center Channel Extractor | `center` | new, below |
| Stereo Expander | `widen --center-pan` | above, `--center-pan` added |
| Automatic Pitch Correction, Manual Pitch Correction | `tune`, `tune --target`, `envelope --target pitch` | pitch section |
| Pitch Bender | `pitch-bend` | new, below |
| Pitch Shifter, Stretch and Pitch | `pitch`, `stretch` | pitch section |
| Generate Tones, Generate Noise | `generate` | new, below; text-to-speech is not planned |
| Match Loudness (batch) | `normalize --lufs` with several inputs | above |
| Frequency Band Splitter | `band-split` | new, below |
| Essential Sound (Dialogue, Music, SFX, Ambience: loudness, repair, clarity, creative) | `enhance --preset` | new, below: one-knob chains |
| Diagnostics panel, Amplitude Statistics | `analyze problems`, `analyze stats` | new, below |
| Spectral display repair (marquee selection, Auto Heal) | `repair` | new, below |
| Remix (re-arrange to a target duration) | not in v1 | after 1.0, best-effort via beat-aligned cuts |

Sources: Adobe Audition effects reference, read 2026-10-19: [amplitude and compression](https://helpx.adobe.com/in/audition/using/amplitude-compression-effects.html), [noise reduction and restoration](https://helpx.adobe.com/audition/desktop/effects-reference/noise-reduction-restoration-effects.html), [special effects](https://helpx.adobe.com/audition/desktop/effects-reference/special-effects.html), [filter and equalizer](https://helpx.adobe.com/audition/desktop/effects-reference/filter-equalizer-effects.html), [stereo imagery](https://helpx.adobe.com/audition/using/stereo-imagery-effects.html), [time and pitch](https://helpx.adobe.com/audition/using/time-pitch-manipulation-effects.html), [Essential Sound panel](https://helpx.adobe.com/audition/desktop/editing-audio-files/essential-sound-panel.html).

**New commands**

| Command | Parameters (defaults) | Method |
| --- | --- | --- |
| `declip` | `--threshold auto` (flat-topped runs are detected), `--max-gap 10ms`, `--headroom 3dB` | Autoregressive interpolation across each clipped run; output gain is lowered so the restored peaks fit |
| `dehum` | `--freq auto` (50 or 60 Hz found from the spectrum), `--harmonics 8`, `--q 30`, `--depth -40dB`, `--slope -3dB` per harmonic, `--track on` | Cascaded tracking notches; `--residual` writes the removed hum |
| `declick` | `--sensitivity 30%`, `--complexity 16`, `--max-width 2ms`, `--big-pops off`, `--passes 4`, `--auto` for vinyl crackle | Derivative and AR-model outlier detection, interpolation across each click |
| `dehiss` | `--floor auto`, `--reduction 12dB`, `--range 2kHz..20kHz`, `--transition 6dB` | Per-band noise-floor estimate from the quietest frames, then the `denoise` gate engine tuned for broadband hiss |
| `denoise --adaptive` | `--reduction 12dB`, `--noisiness 30%`, `--decay 65%`, `--floor 0dB` | Minimum-statistics noise tracking with no profile; follows changing backgrounds such as wind or traffic |
| `repair` | `--range 1:02.50..1:02.70`, `--freq 2kHz..6kHz` (optional), `--method interpolate\|attenuate` | Spectral repair: the time-frequency region is re-synthesized from its neighbors, the marquee and Auto Heal workflow without a mouse; JSON lists what was replaced |
| `remove-sound` | `--learn 0:10..0:14` or `--model siren.json`, `--complexity 20`, `--passes 2`, `--strength 70%`, `--protect-speech on` | Learns a spectral template of the unwanted sound and subtracts it; best-effort, reports residual energy |
| `phase-align` | `--mode auto\|shift`, `--shift L=0smp,R=12smp`, `--window 200ms` | Cross-correlation per window; fixes azimuth error and microphone-placement smear |
| `center` | `--mode extract\|remove`, `--pan 0`, `--phase-tolerance 5deg`, `--width 20%`, `--freq 120Hz..8kHz`, `--mix 100%` | Mid/side with per-bin phase and pan discrimination; the quick karaoke or vocal-up tool, with `separate` as the higher-quality path |
| `leveler` | `--target -18dB`, `--amount 50%`, `--range -50dB`, `--boost-low off`, `--gate -60dB` | Slow RMS leveling with a noise-floor-aware gate, for speech |
| `enhance-voice` | `--mode male\|female\|music` | Preset chain: rumble high-pass, de-plosive, `deess`, `compress`, presence EQ |
| `enhance` | `--preset dialogue\|music\|sfx\|ambience`, `--loudness auto`, `--repair on`, `--clarity on`, `--reverb 0%` | Essential-Sound-style one-knob chain built from the commands in this document |
| `envelope` | `--points 0s=0dB,2s=-6dB,end=-60dB`, `--curve linear\|spline`, `--target gain\|pan\|pitch` | Keyframed automation of gain (default), pan, or pitch |
| `pitch-bend` | `--points 0s=0,4s=+200cents`, `--curve spline`, `--preserve-duration off` | Pitch envelope; tape-style resampling by default, shifting with `--preserve-duration` |
| `graphic-eq` | `--bands 10\|20\|30`, `--gains "+2,0,-3,..."`, `--accuracy 1000ms` | Linear-phase FIR graphic equalizer |
| `fft-filter` | `--curve "20Hz:0dB,120Hz:-6dB,1kHz:0dB,8kHz:+3dB"`, `--fft 4096` | Drawn magnitude curve applied in the frequency domain |
| `notch` | `--freq 60Hz`, `--q 30`, `--depth -40dB`, `--harmonics 0` | Single or harmonic notches |
| `filter` | `--design butterworth\|chebyshev1\|chebyshev2\|bessel\|elliptic`, `--type lowpass\|highpass\|bandpass\|bandstop`, `--freq`, `--order 4`, `--ripple 0.5dB`, `--stopband 60dB` | Scientific filter designs; JSON reports the realized response and group delay |
| `dynamics` | `--curve "-100:-100,-40:-50,0:-10"`, `--spline on`, `--attack 10ms`, `--release 100ms`, `--lookahead 5ms`, `--detector rms\|peak`, `--band 20Hz..20kHz` | Transfer-curve dynamics: compressor, expander, and gate from one drawn curve |
| `echo` | `--taps 3`, `--time 1/4`, `--feedback 40%`, `--tap-eq "lowpass:4kHz"`, `--pingpong` | Multi-tap echo with successive filtering of each repeat |
| `distort` | `--curve-pos`, `--curve-neg`, `--smooth on`, `--range 60dB`, `--mix 100%` | Drawn transfer curves for positive and negative half-waves |
| `doppler` | `--path line\|circle`, `--velocity 20m/s`, `--distance 10m`, `--pass-by 2m`, `--volume-by-distance on` | Doppler pitch and level trajectory |
| `guitar` | `--amp clean\|crunch\|lead`, `--cab <ir>`, `--filter`, `--drive`, `--compress on`, `--mix` | Compressor, filter, distortion, and cabinet impulse response in one chain |
| `generate` | `tone --freq 440Hz --dur 5s --shape sine\|square\|saw\|triangle --sweep 20Hz..20kHz`, `noise --color white\|pink\|brown`, `silence --dur 2s` | Test signals, fixtures, and tuning references |
| `band-split` | `--crossovers 120Hz,2kHz,8kHz` | Writes one file per band; the bands sum back to the input |
| `analyze problems` | `--json` | Diagnostics: clipping, DC offset, clicks, hum, hiss floor, phase issues, silence at head and tail, loudness outliers, each with a suggested fix command |
| `analyze stats` | `--histogram on` | Amplitude statistics per channel: peak, true peak, RMS, DC, dynamic range, histogram |

**Panning and stereo placement**

| Command | Parameters (defaults) | Notes |
| --- | --- | --- |
| `pan` | `--position 0` (-100 hard left to +100 hard right; names `left`, `center`, `right`, `L30`, `R60` also work), `--law -3dB\|-4.5dB\|-6dB\|linear` (`-3dB`), `--mode pan\|balance` (`pan` places a mono or mono-summed source with constant power; `balance` scales the left and right channels of a stereo source) | Static placement |
| `pan --points` | `--points 0s=-100,4s=0,8s=100`, `--curve linear\|spline`, or `--from -100 --to 100 --over 0s..end` | Movement over time, same keyframe syntax as `envelope` |
| `autopan` | `--rate 0.5Hz` or `--rate 1/2` (beats), `--depth 100%`, `--shape sine\|triangle\|square`, `--phase 0deg` | Cyclic movement |
| `channels mix` | `--out "L=0.5L+0.5R" --out "R=R" --invert R` | Audition's Channel Mixer: per-output channel weights and phase invert |
| `mix --pans` | `mix vox.wav gtr.wav keys.wav --gains 0dB,-3dB,-6dB --pans 0,-40,+40` | Place each input while summing |
| `widen` | `--amount 130%`, `--mono-below 120Hz`, `--center-pan 0` | Above; `--center-pan` added from the Stereo Expander |

Pipeline stages take the same keys (`op: pan`, `args: { position: -30 }`), and the sidecar records the resolved pan law.

**Acceptance criteria**

- `declip` restores a sine clipped 6 dB over full scale to under 1% THD; `dehum` attenuates a 60 Hz hum and five harmonics by at least 40 dB while changing a 1 kHz tone by less than 0.5 dB.
- `declick` removes at least 95% of synthetic clicks on a music fixture and changes click-free regions by less than 0.1 dB; `dehiss` reduces added hiss by at least 10 dB without lowering PESQ below the input's score.
- `phase-align` recovers an artificial 12-sample inter-channel delay to within 1 sample; `center --mode extract` reaches at least 6 dB SDR on a synthetic center-panned vocal over a wide-panned backing.
- `pan --position -100` leaves the right channel silent for a mono source; `--law -3dB` keeps summed power constant within 0.1 dB across a sweep; keyframed pans hit their values at the keyframes sample-accurately.
- `generate tone` is accurate to 0.01% in frequency; every new command passes the null test at `--mix 0%` and has a golden-file test.
- A doc-test parses the coverage map above and asserts that every listed ferraus command exists in the CLI.

## Feature spec: noise reduction

Noise reduction works the way the request describes it: learn what the noise sounds like from a region of the recording, then subtract that fingerprint from the whole file. A model-based path covers recordings with no clean noise sample, and both can be combined.

**Commands**

| Command | Purpose | Parameters (defaults) |
| --- | --- | --- |
| `denoise profile <input> -o <file.noiseprof>` | Learn a noise profile from a region | `--range 0s..2s`, or `--auto` (finds the quietest spectrally stable 1 s window and reports it), or `--from <noise-only file>` |
| `denoise <input> --profile <file>` | Apply a saved profile | `--reduction 12dB`, `--sensitivity 6`, `--smoothing 3`, `--attack 20ms`, `--release 120ms`, `--artifact-control 50%`, `--link on`, `--preserve-transients on` |
| `denoise <input> --noise-range 0s..2s` | One-shot: profile and apply in one command | same as above |
| `denoise <input> --auto` | One-shot with automatic noise-region detection | same as above; the chosen region is reported in `--json` |
| `denoise <input> --mode ml` | Model-based denoising with no profile | `--model dfn3` (DeepFilterNet 3, 48 kHz, speech-tuned), `--model rnnoise` (fast, light), `--strength 100%` |
| `denoise <input> --mode hybrid` | Profile pass first, model pass on the residual | parameters of both |
| `denoise profile merge a.noiseprof b.noiseprof -o c.noiseprof` | Combine profiles from several takes | `--method max\|mean` |

Every denoise run accepts `--residual <path>` to write the removed component as its own file, so a person can listen to what was taken out and an agent can measure it.

**Profile-based algorithm (spectral gating)**

1. STFT with a Hann window of 2048 samples at 44.1/48 kHz (scaled with sample rate), 75% overlap, in `f32`.
2. The profile stores, per frequency bin, the mean and standard deviation of magnitude in dB across the profile frames, plus FFT size, sample rate, channel count, and ferraus version. The file is JSON so agents can inspect, compare, and merge profiles.
3. For each frame and bin, the threshold is `mean + sensitivity × std`. Bins below threshold are attenuated toward `-reduction` dB through a soft sigmoid rather than a hard switch, which is the first defense against "musical noise" artifacts.
4. The gain mask is smoothed across `±smoothing` bins in frequency and with separate attack and release rates in time, with a short lookahead so onsets are never clipped.
5. `--artifact-control` blends in a minimum-statistics noise floor estimate and a median filter on the mask; higher values remove more chirping artifacts at the cost of a little more residual noise.
6. `--link on` computes one mask for all channels so the stereo image does not wobble; `--preserve-transients` detects onsets and holds the gate open through them.
7. Inverse STFT with overlap-add; with `--reduction 0dB` the chain is an exact identity.

**Model-based path**

- DeepFilterNet 3 runs through its official Rust runtime (`deep_filter`, Apache-2.0/MIT) at 48 kHz with internal resampling, and RNNoise through the pure-Rust `nnnoiseless` port. Both are speech models and the JSON result says so; on music they are offered as `--mode ml --model dfn3` but the default for music inputs stays the profile path.
- A music-aware denoise model slot exists in the registry (`denoise/music-*`) for community RoFormer-based denoise and de-reverb checkpoints; it ships only if a model with an acceptable license is available at release time.
- `--dereverb` is a separate optional stage with the same model-slot mechanism and is marked best-effort.

**Acceptance criteria**

- Identity: `--reduction 0dB` reproduces the input within -120 dBFS.
- On synthetic mixes of clean speech and music with stationary noise (fan, hiss, hum, street ambience) at 0, 10, and 20 dB SNR, the profile path improves SI-SDR by at least 8 dB at 10 dB SNR without reducing the clean signal's RMS by more than 1 dB in voiced regions.
- Artifact metric: a spectral-flux "birdie" score on the residual stays below a fixed threshold that a naive hard gate fails.
- The DeepFilterNet path matches the published DeepFilterNet 3 PESQ on the VoiceBank-DEMAND test set within 0.05, proving the runtime integration is faithful.
- Constant memory on multi-hour inputs; at least 20x real time single-threaded for the profile path.

## Feature spec: stem separation

"Better than Demucs" is achievable with open models today: band-split RoFormer models and ensembles outscore Demucs v4 fine-tuned by 3 to 4 dB SDR on vocals and about 2 dB on bass and drums on the [MVSEP Multisong benchmark](https://mvsep.com/en/algorithms), so ferraus's default tier is a RoFormer-class model and its top tier is an ensemble, with Demucs kept as the fast tier.

**Commands**

| Command | Parameters (defaults) |
| --- | --- |
| `separate <input>` | `--stems 2\|4\|6` (`4`), `--tier fast\|balanced\|quality\|max` (`quality`), `--only vocals,drums` (compute a subset), `--instrumental` (one file with vocals removed), `-o "{name}.{stem}.wav"`, `--bit-depth 24` |
| `separate --model <name>` | Pin a registry model instead of a tier; `--ensemble a,b,c --weights 0.5,0.3,0.2` for a custom ensemble |
| `separate --vocal-split` | Second stage on the vocal stem: `lead` and `backing` outputs |
| `separate --drum-split` | Second stage on the drum stem: `kick`, `snare`, `toms`, `hh`, `cymbals` |
| `separate --extended guitar,piano,keys,strings,brass` | Best-effort cascaded extraction from the `other` stem; each output carries a quality flag |
| Quality controls | `--overlap 25%` (window overlap), `--shifts 1` (test-time augmentation, `max` tier uses 2), `--segment auto`, `--sum-to-mix on` |

**Tiers**

| Tier | Model(s) | 4-stem average SDR target (Multisong) | CPU time, 4-minute song | Notes |
| --- | --- | --- | --- | --- |
| `fast` | Demucs v4 `htdemucs` | 9.1 | under 2 min | MIT weights, smallest download (about 80 MB) |
| `balanced` | Demucs v4 `htdemucs_ft` | 9.3 | under 4 min | 4 fine-tuned models, one per stem |
| `quality` (default) | Best open-licensed BS-RoFormer or Mel-Band RoFormer checkpoint for vocals plus a 4- or 6-stem RoFormer for the rest | 11.5 | under 15 min (under 1 min on GPU) | Chosen at release by the license audit in Open questions |
| `max` | Ensemble: BS-RoFormer + Mel-Band RoFormer + SCNet XL for vocals; RoFormer SW + Demucs `htdemucs_ft` for bass and drums; `shifts 2` | 12.5 | under 45 min (under 3 min on GPU) | Mirrors the public MVSEP ensemble recipe; targets within 1 dB of its published 13.67 |

Reference points from the MVSEP Multisong leaderboard, SDR in dB (vocals / bass / drums / other):

| Model | Vocals | Bass | Drums | Other | Piano | Guitar |
| --- | --- | --- | --- | --- | --- | --- |
| Demucs v4 `htdemucs_ft` | 8.33 | 12.05 | 11.24 | 5.74 | 2.23 (6s model) | — |
| BS Roformer SW (6 stems) | 11.30 | 14.62 | 14.11 | 8.71 | 7.83 | 9.05 |
| BS Roformer 124 bands (2026.07, vocals only) | 12.33 | — | — | — | — | — |
| SCNet XL IHF (vocals only) | 11.11 | — | — | — | — | — |
| MVSEP 4-stem ensemble (2025.06) | 11.93 | 14.85 | 14.33 | 9.00 | — | — |

Source: [MVSEP algorithms and quality tables](https://mvsep.com/en/algorithms), read 2026-10-18. Architecture papers: [BS-RoFormer](https://arxiv.org/abs/2309.02612), [Mel-Band RoFormer](https://arxiv.org/pdf/2310.01809), [SCNet](https://arxiv.org/abs/2401.13276), [Demucs v4](https://github.com/facebookresearch/demucs).

**Pipeline**

1. Decode, resample to the model rate (44.1 kHz for all current models), and convert to stereo (mono inputs are duplicated and the output is folded back to mono).
2. Cut into windows (`--segment auto` picks each model's trained length, for example 7.8 s for Demucs HT) with `--overlap` crossfaded by a raised-cosine window; windows run in parallel on GPU or across CPU threads.
3. Test-time augmentation: `--shifts N` runs N random sub-second offsets and averages, the same trick Demucs uses; `max` tier also averages channel-swapped passes.
4. Ensembles average spectrogram magnitudes per stem with the configured weights (or `--ensemble-mode avg\|max\|min` for "fullness" versus "bleedless" preferences, the same trade-off the MVSEP tables expose).
5. `--sum-to-mix on` assigns the residual (mix minus the sum of stems) to `other`, so the stems add back to the original sample-exactly; `off` keeps raw model outputs.
6. Cascades: `--vocal-split` runs a lead/backing model on the vocal stem (reference: BS Roformer 124-band karaoke model, lead 10.77 dB, backing 7.03 dB on MVSEP's set); `--drum-split` runs a DrumSep-class model on the drum stem; `--extended` runs instrument models on `other`. Running on an already-filtered stem is what makes these second stages work.
7. Write each stem as it finishes, so partial results survive a cancel, and emit a per-stem `quality` estimate in JSON: in ensemble mode this is the agreement between member models; in single-model mode it is a calibrated score from the model's own mask confidence.

**Model sourcing**

- Demucs v4 weights are MIT and ship in the registry from day one.
- RoFormer and SCNet checkpoints come from the open community (lucidrains' implementation, weights by viperx, Kimberley Jensen, anvuew, becruily, gabox, and others on Hugging Face). Each candidate needs a recorded license before it enters the registry; checkpoints without a clear license are excluded from default tiers and can only be used via an explicit `--model` path with a warning. The MVSEP team's own best checkpoints are not public and are referenced here only as targets.
- A companion `ferraus-models` repo holds the PyTorch-to-ONNX export scripts for each architecture, the per-stem validation runs, and the manifest with SHA-256 hashes. Export keeps STFT and band-split inside the ONNX graph where ONNX Runtime supports the ops; otherwise `ferraus-ml` implements that stage in Rust and the export is tested for numerical parity (max absolute difference under 1e-4 on a fixed clip).
- Training new checkpoints (for example a strings model, or a RoFormer under a license the project controls) is out of scope for v1 but the export and evaluation harness is designed so that it is the only missing step.

**Acceptance criteria**

- On MUSDB18-HQ test (requires the dataset license) and the public MVSEP Multisong set where available, the `quality` tier beats `htdemucs_ft` on vocals by at least 2 dB SDR and on bass and drums by at least 1 dB; the `max` tier is within 1 dB of the published MVSEP ensemble averages.
- The ONNX ports of each model match the reference PyTorch outputs within 1e-4 max absolute error on a 10-second fixture.
- `--sum-to-mix on` outputs sum to the input within 1 LSB at 24-bit.
- Chunked processing gives identical results to whole-file processing on a 30-second fixture (no seam artifacts), verified by a spectral discontinuity test at window boundaries.
- A 60-minute input runs with peak memory under 4 GB on CPU in every tier.

## Feature spec: pitch and time

Pitch correction is a three-stage job: track the pitch, decide where each note should be, and move it there without changing the tone color of the voice. ferraus does each stage with a swappable component so the natural-sounding default can be traded for speed or for a stronger, deliberate effect.

**Commands**

| Command | Purpose | Parameters (defaults) |
| --- | --- | --- |
| `tune <input>` | Pitch correction (auto-tune) | `--key auto` (or `C`, `F#`), `--scale major\|minor\|chromatic\|<custom notes>` (`major`/`minor` follow detected key), `--strength 100%`, `--retune-speed 25ms`, `--humanize 0%`, `--vibrato-preserve on`, `--formant preserve\|shift` (`preserve`), `--range 60Hz..1.2kHz`, `--tracker pyin\|crepe\|rmvpe\|swiftf0` (`pyin`), `--notes-out <file.json>` |
| `tune --target <midi>` | Pull the vocal to a written melody instead of a scale | `--tolerance 150cents` (notes farther than this are left alone and reported) |
| `tune --effect hard` | Deliberate robotic effect | sets `--retune-speed 0ms --strength 100% --humanize 0%` |
| `pitch <input>` | Shift pitch, keep duration | `--semitones 0`, `--cents 0`, `--formant preserve\|shift` (`preserve`), `--quality high\|fast` |
| `stretch <input>` | Change duration, keep pitch | `--factor 1.0`, or `--to 3:30`, or `--bpm 120 --to-bpm 128`, `--mode auto\|tonal\|percussive\|speech` (`auto`), `--quality high\|fast` |
| `analyze pitch <input>` | Pitch curve as JSON or CSV | `--tracker`, `--hop 10ms`, `--range` |
| `analyze key <input>` | Key and scale with confidence | `--method chroma` |

**Stage 1: pitch tracking.** The default tracker is pYIN, implemented in pure Rust in `ferraus-pitch`, which needs no model download and is accurate on clean monophonic vocals. Model trackers plug in through `ferraus-ml` for noisy or breathy material: CREPE (MIT), RMVPE, and SwiftF0 are candidates, selected by `--tracker` and audited for license like any other model. Output per frame (10 ms hop): frequency, confidence, voiced flag, RMS.

**Stage 2: target decision.**

- Key detection runs on the whole take (chroma profile correlation against major and minor templates) and reports confidence; below 0.6 confidence the scale falls back to chromatic and the JSON result says why.
- Each voiced frame gets a target: the nearest scale note, or the written note when `--target` is set.
- Notes are segmented from the pitch track and confidence (onset = voicing starts or pitch jumps more than 80 cents and stays; minimum note length 40 ms) so that correction operates per note and not per frame. That is what keeps scoops and slides musical.
- Within a note, the correction curve is the deviation from target filtered by `--retune-speed` (a one-pole smoother, so 0 ms is instant and 100 ms or more is gentle). `--vibrato-preserve` estimates vibrato as the 4 to 8 Hz component around the note center and subtracts it before smoothing, then adds it back; `--humanize` adds a bounded random drift seeded by `--seed`. `--strength` scales the whole correction curve.

**Stage 3: resynthesis.** The default shifter is TD-PSOLA (time-domain pitch-synchronous overlap-add), which moves pitch while leaving formants in place and is the standard choice for natural-sounding monophonic vocals. The phase-vocoder shifter (with formant envelope correction via cepstral smoothing) is the fallback for polyphonic or noisy input and for `pitch` on non-vocal material. Both are pure Rust; the `ssstretch` binding to Signalsmith Stretch (MIT) is an optional feature for the `stretch` command's `high` quality mode until the pure-Rust stretcher matches it in listening tests.

**Time stretch.** `stretch` uses a hybrid of phase vocoder (tonal content, with phase locking below 150 Hz) and WSOLA (transients), choosing per segment in `auto` mode from a transient detector; `percussive` forces WSOLA with transient preservation and `speech` uses shorter windows. Beat-synced stretches (`--bpm`, `--to-bpm`) use the tempo from `analyze tempo` when `--bpm` is omitted.

**Acceptance criteria**

- On a synthetic vocal fixture detuned by ±40 cents per note, `tune` brings every note within 5 cents of target at `--strength 100%` and within 20 cents at `--strength 50%`.
- Formant preservation: the first three formant centers of a sustained vowel move by less than 3% after a 4-semitone shift (measured by LPC).
- Vibrato preservation: a 6 Hz, 40-cent vibrato survives correction with amplitude within 20% of the original.
- pYIN matches the reference implementation's raw pitch accuracy within 1% on the MDB-stem-synth or a comparable public vocal set.
- `stretch --factor 1.0` is the identity; `--factor 2.0` on a click track keeps every click within 1 ms of its expected position.
- `tune` runs under 60 seconds CPU for a 4-minute vocal with the default tracker.

## Feature spec: MIDI, hum-to-MIDI, and rendering

MIDI support exists so a hummed idea becomes a playable part: transcribe the hum, quantize it to the beat, and render it through a real instrument. Every MIDI command also has a JSON form of the file so agents can edit notes without a MIDI library.

**MIDI editing commands**

| Command | Purpose | Parameters (defaults) |
| --- | --- | --- |
| `midi info`, `midi to-json`, `midi from-json` | Inspect and round-trip a MIDI file as JSON: tracks, notes (onset and duration in beats and seconds), velocities, tempo map, time signatures, controllers | `--ppq 960` on write |
| `midi quantize <in.mid>` | Snap notes to a grid | `--grid 1/16` (or `auto` to detect the dominant subdivision), `--strength 100%`, `--swing 0%`, `--what start\|end\|both\|duration` (`start`), `--humanize 0ms`, `--min-length 1/64`, `--no-overlap on` |
| `midi groove extract <ref.mid> -o groove.json` and `midi quantize --groove groove.json` | Capture timing and velocity offsets per grid position from a reference and apply them | `--grid 1/16` |
| `midi transpose`, `midi velocity`, `midi legato`, `midi tempo`, `midi split-tracks`, `midi merge`, `midi trim` | Utilities | `--semitones`, `--scale 1.2`, `--compress 50%`, `--set 120`, `--range` |

**Quantize algorithm.** Notes are mapped to ticks against the file's tempo map, the grid is `ppq × 4 / subdivision` ticks (triplet and dotted grids supported), `--swing` delays every second grid point by `swing% × grid`, and each note-on moves by `strength × (nearest grid − onset)`. `--what` decides whether note-offs move with note-ons (`start`), snap independently (`both`), or keep the original duration. `--humanize` adds seeded Gaussian jitter after quantization. `--groove` replaces the fixed grid with per-position offsets and velocity deltas extracted from a reference file, which is how a stiff part picks up the feel of a drummer.

**Hum-to-MIDI (`transcribe`)**

`ferraus transcribe <audio> -o part.mid` turns singing, humming, whistling, or a monophonic instrument into notes; `--mode poly` handles piano and guitar chords with the Basic Pitch model.

| Parameter | Default | Meaning |
| --- | --- | --- |
| `--mode mono\|poly\|auto` | `auto` | `auto` picks `mono` when the pitch tracker reports a single voiced line |
| `--tracker pyin\|crepe` | `pyin` | Pitch tracker for mono mode |
| `--min-note 60ms` | 60 ms | Shorter notes merge into neighbors |
| `--onset-sensitivity 50%` | 50% | Energy-dip and pitch-jump thresholds for splitting repeated notes |
| `--velocity from-rms\|fixed:<n>` | `from-rms` | Peak RMS per note mapped to 1 to 127 through a loudness curve |
| `--bpm auto\|<n>` | `auto` | Tempo from `analyze tempo`, or fixed |
| `--quantize <grid>` | off | Apply quantization in the same run |
| `--key-snap on\|off` | `on` | Snap to the detected key when confidence is above 0.6 |
| `--pitch-bend on\|off` | `off` | Encode slides as pitch bend (±2 semitone range) instead of discrete notes |
| `--octave auto\|<n>` | `auto` | Shift so the median note lands in the target instrument's comfortable range |
| `--program 0` | 0 | GM program number written into the track |

Mono algorithm: pitch track at 10 ms hop → voiced segments → split on sustained pitch jumps over 80 cents or energy dips → median pitch per segment rounded to a MIDI note → velocity from peak RMS → merge sub-minimum notes → optional key snap → tempo map → MIDI file plus a JSON sidecar with per-note confidence. Polyphonic mode runs Basic Pitch (Apache-2.0, exported to ONNX) and post-processes its note activations with the same merge and snap rules.

**Rendering (`render`)**

`ferraus render part.mid -o part.wav --instrument <source>` renders MIDI to audio through one of:

- A SoundFont (`.sf2`) via the pure-Rust `oxisynth` engine; a small General MIDI set installs with `ferraus sounds install gm-basic`.
- An SFZ library (`.sfz`) via `sfizz` (feature-gated FFI), which opens the large free sample libraries (pianos, strings, orchestral sets).
- A hosted instrument plugin (`--instrument plugin:"Surge XT" --preset "Pad 1"` or `--state preset.vstpreset`) through the plugin host described in the next section.

`--sample-rate 48000`, `--bit-depth 24`, `--tail 2s` (let releases ring), `--fx "reverb:mix=0.2"` to apply effects in the same run.

**One-shot: `hum`.** `ferraus hum voice.wav --to piano -o idea.wav` runs `transcribe`, `midi quantize --grid 1/16 --strength 80%`, and `render` with the named installed sound, and writes the intermediate `.mid` beside the output so the part can be edited and re-rendered.

**Acceptance criteria**

- `midi quantize --strength 100%` on a jittered fixture returns every note-on exactly on the grid; `--swing 60%` places every second sixteenth at 60% of the grid interval.
- `to-json` then `from-json` reproduces a MIDI file's events exactly (tested on a corpus of free MIDI files).
- Mono transcription on synthetic sung-melody fixtures (sine-plus-formant vocals with known notes) reaches at least 95% note F-measure at 50 ms onset tolerance and exact pitch; on real humming fixtures at least 85%.
- Basic Pitch ONNX output matches the reference implementation's note list on its own example clips.
- `render` output duration equals the MIDI length plus tail within 1 ms, and note onsets in the rendered audio align to MIDI onsets within 1 ms.

## Feature spec: scores and sheet music

ferraus turns audio or MIDI into readable notation (a drum score, a lead sheet or grand-staff piano score with guitar chord symbols, or solfa text) and goes the other way, reading a sheet-music PDF into MIDI and performing it; MusicXML is the hub format, MIDI is always produced first, and every output carries per-measure confidence so an AI model knows which bars to trust and which to fix.

**Commands**

| Command | Purpose | Parameters (defaults) |
| --- | --- | --- |
| `score <audio or midi>` | Notation from a recording or a MIDI file | `--layout auto\|lead-sheet\|grand\|treble\|bass\|drums\|solfa` (`auto`: vocals become a lead sheet, piano a grand staff, drums a drum score), `--chords on\|off\|diagrams` (`on`: chord symbols; `diagrams` adds guitar fretboard diagrams), `--solfa off\|on\|only` (`only` writes solfa text and no staff), `--key auto`, `--time auto`, `--tempo auto`, `--quantize 1/16`, `--triplets auto`, `--split auto` (grand-staff hand split, or a note such as `C4`), `--parts auto\|vocals,piano,guitar,bass,drums` (runs `separate` first), `--lyrics <file>`, `--transpose 0`, `--title`, `--composer`; outputs `-o score.musicxml` plus any of `--midi`, `--pdf`, `--svg`, `--png`, `--abc`, `--ly`, `--solfa-txt`, `--json` |
| `score read <pdf or image>` | Optical music recognition: sheet music to MusicXML | `--pages 1-4`, `--dpi 300`, `--engine builtin\|audiveris` (`builtin`), `--review` (per-measure confidence and issue list), `--fix <edits.json>` (apply corrections) |
| `score play <musicxml, mei, abc, ly, or mid>` | Notation to a MIDI performance | `--tempo` (override), `--repeats on`, `--dynamics on`, `--articulation on`, `--humanize 0%`, `--swing 0%`, `--instrument-map "P1=piano,P2=strings"`, `--click off` |
| `score perform <pdf or notation>` | One shot: read, play, render | everything above plus `--instrument piano` and `-o song.wav`; the MIDI and MusicXML are written beside the audio |
| `score convert <in> <out>` | Between MusicXML, MEI, ABC, LilyPond, MIDI, PDF, SVG, PNG, solfa text, and JSON | formats from the extensions |
| `score to-json`, `score from-json` | The agent-facing score model: measures, voices, notes (pitch, duration, ties), chords, lyrics, solfa syllables, confidence | `--pretty` |
| `score check` | Validates a score: measure durations, voices, unsupported constructs | `--json` |

**Audio to notation pipeline**

1. Parts: `--parts auto` runs `separate` and transcribes each stem with the right tool: vocals with the monophonic pitch tracker, piano and guitar with the polyphonic transcriber, bass with the monophonic tracker in a low range, drums with the drum transcriber (kick, snare, hi-hat, toms, cymbals from the `--drum-split` stems plus onset detection).
2. Time: `analyze tempo` gives the beat grid and downbeats, so bar lines and the time signature (4/4, 3/4, 6/8 chosen from the accent pattern) come from the music, not a default; pickup bars are detected.
3. Rhythm: notes are quantized to `--quantize` with triplet and dotted detection, split into at most two voices per staff, rests are filled, and ties cross bar lines.
4. Pitch spelling: `analyze key` picks the key and accidentals are spelled for it (F# in G major, Gb in Db major).
5. Harmony: chord symbols per beat or bar from chroma templates over the harmonic stems with the bass note for slash chords (`C`, `Am7`, `G/B`); `--chords diagrams` adds guitar voicings from a built-in table of common shapes, with capo suggestions when `--transpose` is used.
6. Layout: lead sheet (treble clef, chord symbols above, lyrics below when given), grand staff (treble and bass clefs with hand split), drum score (percussion clef with the standard drum map: kick in the bottom space, snare in the third space, hi-hat and cymbals as x noteheads above the staff), or solfa.
7. Solfa: tonic sol-fa with movable do (`d r m f s l t`, octave marks, Curwen rhythm punctuation), written as text and Markdown; `--solfa fixed` for fixed-do; `--solfa only` suppresses the staff entirely for people who read solfa and not notation.
8. Writers: MusicXML 4.0, ABC, LilyPond source, solfa text, and the JSON model are pure Rust; SVG, PNG, and PDF engraving go through Verovio (see tooling below), with a LilyPond bridge as an optional publication-quality path.
9. Confidence: every note carries the transcriber's confidence, every measure an aggregate, and the JSON lists measures below the threshold with the reason (polyphony too dense, tempo unstable, low-energy passage).

**Sheet music to performance pipeline**

1. `score read` rasterizes PDF pages at 300 dpi (photos and scans are accepted as images), detects staves and systems, recognizes symbols, and writes MusicXML with per-measure confidence; `--review` lists measures whose durations do not add up or whose symbols were uncertain.
2. An agent inspects the result with `score to-json`, fixes what it needs to with `score read --fix edits.json` (for example `measure 12, voice 1, note 3, pitch E5`) or `score from-json`, and runs `score check` until it passes.
3. `score play` turns the notation into MIDI: tempo and tempo changes from the score or `--tempo`, dynamics to velocity, articulations to note lengths, repeats and endings unrolled, optional humanize and swing.
4. `render` plays the MIDI through the chosen instrument (piano by default, or any sound from the sound folders) and the usual effects and mastering stages apply. `score perform` does all four steps in one command and writes every intermediate file.

**Tooling and models** (all subject to the license audit in the model registry)

| Need | Choice | License and notes |
| --- | --- | --- |
| Polyphonic note transcription | Basic Pitch | Apache-2.0; already in the registry |
| Multi-instrument and drum transcription | [YourMT3+](https://arxiv.org/abs/2407.04822) as the registry slot `transcribe/multi-*`; MuScriptor (2026, open-weight) only if its gated license permits redistribution | audit required; drums are onset-only in these models, so the drum map uses `--drum-split` stems too |
| Optical music recognition | Built-in: the oemer segmentation networks and the homr transformer exported to ONNX (both MIT); optional [Audiveris](https://github.com/Audiveris/audiveris) bridge (`doctor --install-audiveris`, Java, AGPL) for the highest accuracy on printed scores | printed scores work well, handwritten scores are best-effort and say so in the report |
| Engraving | [Verovio](https://www.verovio.org/) through the `verovio` Rust crate (LGPL-3.0) for SVG, PNG, and PDF, isolated in a `ferraus-engrave` helper executable like the VST3 host; optional LilyPond bridge (GPL, external) for publication quality | LGPL isolation decision listed in Open questions |
| Chord recognition | Native chroma templates with bass-note tracking; a model slot `chords/*` for later | pure Rust |
| PDF rasterizing | `pdfium-render` | Apache-2.0 or BSD |

**Acceptance criteria**

- Round trip: `score` on a MIDI rendered from a known MusicXML file reproduces pitches and durations exactly on the quantized test set; on recorded piano fixtures note F1 is at least 0.85 and 95% of bar lines fall within one beat of the reference.
- Drum score from a drum-stem fixture: kick, snare, and hi-hat F1 at least 0.9 at 50 ms tolerance, and the engraved page uses the standard drum map.
- Chord symbols: at least 75% accuracy on major, minor, and seventh chords on a public chord dataset with a licensed copy; slash chords carry the detected bass.
- Solfa: syllables match the reference for the test melodies in every key, and `--solfa only` output contains no staff.
- OMR: on a clean printed test set, pitch-plus-duration accuracy of at least 90%; every `score read` output passes `score check` or lists the failing measures in `--review`.
- `score play` on a MusicXML file with repeats and dynamics has the expected duration and velocity changes; `score perform` on a printed piano PDF writes MusicXML, MIDI, and WAV in one run with audio onsets within 1 ms of the MIDI.
- Engraved SVG and PDF pages have no overlapping symbols on the fixture set (visual regression test), and every example in this section runs in the documentation tests.

## Feature spec: plugin hosting and installable sounds

ferraus hosts CLAP and VST3 plugins headlessly, in a separate process, with parameters and presets set from the command line, and it ships a curated registry so `ferraus sounds install piano` gives a user or agent a real instrument in one command.

**Plugin commands**

| Command | Purpose | Parameters (defaults) |
| --- | --- | --- |
| `plugin scan` | Find plugins in the standard per-OS directories and cache their metadata | `--paths <dir,...>` extra locations, `--format clap,vst3`, `--rescan` |
| `plugin list` | List cached plugins | `--instruments`, `--effects`, `--format` |
| `plugin info <name-or-id>` | Parameters (id, name, range, default, unit), presets, audio bus layout, latency, MIDI support | `--json` |
| `plugin run <input> --plugin <name>` | Run an effect plugin over audio | `--param name=value` (repeatable; value in display units or `norm:0.6`), `--preset <name>`, `--state <file>`, `--tail 2s`, `--block-size 512`, `--mix 100%` |
| `render --instrument plugin:<name>` | Drive an instrument plugin with MIDI (see MIDI section) | same parameter flags |
| `plugin presets list <name>` and `plugin state save\|load` | Preset discovery and native state as base64 JSON |  |

**Formats and priority**

1. CLAP first: a clean MIT-licensed C ABI with an actively maintained safe Rust host crate (`clack`). Most modern free instruments ship CLAP builds.
2. VST3 second: the widest catalog, hosted through bindings to the VST3 SDK. The SDK is dual-licensed GPLv3 or proprietary, so this code lives only in the `ferraus-plughost` binary (see Licensing).
3. Audio Units (macOS) and LV2 (Linux) are later phases; VST2 is not supported because its SDK is no longer licensable.

Standard scan locations: CLAP in `~/.clap`, `/usr/lib/clap`, `~/Library/Audio/Plug-Ins/CLAP`, `/Library/Audio/Plug-Ins/CLAP`, `%COMMONPROGRAMFILES%\\CLAP`, `%LOCALAPPDATA%\\Programs\\Common\\CLAP`; VST3 in `~/.vst3`, `/usr/lib/vst3`, `~/Library/Audio/Plug-Ins/VST3`, `/Library/Audio/Plug-Ins/VST3`, `%COMMONPROGRAMFILES%\\VST3`.

**Headless hosting rules**

- No GUI is ever opened. Parameters are addressed by stable id or display name, values accept display units (`cutoff=2.5kHz`, `mix=30%`) or normalized (`norm:0.3`) and are converted through the plugin's own text-to-value functions.
- Audio runs offline at the output sample rate in fixed blocks; MIDI events are sample-accurate; latency reported by the plugin is compensated; `--tail` lets reverbs and releases ring out.
- State is captured as the plugin's native blob (base64 inside the JSON sidecar) so a render can be reproduced later, and `--dry-run --json` shows the resolved parameter set.
- Determinism caveat: plugins may use internal randomness or free-running LFOs. The sidecar records this as `deterministic: unknown` for plugin stages.
- Isolation: one `ferraus-plughost` process per plugin instance, shared-memory audio, watchdog timeout (default 60 s without progress), and exit code 7 with the plugin's name on any crash. The repository ships a tiny test CLAP plugin that crashes on demand to keep this path tested.

**Installable sounds registry**

| Command | Purpose |
| --- | --- |
| `sounds search <query>` | Search the registry by name, tag (`piano`, `synth`, `drums`, `strings`), format, or license |
| `sounds install <id>` | Download, verify SHA-256, and place the bundle: plugins into the per-user plugin directory, SoundFonts and SFZ libraries into `<data>/ferraus/sounds/`; then rescan |
| `sounds install --from <file>` | Register a bundle the user downloaded themselves (for free-but-proprietary instruments whose vendors require their own installer) |
| `sounds list`, `sounds info <id>`, `sounds remove <id>`, `sounds update` | Lifecycle; `info` shows license text and size before anything downloads |
| `sounds alias piano=<id>` | Set what `--to piano` and `--instrument piano` resolve to |

The registry is a signed JSON manifest hosted beside the model registry, listing for each entry: id, name, type (`clap`, `vst3`, `sf2`, `sfz`, `ir`), license, size, platforms, download URL, SHA-256, and tags. Candidate first catalog, every entry subject to the license audit: Surge XT (GPL-3.0, CLAP and VST3), Dexed (GPL-3.0), Odin 2 (GPL-3.0), Cardinal (GPL-3.0), Dragonfly Reverb (GPL-3.0), sfizz player (BSD-2), FluidR3 GM SoundFont (MIT), GeneralUser GS (free license), Salamander Grand Piano (CC BY 3.0), VSCO 2 Community Edition (CC0), Virtual Playing Orchestra (free, mixed CC), and a CC0 impulse-response pack. Vital and Decent Sampler are free but proprietary and appear as `install --from` entries that link to the vendor download.

ferraus never runs vendor installers, never asks for administrator rights, and writes only to per-user directories.

**Sound folders and instrument switching**

- `--sound-path <dir>` (repeatable) on every command that uses an instrument (`render`, `hum`, `vocoder --carrier plugin:...`, `plugin run`, `plugin info`), the `FERRAUS_SOUND_PATH` environment variable (a path list), and `[sounds] paths = [...]` in the config file all add folders that ferraus scans for CLAP and VST3 bundles, SF2 and SFZ libraries, and impulse responses. `sounds scan <dir>` indexes a folder once and caches the result; `sounds scan --watch` re-indexes when files change.
- Instrument names resolve in this order: (1) an alias from `sounds alias` (`piano=...`), (2) an installed or scanned sound whose name or tags match (`strings`, `ensemble`, `guitar`, `nylon guitar`), (3) a General MIDI program name or number (`gm:48`, `"String Ensemble 1"`) rendered with the default GM soundfont, (4) an explicit source (`plugin:"Surge XT"`, `sf2:<path>`, `sfz:<path>`, `plugin:<path>`). An ambiguous name lists the candidates in the error with a `hint` command for each.
- Switching the sound of an existing part: `render part.mid --instrument strings` re-renders the same MIDI with a different sound; `render song.mid --map "1=piano,2=strings,10=drums"` maps MIDI tracks or channels to instruments; `render --swap piano=strings` substitutes by General MIDI family; `hum voice.wav --to ensemble` resolves the same way.
- `sounds list --path <dir> --json` shows what a folder contains with the category ferraus inferred for each entry (keys, strings, guitar, bass, drums, synth, pad, brass, winds, vocal, fx), so an agent can pick by category.
- The resolved instrument (file path, plugin id, preset) is written to the sidecar next to the name that was given, so a render is reproducible even if aliases change later.

**Acceptance criteria**

- `plugin scan` finds a CLAP and a VST3 test plugin in the standard directories on Linux, macOS, and Windows CI runners.
- `render --instrument plugin:"Surge XT"` of a one-note MIDI file produces non-silent audio whose onset is within 1 ms of the MIDI onset, and the same command with a saved `--state` reproduces the render bit-exactly when the plugin is deterministic.
- A deliberately crashing test plugin returns exit code 7 within the watchdog timeout and leaves `ferraus` running and able to process the next job.
- Latency compensation: an impulse through a known-latency test plugin lands at the original sample position.
- `sounds install` refuses any download whose hash does not match the manifest, and `sounds info` prints the license before install.
- `--sound-path` pointing at a folder that holds a CLAP instrument, an SF2 file, and an SFZ library makes all three available to `render --instrument <name>` with no `sounds install`.
- Rendering the same MIDI with `--instrument piano` and then `--instrument strings` changes only the sound: note onsets and durations stay within 1 ms.

## Feature spec: analysis and pipelines

Analysis commands return numbers an agent can reason with (tempo, key, loudness, silence map, pitch curve), and pipeline files let one invocation express a whole job graph so multi-step work is reproducible from a single document.

**Analysis commands** (all return JSON with `--json`; human mode prints a short summary)

| Command | Output | Method and defaults |
| --- | --- | --- |
| `analyze tempo` | BPM, confidence, beat positions (s), downbeats when confident | Onset strength envelope → autocorrelation and comb-filter tempogram → dynamic-programming beat tracking; `--range 60..200`; an ML beat tracker slot exists for a later phase |
| `analyze key` | Key, mode, confidence, top-3 alternatives | Chroma from a constant-Q transform correlated with Krumhansl-Schmuckler profiles |
| `analyze loudness` | Integrated LUFS, short-term max, momentary max, loudness range, true peak, sample peak | EBU R128 via `ebur128` |
| `analyze silence` | List of silent regions with start, end, and floor level | `--threshold -40dB`, `--min 300ms` |
| `analyze pitch` | Frame-by-frame pitch curve (see Pitch and time) | `--tracker pyin` |
| `analyze spectrum` | Long-term average spectrum in 1/3-octave or per-bin JSON | `--resolution 1/3oct` |
| `analyze clipping` | Count and positions of clipped runs | `--threshold 0dBFS`, `--min-run 3` |
| `analyze chords` | Chord sequence with times and confidence (best-effort) | Chroma templates; labeled best-effort in output |
| `analyze similarity a b` | Spectral and loudness similarity score between two files | For agents checking whether a processed take drifted from the original |
| `compare a b` | Null-test metrics: RMS difference, peak difference, SI-SDR of b against a, aligned by cross-correlation | The agent's regression test |

**Pipelines**

`ferraus pipeline run job.yaml` executes a graph of stages over one or more inputs; `ferraus pipeline validate` checks it without running; `ferraus pipeline explain` prints the resolved plan with every default filled in. YAML and JSON are both accepted, and `--set key=value` overrides any field from the command line.

```yaml
# job.yaml — clean a vocal take, tune it, and master a bounce
version: 1
inputs:
  vocal: takes/vocal_take3.wav
  beat: beats/instrumental.wav
stages:
  - id: clean
    op: denoise
    in: vocal
    args: { auto: true, reduction: 12dB }
  - id: tuned
    op: tune
    in: clean
    args: { key: auto, strength: 85%, retune_speed: 30ms }
  - id: vox_fx
    op: fx
    in: tuned
    args:
      chain:
        - { op: compress, preset: vocal }
        - { op: deess }
        - { op: reverb, type: plate, mix: 18% }
  - id: mix
    op: mix
    in: [vox_fx, beat]
    args: { gains: [0dB, -3dB] }
  - id: master
    op: fx
    in: mix
    args:
      chain:
        - { op: normalize, lufs: -14 }
        - { op: limit, ceiling: -1dBTP }
outputs:
  - { from: master, path: out/song.wav, bit_depth: 24 }
  - { from: tuned, path: out/vocal_tuned.wav }
```

Rules: stage `op` names and `args` keys match the CLI commands and flags exactly (hyphens become underscores); every stage result can be an input to later stages or an output; the executor runs independent branches in parallel, caches stage results keyed by content hash so a re-run after editing one stage recomputes only what changed (`--no-cache` disables), and writes one sidecar per output plus a `job.ferraus.json` with the full resolved plan and timings.

`mix` is a pipeline-only op (sum inputs with gains and optional alignment offsets); `ferraus mix a.wav b.wav --gains 0dB,-3dB` exists as a verb too.

**Acceptance criteria**

- `analyze tempo` within ±2 BPM (or a tempo-octave) on at least 90% of a public beat-tracking test set; beat positions within 70 ms for at least 85% of beats on the same set.
- `analyze key` correct key on at least 70% of a public key-detection set, with the correct key in the top 3 at least 90% of the time.
- `analyze loudness` matches the EBU R128 compliance test vectors within 0.1 LU.
- Pipeline cache: re-running a 5-stage job after changing only the last stage's args performs exactly one stage of work.
- `pipeline run` on the example above completes on CPU in under 3 minutes for a 3-minute input.

## Feature spec: mastering

`ferraus master` turns a finished mix into a release-ready master in one command, and every stage it uses is also an individual command, so an AI model can either accept the automatic result or run its own analyze, adjust, and verify loop until the measurements and a listening check agree.

**Commands**

| Command | Purpose | Parameters (defaults) |
| --- | --- | --- |
| `master <mix>` | Automatic master to a delivery target | `--target streaming\|apple\|youtube\|club\|cd\|broadcast\|podcast\|custom` (`streaming`), `--lufs` and `--true-peak` for `custom`, `--reference <track>` with `--match-amount 50%`, `--genre pop\|hiphop\|edm\|rock\|acoustic\|classical\|jazz\|podcast` (`auto` from analysis), `--intensity light\|medium\|heavy` (`medium`), `--width 100%`, `--bass-mono 120Hz`, `--warmth 15%`, `--no-eq`, `--no-widen`, `--trim-silence -70dB`, `--report <json>`, `--ab` (loudness-matched before and after pair for listening) |
| `master analyze <mix>` | The measurement report an agent decides from | `--reference <track>`, `--genre`, `--json` |
| `master plan <mix>` | The exact chain `master` would apply, every parameter resolved, editable | `--target`, `--json`; `master --plan plan.json` runs an edited plan |
| `master verify <file>` | Pass or fail against a delivery spec | `--target`, `--tolerance 0.5LU`, `--json` |
| `master deliver <master>` | Write the deliverable set | `--formats wav24,wav16,mp3,flac,aac` (`wav24,wav16,mp3`), `--tags`, `--isrc`, `--loudness-sheet` |
| `master --stems` | Stem-aware mastering when only a mix exists | runs `separate --tier quality`, processes vocals, drums, bass, and other with per-stem chains, then sums; labeled best-effort |

**Automatic chain** (the default `master plan`, in order; each stage names the command it is built from)

1. Pre-flight: `dc-remove`, head and tail `silence trim` at -70 dB, `analyze problems`; clipping found means `declip` runs first, and the report says so.
2. Corrective EQ: `match-eq` toward the reference track or the genre curve at `--match-amount` (1/3-octave smoothing, never more than 4 dB per band), narrow resonances found by `analyze spectrum` tamed with a dynamic `eq` band, and a 20 Hz high-pass.
3. Dynamics: `multiband` with four bands at 1.5:1 to 2:1 and program-dependent release, then an optional single-band glue `compress --ratio 1.5:1 --attack 30ms` when the crest factor is above the genre target; transients are preserved by lookahead.
4. Stereo: `widen --mono-below 120Hz` to the target width, with a correlation guard that refuses to go below 0.3 broadband.
5. Color: `saturate --type tape` plus an exciter above 3 kHz at the `--warmth` amount.
6. Loudness: gain to the target, then `limit` with 4x true-peak oversampling at the target ceiling; `--intensity` bounds how much gain reduction the limiter may apply (light up to 2 dB, medium up to 4 dB, heavy up to 6 dB) and the report shows how much was used.
7. Output: TPDF noise-shaped dither when writing 16-bit, `master verify`, the before and after report, and the `--ab` pair.

**Delivery targets** (common platform practice; the table is data in the binary and `--target custom` overrides it)

| Target | Integrated loudness | True-peak ceiling | Notes |
| --- | --- | --- | --- |
| `streaming` | -14 LUFS | -1 dBTP | Spotify, YouTube, Tidal, Amazon normalize around this level |
| `apple` | -16 LUFS | -1 dBTP | Apple Music and Apple Podcasts guidance |
| `youtube` | -14 LUFS | -1 dBTP | same as streaming, kept as a named alias |
| `club` | -8 LUFS | -0.3 dBTP | DJ and club playback, no normalization |
| `cd` | -10 LUFS | -0.3 dBTP | 16-bit 44.1 kHz with dither |
| `broadcast` | -23 LUFS | -1 dBTP | EBU R128; `--standard atsc` switches to -24 LKFS and -2 dBTP |
| `podcast` | -16 LUFS stereo, -19 LUFS mono | -1 dBTP | speech chain adds `leveler` and `deess` |

**What `master analyze` reports** (JSON, and the same numbers an agent gets from `--report`)

- Loudness: integrated, short-term maximum, momentary maximum, loudness range, true peak, sample peak, crest factor, peak-to-loudness ratio.
- Tonal balance: energy per 1/3-octave band against the reference or genre curve, with the delta in dB and the three largest deviations named.
- Dynamics: gain-reduction headroom to the target, transient density, sections that are much louder or quieter than the whole.
- Stereo: width per band, L/R correlation broadband and below 120 Hz, side energy, mono-compatibility score.
- Problems: clipping, DC offset, inter-sample peaks, sub-30 Hz rumble, sibilance level, hum, noise floor, silence at head or tail, each with a suggested fix as a runnable `fx` stage.
- Context: tempo, key, duration, sample rate, bit depth, and whether the input already looks mastered (limited, loud, little headroom), in which case `master` defaults to `--intensity light` and says why.

**How an AI model masters a song with ferraus**

```bash
ferraus master analyze mix.wav --reference ref.wav --json > report.json   # read the numbers and the problem list
ferraus master plan mix.wav --target streaming --json > plan.json          # see the proposed chain, edit any value
ferraus master mix.wav --plan plan.json -o master_v1.wav --report r1.json  # run it
ferraus master verify master_v1.wav --target streaming --json             # pass or fail per spec
ferraus compare mix.wav master_v1.wav --json                              # confirm nothing was damaged
```

The loop repeats with an edited plan until `verify` passes and the report's deviations are within the model's own thresholds; the MCP prompt `master-song` encodes this sequence, and every suggested fix in the report is a stage the model can paste into the plan or run directly with `fx`. For a human, `ferraus master mix.wav` with no flags does the whole loop once and prints the report summary.

**Acceptance criteria**

- On a 12-song test set spanning the genre list, `master --target streaming` lands within 0.5 LU of -14 LUFS and never exceeds -1 dBTP measured at 4x oversampling.
- With `--reference` at `--match-amount 100%`, tonal balance is within 3 dB of the reference in every 1/3-octave band from 60 Hz to 12 kHz.
- Broadband L/R correlation stays at or above 0.3 and at or above 0.9 below 120 Hz on every test song.
- `master verify` flags each fault in a synthetic fault set (clipping, DC offset, a 2 dB true-peak overshoot, out-of-phase bass, 3 seconds of head silence) and passes every clean master.
- The MCP `master-song` loop completes on the test set without human input and ends with `verify` passing.
- Listening: on the release rubric, masters score at least 7 of 10 against commercial references from at least two of three listeners, and the `--ab` pair is loudness-matched within 0.2 LU so the comparison is fair.

## Agent interface

An agent should be able to install ferraus, ask it what it can do, call any capability with validated JSON, follow progress, and prove afterwards what was done; the four pieces below (schemas, MCP server, jobs, provenance) are generated from the same command definitions as the CLI so they cannot drift from it.

**Schema discovery**

- `ferraus schema` lists every command; `ferraus schema <command>` prints JSON Schema (draft 2020-12) for its arguments and its result envelope, derived from the clap definitions through `schemars`.
- `ferraus describe --agent` prints a compact catalog (one line per command with purpose, required inputs, and the three most useful flags), sized to fit in a model's context; `ferraus describe --agent --format markdown` is the same content as an `AGENTS.md`-style guide.
- The repository ships `AGENTS.md` and a `skills/ferraus/SKILL.md` so Claude Code and similar tools can load ferraus usage as a skill.

**MCP server**

- `ferraus mcp serve` runs an MCP server on stdio using the official Rust SDK (`rmcp`); `ferraus mcp serve --http 127.0.0.1:8765` runs the streamable-HTTP transport for multi-client use.
- Tools: one MCP tool per command (`ferraus_separate`, `ferraus_denoise`, `ferraus_tune`, …) with the same input schema as `ferraus schema`; results are the JSON envelope. Long operations stream MCP progress notifications and can be started as background jobs through `ferraus_job_start`, `ferraus_job_status`, `ferraus_job_result`, and `ferraus_job_cancel`.
- Resources: `ferraus://models`, `ferraus://plugins`, `ferraus://sounds`, `ferraus://presets/<effect>`, and `ferraus://jobs/<id>` expose registry and job state as readable resources.
- Prompts: a small set of workflow prompts (`clean-and-tune-vocal`, `extract-stems-for-remix`, `hum-to-instrument`) that chain tools with recommended defaults.
- Path sandbox: `--allow <dir>` (repeatable) restricts every read and write to the listed directories; the default when serving is the current working directory. Outputs default to a `ferraus-out/` folder under the first allowed directory so the agent never has to invent paths.
- Example Claude Code registration: `claude mcp add ferraus -- ferraus mcp serve --allow ~/music`.

**Jobs**

- Any command accepts `--async`, which returns `{"job_id": ...}` immediately and continues in a detached process. `ferraus jobs list|status|logs|wait|cancel|result <id>` manage them; job records live under the data directory and survive terminal closure.
- `--timeout 30m` and `--max-memory 2GB` bound a job; exceeding either cancels it with exit code 8 and a clear reason.

**Progress events** (`--progress json` on stderr, one JSON object per line)

```json
{"event": "stage_start", "stage": "separate", "index": 1, "of": 3}
{"event": "progress", "stage": "separate", "percent": 42.1, "eta_s": 31}
{"event": "output", "path": "song.vocals.wav", "role": "vocals"}
{"event": "warning", "code": "low_key_confidence", "message": "Key confidence 0.41; using chromatic scale"}
{"event": "done", "timing_ms": 48210}
```

**Provenance sidecar** (`<output>.ferraus.json`, written by default)

```json
{
  "ferraus_version": "1.0.0",
  "command": "tune",
  "argv": ["tune", "take.wav", "--key", "auto", "-o", "take.tuned.wav"],
  "params": {"key": "G major (detected, confidence 0.83)", "strength": "100%", "retune_speed": "25ms"},
  "inputs": [{"path": "take.wav", "sha256": "...", "duration_s": 182.4}],
  "outputs": [{"path": "take.tuned.wav", "sha256": "..."}],
  "models": [],
  "plugins": [],
  "host": {"os": "macos", "arch": "aarch64", "device": "cpu", "threads": 8},
  "deterministic": true,
  "timing_ms": 21304
}
```

`ferraus replay <sidecar>` re-runs the recorded command; `ferraus verify <sidecar>` checks that inputs and outputs still match their hashes. Because the sidecar records resolved parameters, an agent can read the key ferraus detected, the model it used, or the noise region it chose, and decide what to do next without re-running analysis.

**Agent ergonomics**

- Idempotent outputs: if the output path exists and its sidecar matches the requested command, inputs, and model versions, the command returns the existing result instantly (`--force` recomputes).
- Every error carries a `hint` that is a runnable command when one exists (`ferraus models pull separate/bs-roformer-4s`).
- `--explain` prints, for the resolved parameters, one line each on what the value does and why it was chosen, so an agent can justify choices to a user.
- No telemetry, no network calls except explicit downloads, and `--offline` makes that a hard guarantee.

**Library use.** The crates publish to crates.io with a stable `Pipeline` API in `ferraus-core`; a C FFI (`ferraus-ffi`) and a Python wheel (`pyferraus` via PyO3) are a later phase and are listed in the roadmap.

**Acceptance criteria**

- The MCP server passes the MCP Inspector conformance checks and a test asserts a one-to-one mapping between CLI commands and MCP tools.
- Every example invocation in this PRD validates against the published schemas (a doc-test extracts them).
- `replay` of any deterministic command reproduces the output bit-exactly on the same machine.
- An end-to-end agent test drives the whole "hum over street noise to mastered part" scenario from the Summary through MCP alone.

## Documentation

Documentation is a build artifact generated from the same command definitions as the CLI and the MCP tools, and every example in it runs in CI, so a model reading the docs sees exactly what the binary does; it is written in full depth for two readers, AI models and people, with nothing left to guesswork: every command, flag, unit, error, and workflow is documented, and every example runs.

**What ships**

| Artifact | Contents | Where |
| --- | --- | --- |
| Command reference | One page per command: synopsis, purpose, every flag with type, unit, default and range, at least three examples with the exact `--json` output, exit codes and error hints, the pipeline-file equivalent, related commands | docs site, `ferraus docs <command>` offline, `--help`, `--help --json`, man pages |
| `describe --agent` | The whole surface in under 4,000 tokens: one line per command with purpose, required inputs, and the three most useful flags | binary, `AGENTS.md`, `skills/ferraus/SKILL.md` |
| `llms.txt` and `llms-full.txt` | Site-level index for models, and the full reference concatenated, at the docs site root | `https://djynnius.github.io/ferraus/llms.txt` |
| Cookbook | Agent workflows end to end with commands and expected outputs: clean, tune, and master a vocal; stems for a remix; hum to an instrument; sheet music to a performance; batch podcast cleanup; FFmpeg equivalents | docs site |
| Concept guides | Units and parsing, pan laws, loudness standards, stems and tiers, notation and solfa, MCP setup, sound folders, provenance sidecars | docs site |
| Error catalog | Every error code with its cause and the runnable hint the binary prints | docs site and `ferraus docs errors` |
| Changelog and versioned docs | Conventional-commit changelog; docs for every released version | docs site |
| Getting started per platform | Install on macOS, Linux, and Windows, verify with ferraus doctor, first five commands with their output, where files and models live, how to uninstall | docs site |
| Tutorials for people | Step-by-step walkthroughs in plain language, one per major workflow: clean a podcast, tune a vocal, split a song into stems, turn a hum into an instrument part, make a score from a song, perform a sheet-music PDF, master a track, host a plugin; each states what to listen for at every step | docs site |
| Troubleshooting and FAQ | GPU setup, model downloads and offline use, plugins not found, FFmpeg missing, Windows PATH, slow runs, quality expectations per feature, how to report a bug with a sidecar | docs site and ferraus docs troubleshooting |
| Glossary | Every audio, MIDI, notation, and agent term used anywhere in the docs, defined in plain language with a link to where it is used | docs site |

**Rules the building agent follows**

- The doc comment on each clap command and flag is the only source of help text; the schema descriptions, MCP tool descriptions, man pages, and site pages are all generated from it, so nothing is written twice.
- Every example is a file under `docs/examples/<command>/` with a command line, the fixture it runs on, and the expected JSON; `cargo test --features doc-examples` runs all of them and compares the output, ignoring timing and hash fields.
- Parameters are documented with units and defaults, and examples come before prose; a page with no runnable example fails the docs build.
- `describe --agent` is regenerated and size-checked in CI; a command change that is not reflected in `AGENTS.md`, the skill, and the schema snapshots fails the PR.
- The site builds with mdBook from the repository and deploys on every tag; `ferraus docs` reads the same Markdown embedded in the binary so documentation works offline.
- Two voices on every command page, neither abbreviated: a plain-language explanation and walkthrough for people first, then a compact machine-oriented block for models (schema, defaults table, JSON example, exit codes).
- Depth bar: a command page is complete only when it documents every flag with unit, default, and range, every exit code the command can return, at least three runnable examples, and the common mistakes; a docs coverage check in CI lists any command or flag below that bar and fails the build.

## Model management

Models are versioned, hashed, licensed artifacts in a registry; the binary knows how to fetch, verify, cache, and report them, and nothing else about them is hard-coded.

**Commands**

| Command | Purpose |
| --- | --- |
| `models list` | Registry entries with name, version, task, size, license, cached or not, and which tier uses them |
| `models pull <name>[@version]` | Download with resume and SHA-256 verification; `--all-for separate` pulls every model a command's tiers need |
| `models rm <name>`, `models prune` | Remove a model or everything not used by any tier |
| `models verify` | Re-hash the cache |
| `models info <name>` | Spec, license text, source URL, benchmark numbers, export date |
| `models path <name>` | Print the cached file path (for embedding use) |
| `models registry update` | Refresh the manifest; `--pin <manifest.json>` freezes a registry snapshot for reproducible deployments |

**Registry manifest.** A signed JSON file (Ed25519 signature; the public key ships in the binary) hosted on GitHub Releases with Hugging Face Hub as a mirror. Each entry:

```json
{
  "name": "separate/bs-roformer-4s",
  "version": "2026.07",
  "task": "separate",
  "stems": ["vocals", "drums", "bass", "other"],
  "format": "onnx",
  "size_bytes": 612000000,
  "sha256": "...",
  "urls": ["https://github.com/.../releases/download/...", "https://huggingface.co/.../resolve/..."],
  "sample_rate": 44100,
  "chunk_seconds": 11.9,
  "overlap": 0.25,
  "license": {"spdx": "MIT", "url": "...", "commercial_use": true},
  "source": {"paper": "https://arxiv.org/abs/2309.02612", "weights": "...", "export_script": "tools/export/bs_roformer.py"},
  "benchmarks": {"multisong_sdr": {"vocals": 11.9}},
  "min_ferraus_version": "1.0.0"
}
```

**Cache layout.** `<cache>/ferraus/models/<task>/<name>/<version>/model.onnx` plus `spec.json`; `<cache>/ferraus/providers/` for execution-provider libraries; a `manifest.lock` records exactly which versions are present so `--offline` runs are fully described.

**Policies**

- Default tiers reference only models whose license permits redistribution and commercial use; models with research-only licenses are listed but flagged, excluded from tiers, and require `--model <name> --accept-license` on first use.
- Auto-download prints size and license before fetching; `-y` or `models.auto_download = true` in config skips the prompt, and `--offline` or `models.auto_download = false` turns a miss into exit code 5 with the `models pull` hint.
- Model updates never change behavior silently: a tier pins a version per ferraus release, `models registry update` can advertise newer versions, and switching requires `--model name@version` or a ferraus upgrade.
- Disk budget: `models.max_cache_gb` (default 20) triggers least-recently-used pruning with a warning.

**Acceptance criteria**

- A corrupted download (hash mismatch) is deleted and retried from the mirror, and a second mismatch returns exit code 6.
- `models pull` resumes after an interrupted transfer without re-downloading completed ranges.
- `--offline` with every model cached runs the full test suite with network access disabled in CI.

## Licensing and dependency strategy

The recommendation is Apache-2.0 OR MIT for every crate and the main binary, with the one GPL-tainted component (VST3 hosting) isolated in a separately licensed helper executable, and no FFmpeg code linked into ferraus at all. This keeps the tool embeddable in commercial products and agents while still reaching the VST3 catalog.

| Dependency | License | Strategy |
| --- | --- | --- |
| Symphonia (decoding) | MPL-2.0 | Link normally; file-level copyleft does not affect ferraus's license |
| ONNX Runtime via `ort` | MIT | Bundle the CPU runtime per platform; GPU providers downloaded on demand |
| NVIDIA CUDA and cuDNN runtime libraries | NVIDIA proprietary, redistributable under their EULA | Never in the base package; `accelerate install` fetches them and shows the EULA |
| FFmpeg | LGPL or GPL depending on build | Not linked. Optional bridge executes a user-installed binary; `doctor --install-ffmpeg` downloads a static build and shows its license |
| LAME (MP3 encoding) | LGPL-2.1 | Load `libmp3lame` dynamically at runtime when present; otherwise fall back to a pure-Rust encoder with a quality warning or the FFmpeg bridge |
| libopus, libvorbis, libFLAC bindings | BSD | Static link is fine |
| `deep_filter` (DeepFilterNet 3 runtime and weights) | MIT or Apache-2.0 | Feature-gated, in default builds |
| `nnnoiseless` (RNNoise) | BSD-3 | In default builds |
| Signalsmith Stretch (`ssstretch`) | MIT | Optional feature until the pure-Rust stretcher matches it |
| Rubber Band Library | GPL or commercial | Not used |
| CLAP ABI and `clack` | MIT | In default builds |
| VST3 SDK | GPLv3 or Steinberg proprietary agreement | Only in `ferraus-plughost-vst3`, a GPLv3 executable that talks to ferraus over IPC; the CLAP-only host stays MIT. Needs a legal review before 1.0 (see Open questions) |
| `oxisynth`, `sfizz` | MIT (verify), BSD-2 | SF2 in default builds; SFZ feature-gated |
| `midly`, `rmcp`, `clap`, `rustfft`, `rubato`, `ebur128` | MIT or Apache-2.0 | In default builds |
| Demucs v4 code and weights | MIT | Registry, default `fast` and `balanced` tiers |
| BS-RoFormer, Mel-Band RoFormer, SCNet code | MIT (lucidrains and reference implementations) | Export scripts in the companion repo |
| Community RoFormer and SCNet checkpoints | Varies; several have no explicit license or are non-commercial | License audit per checkpoint before registry entry; unlicensed ones excluded from tiers |
| Basic Pitch | Apache-2.0 | Registry |
| CREPE | MIT | Registry |
| RMVPE, SwiftF0 | To be verified | Registry only after audit |
| MUSDB18-HQ, other evaluation sets | Research-only | Used only in evaluation CI with the licensee's own copy; never redistributed |
| Soundfonts, sample libraries, plugins in the sounds registry | Per entry (MIT, CC0, CC BY, GPL-3.0, proprietary free) | License text shown before install; attribution requirements recorded in the manifest |

**Obligations the build must meet**

- `ferraus licenses` prints every third-party license and attribution, generated at build time with `cargo-about`; packages include the same text as `THIRD-PARTY-LICENSES.md`.
- A software bill of materials (CycloneDX) is attached to every release.
- The project name must be checked against crates.io, Homebrew, winget, PyPI, and trademark registries before 1.0; `ferraus` is the chosen name; the au stands for audio.

## Quality bar, testing, and evaluation

Every feature ships with a measurable acceptance test, and CI runs three layers: fast unit and golden-file tests on every push, model-parity and benchmark jobs on a schedule, and a listening checklist before each release.

**Layer 1: unit, property, and golden tests (every push, under 10 minutes)**

- Unit tests per crate; property tests (`proptest`) for the unit parser, pipeline executor, MIDI round-trip, and quantizer invariants.
- Golden audio fixtures: small synthetic signals (sines, chirps, clicks, noise bursts, a synthetic vocal with known pitch) generated by code, never binary files in git. Each effect has a golden output hashed in the repo with an RMS tolerance; a golden change requires a reviewed update with a rendered comparison.
- Null tests: every effect at `--mix 0%`, `fade` with zero lengths, `stretch --factor 1.0`, `denoise --reduction 0dB`, and `convert` WAV-to-WAV are identities.
- Decoder fuzzing with `cargo-fuzz` on the Symphonia entry points and the noise-profile and MIDI parsers; corpus kept in the repo.
- Snapshot tests of `--json` output and `schema` output so contract changes are visible in review.

**Layer 2: model parity and benchmarks (nightly and on model changes)**

- Parity: each ONNX export is compared with its PyTorch reference on fixed clips (max absolute error under 1e-4) using the companion repo's Python environment; CI caches the models.
- Separation benchmark: SDR per stem on MUSDB18-HQ test (self-hosted runner holding a licensed copy) and on the MVSEP Multisong public subset where permitted; results are posted as a table in the PR and compared against the tier targets in the separation section.
- Denoise benchmark: SI-SDR improvement on synthetic mixes and PESQ on VoiceBank-DEMAND for the model path.
- Pitch benchmark: raw pitch accuracy of trackers on a public vocal set; transcription F-measure on synthetic and recorded humming fixtures.
- Performance benchmark (`ferraus bench`): real-time factors per command on the reference CPU, with regressions over 15% failing the job.

**Layer 3: release checklist**

- Listening session on a fixed 12-clip set (pop vocal, rap, acoustic guitar, orchestral, podcast speech, phone recording, live band, EDM, jazz piano, choir, drum loop, humming) with a scored rubric per feature; scores are recorded in the release notes.
- Fresh-machine install test on each platform from the published package, running the Summary scenario end to end through the CLI and again through MCP.
- `--offline` full-suite run with network disabled.

**Determinism tests.** `--deterministic` runs of every command execute twice on CPU and must produce identical bytes; GPU runs must match CPU within 1e-3 RMS.

**Quality metrics reported to users.** `compare`, `analyze similarity`, and the per-stem `quality` estimate are first-class features, so the same metrics the project uses in CI are available to agents at run time.

## Packaging and release engineering

One release workflow, driven by [cargo-dist](https://github.com/axodotdev/cargo-dist) from a tagged commit, produces every artifact: archives, shell and PowerShell installers, a Homebrew formula, an MSI, `.deb` and `.rpm` packages, container images, and the signed model-registry snapshot.

**Builds run only on GitHub Actions**

- Every release binary, installer, package, container image, and docs deployment is produced by the GitHub Actions workflows in `djynnius/ferraus`, on GitHub-hosted runners, from a tagged commit. Nothing built on a developer's or an agent's machine is ever uploaded, signed, or published.
- The release workflow is the only path with permission to publish: to the GitHub Release, the Homebrew tap, the apt and dnf repositories, winget and scoop, `ghcr.io`, and crates.io. Branch protection and environment rules enforce it; a human approval gate sits in front of the publish job.
- Signing happens only inside the workflow: the Apple Developer ID and notarization credentials and the Windows Authenticode certificate live in GitHub environment secrets, and Linux packages and checksums are signed with keys held there too. No signing key exists on a laptop.
- Every artifact carries a GitHub artifact attestation, so `gh attestation verify` proves it came from this repository's workflow; the installers and the package publish steps refuse artifacts without one.
- Local `cargo build` and `cargo dist build` remain available for development and dry runs, but their outputs are marked unpublishable and the agent never runs the release or publish steps locally. Self-hosted runners are used only for the GPU benchmark jobs, never for release artifacts.
- Builds aim to be reproducible: pinned toolchain (`rust-toolchain.toml`), locked dependencies, vendored C sources where a dependency compiles C or C++, and `SOURCE_DATE_EPOCH` set from the tag, so two workflow runs of the same tag produce identical binaries.

**Build matrix**

| Target | Runner | Notes |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | Ubuntu 22.04 (glibc 2.35 floor) | `.deb` via `cargo-deb`, `.rpm` via `cargo-generate-rpm`; also a musl static build for the shell installer |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | macOS 14 | Universal binary optional; code-signed with a Developer ID certificate and notarized; Homebrew tap formula with bottles |
| `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` | Windows 2022 | MSI via `cargo-wix` (through cargo-dist), Authenticode-signed; winget and scoop manifests submitted per release |
| Container `linux/amd64`, `linux/arm64` | Ubuntu | `ghcr.io/djynnius/ferraus:<version>` (CPU) and `:<version>-cuda` |

**Install from a URL**

- cargo-dist generates `ferraus-installer.sh` and `ferraus-installer.ps1` for every release and attaches them to the GitHub Release, so `curl -fsSL https://github.com/djynnius/ferraus/releases/latest/download/ferraus-installer.sh \| sh` and the PowerShell equivalent always resolve to the newest version; `FERRAUS_VERSION=1.2.0` before the command pins one.
- `install.sh` at the repository root is a short shim that resolves the latest release and runs the generated installer, so `curl -fsSL https://raw.githubusercontent.com/djynnius/ferraus/main/install.sh \| sh` keeps working across versions and is the URL to print in documentation.
- The installer detects OS, architecture, and libc (static musl build on Linux when glibc is too old), downloads the matching archive, verifies its SHA-256 against the release's checksum file and, when `gh` is present, its GitHub artifact attestation, installs to `~/.ferraus/bin` (override with `FERRAUS_INSTALL_DIR`), and adds that directory to the shell profile unless `--no-modify-path` is given. It never asks for sudo. `ferraus self-uninstall` removes everything it placed.
- Both scripts are linted (`shellcheck`, `PSScriptAnalyzer`) and run on fresh VMs in the release workflow before the release is marked latest.

**Package identifiers**

| Channel | Identifier |
| --- | --- |
| Debian and Ubuntu | package `ferraus`; apt repository `https://djynnius.github.io/ferraus/apt` with a published signing key; `.deb` files also attached to each release |
| Fedora and RHEL | package `ferraus`; dnf repository `https://djynnius.github.io/ferraus/rpm`; `.rpm` files attached to each release |
| Homebrew | tap `djynnius/homebrew-tap`, formula `ferraus`, bottles for Apple Silicon and Intel |
| Windows | `winget install djynnius.ferraus`; scoop bucket `djynnius/scoop-bucket`; signed `ferraus-<version>-x86_64.msi` |
| Containers | `ghcr.io/djynnius/ferraus:<version>` and `:<version>-cuda`, both including FFmpeg |
| Full variants | `ferraus-full` packages for deb, rpm, and Homebrew bundle a static FFmpeg so the bridge works offline from the first run |

**What goes in the package**

- `ferraus` (CLI), `ferraus-plughost` (CLAP host, MIT) and `ferraus-plughost-vst3` (GPLv3), the ONNX Runtime CPU shared library for the platform, `THIRD-PARTY-LICENSES.md`, shell completions (bash, zsh, fish, PowerShell), and man pages.
- No model weights, no GPU libraries, no soundfonts. Target package size: under 60 MB; `ferraus doctor` reports what is present and what `accelerate install`, `models pull`, or `sounds install` would add.

**Per-platform details**

- Debian/Ubuntu: `.deb` depends only on glibc and libstdc++; an apt repository under the project domain with a signing key; `apt install ferraus`.
- Fedora/RHEL: `.rpm` with the same dependency floor; a dnf repository; Copr as a secondary channel.
- macOS: `brew install djynnius/tap/ferraus` from day one, with a submission to homebrew-core once the project meets its notability bar; the formula installs completions and man pages; notarization avoids Gatekeeper prompts.
- Windows: `winget install djynnius.ferraus` and `scoop install ferraus` resolve to the signed MSI; the installer adds `ferraus` to `PATH` for the current user and never requires administrator rights for the per-user install.
- From source: `cargo install ferraus-cli` builds the CPU variant; features `cuda`, `sfz`, `mp3-lame`, `vst3` opt into the heavier dependencies.

**Release process**

1. Tag `vX.Y.Z` on `main`; cargo-dist's generated GitHub Actions workflow builds every target, runs the Layer 1 test suite on each, signs, and uploads to the GitHub Release with checksums and an attestation.
2. The same workflow publishes the Homebrew formula to the tap, pushes container images, and opens the winget and scoop PRs.
3. The model registry manifest for that version is pinned, signed, and attached to the release.
4. Release notes are generated from conventional commits plus the listening-session scores and benchmark table.
5. Semantic versioning applies to the CLI contract: a breaking change to any `--json` schema or exit code is a major version; new flags and commands are minor; `schema_version` in every envelope increments on breaking changes.

**Update checks.** `ferraus --version` is local only; `ferraus self-update` (via the installer's updater) is opt-in and never runs automatically.

**Acceptance criteria**

- A fresh VM per platform installs the published package and completes `ferraus doctor`, `ferraus convert`, and `ferraus separate --tier fast` without any other software present.
- Package sizes stay under 60 MB and the install leaves no files outside the package's own directories, the per-user config and cache paths.
- The release workflow completes in under 60 minutes from tag to published artifacts.
- No release artifact is produced or published outside GitHub Actions: the publish job fails when an artifact lacks a workflow attestation, and a release built locally cannot be uploaded even with maintainer credentials.

## Roadmap and milestones

The build order puts packaging and the agent contract first, so every later milestone ships through real installers and is usable by agents the day it lands; the ML-heavy features follow once the runtime and registry exist, and plugin hosting comes last because it depends on the most external pieces.

&#91;embedded content: roadmap · 10 milestones in 3 phases, 5 gates\]

Milestones run top to bottom in three phases; the gates on the right are the checks that must pass before the next milestone starts.

1. **M0 Scaffold and release pipeline.** Workspace, CI matrix, clap command tree with the JSON envelope, `schema`, unit parser, `AudioBuffer` and pipeline executor, WAV and FLAC codecs, `info` and `convert`, and the full cargo-dist release, run only on GitHub Actions, producing the one-line URL installers, `.deb`, `.rpm`, Homebrew, and MSI. Done when a fresh VM on each OS installs the package and `ferraus info` works, and schema snapshot tests pass. Gate G1: installers on all three platforms.
2. **M1 Core editing.** Every command in the core section, all native codecs, the FFmpeg bridge with ffmpeg pass-through and the media commands, sidecars, batch mode, `--in-place`. Done when the core acceptance criteria pass.
3. **M2 Effects, dynamics, and chains.** Every effect in the effects and restoration sections, panning and envelopes, the master command, presets, `fx` chains, pipeline files with caching, `mix`. Done when the effects, restoration, mastering, and pipeline acceptance criteria pass. Gate G2: the `--json` contract and exit codes are frozen at `schema_version` 1.
4. **M3 Agent interface.** MCP server over stdio and HTTP, jobs, `replay` and `verify`, `describe --agent`, `AGENTS.md`, the Claude skill, path sandbox. Done when the MCP conformance and one-to-one mapping tests pass and an agent completes an end-to-end editing task through MCP alone.
5. **M4 Noise reduction.** Profile engine, auto region, residual output, DeepFilterNet and RNNoise paths, hybrid mode. Done when the denoise acceptance criteria pass.
6. **M5 ML runtime, registry, and separation.** `ferraus-ml`, model registry and `models` commands, execution providers on all three OS, Demucs export for `fast` and `balanced`, then RoFormer and SCNet exports for `quality`, ensembles for `max`, vocal and drum splits, extended stems. Done when the separation acceptance criteria pass and GPU runs match CPU. Gate G3: model license audit complete for every registry entry.
7. **M6 Pitch and time.** pYIN, key detection, `tune`, `pitch`, `stretch`, `analyze pitch|key|tempo`, model trackers. Done when the pitch acceptance criteria pass.
8. **M7 MIDI, scores, and sheet music.** MIDI commands and JSON round-trip, quantize and groove, `transcribe` (mono and Basic Pitch), `render` with SF2, `hum`, score output from audio or MIDI (lead sheet, grand staff, drum score, solfa), sheet-music PDF reading, and score playback. Done when the MIDI and score acceptance criteria pass.
9. **M8 Plugin hosting and sounds.** CLAP host with process isolation, VST3 host binary, `plugin` commands, sounds registry and first catalog, `render` through plugins, SFZ via sfizz. Done when the plugin acceptance criteria pass on all three OS. Gate G4: legal review of the VST3 host licensing.
10. **M9 1.0 hardening.** Listening sessions, performance pass to meet the hardware-tier targets, documentation site, Homebrew core and winget submissions, `self-update`. Gate G5: release checklist passes on all platforms.

**After 1.0 (candidates, in rough priority):** Python wheel and C FFI; Audio Units and LV2 hosting; music-aware denoise and de-reverb models; an ML beat tracker and better chord detection; a real-time monitoring mode (play a file through an effect chain live); a training and fine-tuning harness in the companion repo so the project can produce its own permissively licensed separation checkpoints; multi-language subtitle-style lyric alignment.

**How the building agent should work**

- One milestone per branch, landed in reviewable PRs of a few hundred lines each, with the acceptance tests for that milestone written before the implementation.
- Every PR runs the Layer 1 suite; model-related PRs also run Layer 2 on the self-hosted runner.
- The agent keeps `AGENTS.md`, `ferraus describe --agent`, and the schema snapshots in sync in the same PR as any command change.
- When a decision in Open questions blocks a milestone, the agent implements the recommended default behind a feature flag and records the decision in `docs/decisions/` as an ADR.
- The agent never builds release binaries, installers, or packages on its own machine; it tags a commit and lets the GitHub Actions release workflow produce, sign, attest, and publish every artifact (see Packaging and release engineering).

## Open questions and decisions needed

Each question below has a recommended default so the building agent is never blocked; a different answer from you changes the plan as noted.

| # | Question | Recommended default | What changes if you decide otherwise |
| --- | --- | --- | --- |
| 1 | Project name and CLI binary name | Keep `ferraus` as the name (the au is for audio); a quick search found no existing crate, package, or project called ferraus, so register the crates.io names (ferraus-cli, ferraus-core, and so on), the GitHub org, and the Homebrew and winget identifiers before M0 ends | Rename is a global search-and-replace plus package IDs; do it before any package is published |
| 2 | Project license | Apache-2.0 OR MIT for all crates; VST3 host executable under GPLv3 | A single GPLv3 license simplifies VST3 but blocks embedding in proprietary agents and products |
| 3 | Commercial intent | Assume the tool may be used commercially, so only commercial-use model weights enter default tiers | If strictly non-commercial, several stronger community RoFormer checkpoints become eligible for the `quality` tier immediately |
| 4 | FFmpeg | Native codecs by default plus a first-class FFmpeg bridge (doctor --install-ffmpeg, ferraus ffmpeg pass-through, media commands, ferraus-full packages) so every FFmpeg function stays available for the v2 video work | Bundling FFmpeg in every package rather than only the full variants adds LGPL/GPL binaries and 80 MB or more to each download |
| 5 | GPU scope for 1.0 | CUDA (Linux, Windows), CoreML (macOS), DirectML (Windows) via downloadable providers | CPU-only 1.0 cuts a milestone of work but makes `quality` separation a 15-minute wait |
| 6 | Time-stretch engine | Pure-Rust hybrid stretcher as default, Signalsmith binding as the optional `high` mode | Dropping the binding keeps the build pure Rust at some quality cost on extreme stretches |
| 7 | MP3 encoding | Dynamic load of system LAME, pure-Rust fallback | Static LAME would need LGPL relink obligations in every package |
| 8 | Model hosting | GitHub Releases primary, Hugging Face mirror, Ed25519-signed manifest | A project CDN adds cost but allows bandwidth control |
| 9 | Plugin formats at 1.0 | CLAP and VST3; AU and LV2 after 1.0 | Adding AU at 1.0 adds macOS-only work and Objective-C bindings |
| 10 | Extended stems (strings, keys, brass) | Ship as best-effort cascades with quality flags, only where a licensed model exists | Omitting them keeps the 1.0 surface honest but drops a requested feature |
| 11 | Interpretation of "compress (clip high frequencies, tighter)" | A true dynamics compressor plus a `tighten` preset combining compression, de-essing, high-pass, and optional low-pass | If you meant a specific sound, a reference clip would let the preset be tuned to it |
| 12 | Python bindings | After 1.0 | Including them in 1.0 adds a maturin build matrix to every release |
| 13 | Real-time monitoring (play through effects live) | After 1.0 | A 1.0 live mode would require a real-time audio thread (`cpal`) and changes to every streaming effect's latency handling |
| 14 | Companion training repo | Export and evaluation scripts only in v1 | Committing to training lets the project own its separation weights but needs GPU budget and datasets |
| 15 | Telemetry | None, ever | An opt-in crash reporter could be added later with explicit consent |
| 16 | Score engraving engine | Verovio (LGPL-3.0) isolated in a ferraus-engrave helper executable, like the VST3 host; LilyPond as an optional bridge | Linking Verovio into the main binary needs LGPL relink obligations; dropping it means PDF and SVG scores only through LilyPond |
| 17 | Multi-instrument transcription model | YourMT3+ after the license audit; MuScriptor only if its gated license allows redistribution; Basic Pitch and the native trackers otherwise | Without a multi-instrument model, full-band scores rely on separation plus per-stem transcription, which is slower but fully permissive |

**Information that would sharpen the spec**

- A few reference recordings for the target use cases (a noisy vocal take, a hum, a full mix to split) so default parameters and the listening rubric are tuned to your material.
- Which agent runtimes matter most (Claude Code, a custom agent framework, both) to prioritize the MCP transport and skill packaging.
- Any instruments or sample libraries you already use, to seed the sounds registry aliases.
- A few reference masters you consider production-ready, to calibrate the genre curves and the listening rubric for `master`.
- The folder layout of your own VST and sound libraries, to test `--sound-path` resolution against real names.

## Appendix: command reference, config, glossary

**Command reference (v1 surface)**

| Group | Commands |
| --- | --- |
| Inspect | `info`, `analyze tempo\|key\|loudness\|silence\|pitch\|spectrum\|clipping\|chords\|similarity`, `compare`, `waveform`, `spectrogram` |
| Edit | `convert`, `trim`, `cut`, `split`, `concat`, `resample`, `gain`, `normalize`, `channels`, `silence detect\|trim\|pad`, `tags`, `mix`, `reverse`, `speed` |
| Dynamics | `compress`, `limit`, `gate`, `expand`, `deess`, `multiband`, `transient`, `clip` |
| Tone | `eq`, `highpass`, `lowpass`, `tilt`, `match-eq`, `saturate`, `bitcrush`, `dc-remove`, `invert-phase` |
| Space and time | `reverb`, `delay`, `chorus`, `flanger`, `phaser`, `tremolo`, `vibrato`, `widen`, `pan`, `fade` |
| Clean | `denoise`, `denoise profile`, `denoise profile merge` |
| Separate | `separate` (with `--vocal-split`, `--drum-split`, `--extended`, `--instrumental`) |
| Pitch | `tune`, `pitch`, `stretch`, `vocoder` |
| MIDI | `midi info\|to-json\|from-json\|quantize\|groove\|transpose\|velocity\|legato\|tempo\|split-tracks\|merge\|trim`, `transcribe`, `render`, `hum` |
| Chains | `fx`, `pipeline run\|validate\|explain`, `presets list\|show\|save` |
| Plugins and sounds | `plugin scan\|list\|info\|run\|presets\|state`, `sounds search\|install\|list\|info\|remove\|update\|alias` |
| Models and system | `models list\|pull\|rm\|prune\|verify\|info\|path\|registry`, `accelerate install\|status`, `doctor`, `licenses`, `self-update` |
| Agents | `schema`, `describe --agent`, `mcp serve`, `jobs list\|status\|logs\|wait\|cancel\|result`, `replay`, `verify` |
| Restoration | declip, dehum, declick, dehiss, denoise --adaptive, repair, remove-sound, phase-align, center, leveler, enhance-voice, enhance, envelope, pitch-bend, graphic-eq, fft-filter, notch, filter, dynamics, echo, distort, doppler, guitar, generate, band-split, analyze problems\|stats |
| Panning | pan, autopan, balance, channels mix, mix --pans, widen |
| Mastering | master, master analyze\|plan\|verify\|deliver, master --stems |
| Scores | score (audio or MIDI to notation), score read (PDF or image OMR), score play, score perform, score convert, score to-json\|from-json, score check |
| Media bridge | ffmpeg (pass-through), ffprobe, media info\|streams\|extract-audio\|replace-audio\|remux, doctor --install-ffmpeg; video namespace reserved for v2 |
| Docs | docs \<command>, docs errors, describe --agent, --help --json, self-uninstall |

**Config file** (`config.toml`, every key optional)

```toml
[output]
default_format = "wav"        # when no extension is given
bit_depth = 24
sidecar = true

[models]
auto_download = true           # false turns a miss into exit code 5
max_cache_gb = 20
registry_url = "https://github.com/djynnius/ferraus-models/releases/latest/download/registry.json"

[compute]
device = "auto"               # auto | cpu | cuda | coreml | directml
threads = 0                    # 0 = all cores
max_memory = "2GB"
deterministic = false

[plugins]
extra_paths = []
watchdog_seconds = 60

[presets]
# per-effect overrides of built-in defaults
compress = { threshold = "-16dB", ratio = "3:1" }

[mcp]
allow = ["~/music"]
```

**Glossary**

| Term | Meaning |
| --- | --- |
| SDR | Signal-to-distortion ratio in dB; the standard separation quality metric (higher is better) |
| SI-SDR | Scale-invariant SDR, used for denoising evaluation |
| LUFS, LU | Loudness units per EBU R128; streaming services target about -14 LUFS integrated |
| dBTP | Decibels true peak, measured after oversampling |
| Stem | One instrument group separated from a mix (vocals, drums, bass, other, …) |
| Noise profile | Per-frequency statistics of a noise-only region used by spectral gating |
| Spectral gating | Attenuating time-frequency bins that fall below a noise threshold |
| TD-PSOLA | Time-domain pitch-synchronous overlap-add; pitch shifting that preserves formants |
| Formant | Resonance peak of the vocal tract; keeping formants fixed keeps a voice sounding like itself |
| Retune speed | How fast pitch correction pulls toward the target note; 0 ms is the robotic effect |
| Quantize | Moving MIDI note timings onto a rhythmic grid |
| Groove | A template of timing and velocity offsets per grid position |
| CLAP, VST3, AU, LV2 | Audio plugin formats; CLAP and VST3 are supported in v1 |
| SF2, SFZ | Sample-based instrument formats (SoundFont 2 and SFZ text format) |
| Execution provider | An ONNX Runtime backend (CPU, CUDA, CoreML, DirectML) |
| MCP | Model Context Protocol; the interface through which agents call ferraus as tools |
| Sidecar | The `.ferraus.json` provenance file written beside each output |
| Mid/side | A stereo signal expressed as center (mid) and difference (side) parts; the basis of widening and center extraction |
| Mastering | Final processing of a mix for delivery: tonal balance, dynamics, stereo image, loudness, and format |
| OMR | Optical music recognition: reading printed or scanned sheet music into a machine-readable score |
| MusicXML | The open interchange format for notation; ferraus's hub format between audio, MIDI, and engraved scores |
| Lead sheet | Melody on a treble staff with chord symbols above and lyrics below |
| Solfa | Tonic sol-fa notation (do re mi, written d r m f s l t) relative to the key; readable without a staff |
