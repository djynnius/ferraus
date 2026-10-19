# Getting started

## Install

**macOS and Linux**

```bash
curl -fsSL https://github.com/djynnius/ferraus/releases/latest/download/ferraus-installer.sh | sh
```

Or `brew install djynnius/tap/ferraus` on macOS, the `.deb` on Debian/Ubuntu
(`sudo apt install ./ferraus_<version>_amd64.deb`), or the `.rpm` on Fedora/RHEL.

**Windows (PowerShell)**

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/djynnius/ferraus/releases/latest/download/ferraus-installer.ps1 | iex"
```

Or `winget install djynnius.ferraus`, or the MSI from the release page.

The installers put `ferraus` in `~/.ferraus/bin` (set `FERRAUS_INSTALL_DIR` to change it) and
never ask for administrator rights. Open a new terminal afterwards.

## Check the installation

```bash
ferraus doctor
```

`doctor` prints the version, where config, models, and sounds live, whether FFmpeg is
available for the formats that need it, and which compute device is active.

## Your first five commands

```bash
ferraus info song.mp3                          # what is in this file?
ferraus convert song.mp3 -o song.flac          # lossless copy
ferraus normalize song.flac --lufs -14 -o song.loud.flac
ferraus trim song.flac --range 0:30..1:00 -o chorus.flac
ferraus fx chorus.flac -o chorus.fx.wav --fx "fade:in=1s,out=2s" --fx "reverb:mix=20%"
```

Each command writes its output beside the input (unless you pass `-o`) plus a
`<output>.ferraus.json` file recording exactly what was done. Add `--json` to any command for
a machine-readable result, `--dry-run` to see the plan without processing, and
`ferraus docs <command>` for the full reference page.

## Commands that use models

Stem separation, model denoising, some pitch trackers, and transcription use neural-network
models that are not in the package. Allow the download once with `-y`:

```bash
ferraus separate song.mp3 -y
```

or pull models ahead of time (`ferraus models pull --all-for separate`) and work offline with
`--offline`.

## Where files live

| What | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Config | `~/Library/Application Support/ferraus/config.toml` | `~/.config/ferraus/config.toml` | `%APPDATA%\ferraus\config.toml` |
| Models (cache) | `~/Library/Caches/ferraus/models` | `~/.cache/ferraus/models` | `%LOCALAPPDATA%\ferraus\cache\models` |
| Sounds, jobs (data) | `~/Library/Application Support/ferraus/data` | `~/.local/share/ferraus` | `%LOCALAPPDATA%\ferraus\data` |

`FERRAUS_HOME` moves cache and data; `FERRAUS_CONFIG` points at another config file.

## Uninstall

* URL installer: `ferraus self-uninstall -y` (`--purge` also removes models, sounds, and config).
* Package managers: `brew uninstall ferraus`, `sudo apt remove ferraus`, `sudo dnf remove ferraus`,
  `winget uninstall djynnius.ferraus`.
