# Troubleshooting and FAQ

Each entry says what you see, why it happens, and the exact command that fixes it. Add
`--json` to any command to get the same information as structured data, including an
`error.hint` that is a runnable command.

## Installation

**`ferraus: command not found` after the URL installer.** The installer puts the binary in
`~/.ferraus/bin` and adds it to your shell profile. Open a new terminal, or run
`export PATH="$HOME/.ferraus/bin:$PATH"`. On Windows, open a new PowerShell window; the
installer adds the folder to your user `PATH`.

**`ferraus: command not found` after `cargo install ferraus`.** Cargo installs to
`~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows). rustup adds it to `PATH`; if you
installed Rust another way, add it yourself. Note the package name: `cargo install ferraus`,
not `ferraus-cli`.

**Windows says the publisher is unknown.** Release binaries and the MSI are Authenticode
signed by the GitHub Actions release workflow. Builds made with `cargo install` are not
signed; that warning is expected for them.

**Check what is installed.** `ferraus doctor` prints the version, config/cache/data paths,
FFmpeg status, cached models, and the active compute device.

## Models and offline use

**Exit code 5: model not cached.** ferraus downloads a model only when you allow it: add
`-y` to the command (or set `models.auto_download = true` in the config), or run the command in
the hint, for example `ferraus models pull separate/htdemucs`, then re-run. `ferraus models list`
shows every model, its size and license, and which tiers use it.

**Exit code 6 with `model_unverified`.** The model's registry entry has no recorded SHA-256
(or, for some entries, no download at all), so ferraus refuses to fetch it. In this release that
affects the `balanced`, `quality`, and `max` separation tiers, 6-stem separation, the full-size
CREPE model, and the homr sheet-music transformer (the builtin `score read` engine does not
need it); see
[Stems and separation tiers](concepts/stems.md). `separate` with the default `--tier auto` uses the
best verified tier (`fast` today). You can override
the check for an entry that does have a download with the variable shown in the hint
(`FERRAUS_ALLOW_UNVERIFIED=1`), at your own risk.

**Exit code 6: download or verification failed.** Downloads resume; run the command again.
A second checksum mismatch means a mirror is serving a corrupted file; report it.

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
returned. Pass `--force --overwrite` to recompute (`--force` alone stops at the existing file).

**A batch fails with `unsupported_format` on `.ferraus.json` files.** A glob such as
`"takes/*"` also matches the sidecars ferraus wrote. Name the extension: `"takes/*.wav"`.

**Clipping after a chain of separate commands.** Each command writes an integer file (the
input's bit depth), so a stage that adds gain (`leveler`, `enhance-voice`, `eq` boosts) can push
peaks past 0 dBFS and clip in the file it writes. `analyze stats` shows `full_scale_samples`.
Run the stages in one `ferraus fx` pass (floating point inside) with `normalize` or `limit` last,
or write intermediates as 32-bit float with `--bit-depth 32`.

**`engraver_missing` when writing PDF, PNG, or SVG scores.** Engraving needs the
`ferraus-engrave` helper (in the release packages; from source
`cargo build -p ferraus-engrave --release`, then put it beside `ferraus` or set
`FERRAUS_ENGRAVE`) or LilyPond. MusicXML, ABC, LilyPond, MEI, and sol-fa outputs need nothing.

## Sheet music recognition

**`pdf_renderer_missing` (exit code 4).** PDFs are rasterized with `pdftoppm` (poppler) or
`mutool` (MuPDF); install one (`brew install poppler`, `apt install poppler-utils`), or export
the pages as PNG and pass those. On macOS without either, `sips` reads only the first page.

**`no staves were recognized`.** The page needs staff lines at least about 8 px apart after
scanning; scan at 300 dpi, crop away dark borders, and straighten the page. Photos taken at an
angle, handwritten music, and very faint copies are best-effort.

**Wrong notes or durations.** Run `score read page.png -o out.musicxml --review`: measures
whose durations do not add up are in `failing_measures`, uncertain ones in
`review.low_confidence` with the reason. Fix them with `--fix edits.json` or `score from-json`.
To see what the recognizer saw, set `FERRAUS_OMR_DEBUG=<dir>`: it writes the network's maps
and the boxes of every symbol it read as PNGs (`FERRAUS_OMR_TRACE=1` prints its decisions).

**Slow.** Recognition takes roughly 15–60 s per page on a laptop CPU (two networks over
overlapping tiles); blank areas are skipped. `--device cuda` uses a GPU when one is set up.

## Slow runs

- Separation is the slowest command: the `fast` tier runs at about half real time on a
  laptop CPU. Run it with `--async` and `ferraus jobs wait`, or install a GPU provider.
- `--threads N` caps CPU use; `--deterministic` fixes the thread count and may be slower.
- `ferraus bench` reports real-time factors on your machine.

## Quality expectations per feature

| Feature | Expectation |
| --- | --- |
| Editing, effects, loudness | Studio quality; deterministic and bit-exact on CPU |
| Profile denoise | Excellent on steady noise (fans, hiss, hum); use `--mode ml` for speech without a noise sample |
| Stems: vocals, drums, bass, other | `fast` (Demucs v4) is good for practice and remix sketches, with some bleed; `quality` (the default) adds a RoFormer vocal model, about 3 dB cleaner vocals; see [measured quality](concepts/stems.md) |
| Stems: piano, guitar, strings | Best-effort; each output carries a quality flag |
| Pitch correction | Natural on clean monophonic vocals; `--tracker crepe` or `swiftf0` can help on breathy or noisy takes |
| Hum-to-MIDI | Accurate on clean, separated notes; raise `--onset-sensitivity` for repeated notes |
| Polyphonic transcription (`--mode poly`) | Best-effort; check the per-note confidence |
| Score from MIDI | Reliable; from audio, as good as the transcription |
| Sheet music reading (`score read`) | Clean printed scores: all 975 notes of the engraved test set read right (PNG and PDF); scans are best-effort; one voice per staff; lyrics, tuplets, ties, slurs, dynamics, and repeats are not read; check `--review` before trusting a page |

## Reporting a bug

Attach the output's sidecar (`<output>.ferraus.json`) and the output of `ferraus doctor`: the sidecar records the exact command,
resolved parameters, model hashes, ferraus version, and host. `ferraus verify <sidecar>`
checks that the files still match. Open an issue at
https://github.com/djynnius/ferraus/issues.
