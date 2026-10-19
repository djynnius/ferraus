# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/)
and the project uses [Semantic Versioning](https://semver.org/) for its CLI contract: a breaking
change to any `--json` schema or exit code is a major version.

## [Unreleased] — 0.1.0

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
  resumable verified downloads, Demucs `fast` tier, ensembles, TTA, sum-to-mix, cascades.
- Pitch and time: pYIN, CREPE, SwiftF0, key detection, tune, pitch, stretch.
- MIDI: lossless JSON model, quantize, groove, editing utilities, transcribe (mono and Basic
  Pitch polyphonic), hum, render through the built-in synth, SF2, SFZ, or CLAP/VST3 plugins.
- Scores: lead sheets, grand staff, drum scores, solfa; MusicXML/ABC/LilyPond/MEI writers and
  readers; score play and perform; Verovio engraving helper; Audiveris bridge.
- Plugins: out-of-process CLAP and VST3 hosting with a watchdog, sounds registry.
- Mastering: master, master analyze/plan/verify/deliver.
- Agents: JSON envelope and schemas, MCP server (stdio and HTTP) with jobs, resources, and
  prompts, `describe --agent`, AGENTS.md and a Claude skill.
- Release engineering: cargo-dist workflow (shell/PowerShell installers, Homebrew, MSI),
  `.deb`/`.rpm` jobs, containers, publish job behind an approval gate, docs site from op specs.
