# Getting started

## Install

**From source (any platform with Rust).** Install a Rust toolchain from <https://rustup.rs>,
then:

```bash
cargo install ferraus
```

The Cargo package that builds the `ferraus` binary is named `ferraus` (its directory in the
repository is `crates/ferraus-cli`; see `docs/decisions/0005-binary-package-name.md`). From a
clone of the repository, `cargo install --path crates/ferraus-cli` does the same. Either way
the binary goes to `~/.cargo/bin`, which rustup puts on your `PATH`.

**Release installers.** Releases are built and attested by the repository's GitHub Actions
workflows and published on the [releases page](https://github.com/djynnius/ferraus/releases).
Release builds exist for macOS on Apple silicon (macOS 13.4+), Linux with glibc 2.38 or newer
(Ubuntu 24.04+, Debian 13+, Fedora 39+) on x86-64 and ARM64, and Windows on x86-64 (Windows 11
on ARM runs it under its built-in x64 emulation) with the Microsoft Visual C++ Redistributable,
which most Windows machines already have (<https://aka.ms/vs/17/release/vc_redist.x64.exe>).
Intel Macs, older Linux distributions, and musl-based systems such as Alpine have no release
build, because the ONNX Runtime libraries ferraus links have none.

macOS and Linux:

```bash
curl -fsSL https://github.com/djynnius/ferraus/releases/latest/download/ferraus-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/djynnius/ferraus/releases/latest/download/ferraus-installer.ps1 | iex"
```

The installers put `ferraus` in `~/.ferraus/bin` (set `FERRAUS_INSTALL_DIR` to change it) and
never ask for administrator rights. Open a new terminal afterwards. Packages are also published
for Homebrew (`brew install djynnius/tap/ferraus`), Debian and Ubuntu
(`sudo apt install ./ferraus_<version>_amd64.deb`), Fedora and RHEL
(`sudo dnf install ./ferraus-<version>.x86_64.rpm`), and Windows (the MSI from the releases
page). winget, scoop, and apt/dnf repositories follow in a later release.

## Check the installation

```bash
ferraus doctor
```

```json
{
  "ferraus_version": "0.1.0",
  "platform": {"os": "macos", "arch": "aarch64"},
  "paths": {
    "config": "/Users/me/Library/Application Support/ferraus/config.toml",
    "cache": "/Users/me/Library/Caches/ferraus",
    "data": "/Users/me/Library/Application Support/ferraus/data",
    "models": "/Users/me/Library/Caches/ferraus/models",
    "sounds": "/Users/me/Library/Application Support/ferraus/data/sounds"
  },
  "ffmpeg": {"found": true, "path": "/opt/homebrew/bin/ffmpeg", "source": "path", ...},
  "models": {"cache_bytes": 0},
  "compute": {"providers": ["coreml", "cpu"], "active": "coreml", ...},
  "plughost": {"clap_host": ".../ferraus-plughost"},
  "engraver": {"helper": null, "lilypond": null},
  "suggestions": []
}
```

`doctor` reports the version, where config, models, and sounds live, whether FFmpeg was found
(needed only for a few formats, see [Troubleshooting](troubleshooting.md)), the compute
devices for neural models, the plugin host, and the score engraver. `suggestions` lists
runnable fixes for anything missing.

## Your first five commands

```bash
ferraus info song.mp3                          # what is in this file?
ferraus convert song.mp3 -o song.flac          # lossless copy
ferraus normalize song.flac --lufs -14 -o song.loud.flac
ferraus trim song.flac --range 0:30..1:00 -o chorus.flac
ferraus fx chorus.flac -o chorus.fx.wav --fx "fade:in=1s,out=2s" --fx "reverb:mix=20%"
```

What the first two print:

```json
{"path": "song.mp3", "container": "mp3", "codec": "mp3", "duration_s": 84.8, "duration": "1:24.800",
 "sample_rate": 48000, "channels": 2, "channel_layout": "stereo", "bit_depth": null,
 "bitrate_kbps": 129.0, "lossless": false, "tags": {}, "art": {"present": false, "pictures": []}}
```

```text
song.flac
```

```json
{"input": "song.mp3", "output": "song.flac", "format": "flac", "bit_depth": 16, "sample_rate": 48000,
 "channels": 2, "duration_s": 84.8, "frames": 4070400, ...}
```

Each command prints the output path, then its result. Each output also gets a
`<output>.ferraus.json` file beside it, recording exactly what was done
([provenance sidecars](concepts/json-and-sidecars.md)). Without `-o` the output goes beside the
input as `<input>.<command>.<ext>` (`song.normalize.flac`). Existing files are never
overwritten unless you pass `--overwrite`.

Useful on every command:

* `--json`: one machine-readable result object on stdout, nothing else.
* `--dry-run`: resolve everything and show the plan without processing.
* `ferraus docs <command>`: the full reference page, offline. `ferraus <command> --help` is
  the short version.

Where to go next: a [tutorial](tutorials/clean-a-podcast.md) for your task, or the
[agent guide](agents.md) if you are building with an AI model.

## Commands that use models

Stem separation, speech denoising, the neural pitch trackers, chord transcription, and sheet
music recognition use neural-network models that are not in the package. ferraus never
downloads one unless you allow it, once, with `-y`:

```bash
ferraus separate song.mp3 --tier fast -y
```

or pull models ahead of time (`ferraus models pull separate/htdemucs`) and work offline with
`--offline`. `ferraus models list`
shows every model with its size, license, and which tiers use it. Some models are not yet
downloadable in this release; see [Stems and separation tiers](concepts/stems.md).

## Where files live

| What | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Config | `~/Library/Application Support/ferraus/config.toml` | `~/.config/ferraus/config.toml` | `%APPDATA%\ferraus\config.toml` |
| Models (cache) | `~/Library/Caches/ferraus/models` | `~/.cache/ferraus/models` | `%LOCALAPPDATA%\ferraus\cache\models` |
| Sounds, jobs (data) | `~/Library/Application Support/ferraus/data` | `~/.local/share/ferraus` | `%LOCALAPPDATA%\ferraus\data` |

On Linux the `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, and `XDG_DATA_HOME` variables are honoured.
`FERRAUS_HOME=<dir>` moves all three (to `<dir>/config`, `<dir>/cache`, and `<dir>/data`),
which is handy for a sandbox or a test; `FERRAUS_CONFIG` points at another config file.

## Uninstall

* Release installer: `ferraus self-uninstall -y` (`--purge` also removes models, sounds, and config).
* `cargo install`: `cargo uninstall ferraus`.
* Package managers: `brew uninstall ferraus`, `sudo apt remove ferraus`, `sudo dnf remove ferraus`,
  `winget uninstall djynnius.ferraus`.

Models, sounds, and config stay in the folders above until you delete them (or use `--purge`).
