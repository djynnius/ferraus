# Troubleshooting and FAQ

Each entry says what you see, why it happens, and the exact command that fixes it. Add
`--json` to any command to get the same information as structured data, including an
`error.hint` that is a runnable command.

## Installation

**`ferraus: command not found` after the URL installer.** The installer puts the binary in
`~/.ferraus/bin` and adds it to your shell profile. Open a new terminal, or run
`export PATH="$HOME/.ferraus/bin:$PATH"`. On Windows, open a new PowerShell window; the
installer adds the folder to your user `PATH`.

**Windows says the publisher is unknown.** Release binaries and the MSI are Authenticode
signed by the GitHub Actions release workflow. Builds made with `cargo install` are not
signed; that warning is expected for them.

**Check what is installed.** `ferraus doctor` prints the version, config/cache/data paths,
FFmpeg status, cached models, and the active compute device.

## Models and offline use

**Exit code 5: model not cached.** Downloads are off for this run (`--offline`, or
`models.auto_download = false` in the config). Run the command in the hint, for example
`ferraus models pull separate/htdemucs`, then re-run. `ferraus models list` shows what each
tier needs; `ferraus models pull --all-for separate` fetches all of them.

**Exit code 6: download or verification failed.** Downloads resume; run the command again.
A second checksum mismatch means a mirror is serving a corrupted file — report it.

**Working with no network.** Pull the models you need while online, then pass `--offline`
(or set `FERRAUS_OFFLINE=1`): any attempt to use the network becomes an error instead of a
download.

## GPU setup

**Inference runs on the CPU.** The base package is CPU-only. `ferraus accelerate install`
downloads the execution-provider libraries for your platform (CUDA on Linux/Windows with an
NVIDIA GPU; CoreML is built in on macOS; DirectML on Windows). `ferraus doctor` shows the
active provider; `--device cpu|cuda|coreml|directml` forces one per run. GPU results match
CPU results within 1e-3 RMS.

## Formats and FFmpeg

**Exit code 4: unsupported format.** Native decoding covers WAV, AIFF, FLAC, MP3, AAC/M4A,
ALAC, Vorbis, Opus, and the audio track of MP4/MKV/WebM/MOV. Encoding AAC/ALAC (`.m4a`),
exotic containers, and writing video need the FFmpeg bridge: `ferraus doctor --install-ffmpeg`
installs a pinned static build into the ferraus cache, or set `ffmpeg.path` in the config.

**MP3 encoding is unavailable.** MP3 encoding loads the system LAME library at run time.
Install it (`brew install lame`, `apt install libmp3lame0`) or install the FFmpeg bridge.

## Plugins and sounds

**`plugin not found` (exit code 7).** Run `ferraus plugin scan` (add `--paths <dir>` for
non-standard locations) and check `ferraus plugin list`. Folders passed with `--sound-path`
or `FERRAUS_SOUND_PATH` are scanned too.

**A plugin crashed.** Plugins run in a separate `ferraus-plughost` process, so ferraus keeps
running; the error names the plugin and its path. Try another preset or report it to the
plugin vendor.

**An instrument name is ambiguous.** The error lists the candidates with a runnable hint for
each. Pin one with `ferraus sounds alias piano=<id>`.

## Output files

**`output_exists` (exit code 2).** ferraus never overwrites a file unless you pass
`--overwrite` (or `--in-place`, which keeps `<input>.bak`).

**The command returned instantly with `reused_output`.** An identical output from the same
command, parameters, inputs, and ferraus version already exists, so the existing file was
returned. Pass `--force` to recompute.

## Slow runs

- Separation `quality` and `max` tiers are slow on CPU (minutes per song). Use
  `--tier fast` for previews or install a GPU provider.
- `--threads N` caps CPU use; `--deterministic` fixes the thread count and may be slower.
- `ferraus bench` reports real-time factors on your machine.

## Quality expectations per feature

| Feature | Expectation |
| --- | --- |
| Editing, effects, loudness | Studio quality; deterministic and bit-exact on CPU |
| Profile denoise | Excellent on steady noise (fans, hiss, hum); use `--mode ml` for speech without a noise sample |
| Stems: vocals, drums, bass | `quality` tier beats Demucs v4 by 2 dB+ on vocals |
| Stems: piano, guitar, strings | Best-effort; each output carries a quality flag |
| Pitch correction | Natural on clean monophonic vocals; `--tracker crepe` helps on breathy or noisy takes |
| Hum-to-MIDI | ~85%+ note accuracy on clean humming; polyphonic transcription is best-effort |
| Sheet music reading | Good on clean printed scores; handwritten scores are best-effort |

## Reporting a bug

Attach the output's sidecar (`<output>.ferraus.json`): it records the exact command,
resolved parameters, model hashes, ferraus version, and host. `ferraus verify <sidecar>`
checks that the files still match. Open an issue at
https://github.com/djynnius/ferraus/issues.
