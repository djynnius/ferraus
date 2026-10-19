# ferraus

**A command-line audio workstation for scripts and AI agents.** One cross-platform binary
covers conversion and editing, effects, restoration, noise reduction, stem separation, pitch
correction, MIDI, notation in both directions, CLAP/VST3 instrument hosting, and mastering.

* **New here?** Start with [Getting started](getting-started.md), then pick a tutorial:
  [clean a podcast](tutorials/clean-a-podcast.md), [tune a vocal](tutorials/tune-a-vocal.md),
  [split a song into stems](tutorials/split-a-song-into-stems.md),
  [turn a hum into an instrument part](tutorials/hum-to-instrument.md),
  [make a score](tutorials/make-a-score.md),
  [perform a sheet-music file](tutorials/perform-sheet-music.md),
  [master a track](tutorials/master-a-track.md), or [host a plugin](tutorials/host-a-plugin.md).
  Each one says what to listen for at every step.
* **Building an agent?** Read the [agent guide](agents.md), [MCP setup](cookbook/mcp-tool-calls.md),
  and the cookbook: [clean, tune, and master a vocal](cookbook/clean-tune-master-vocal.md),
  [stems for a remix](cookbook/stems-for-remix.md), [hum to an instrument](cookbook/hum-to-instrument.md),
  [sheet music to a performance](cookbook/sheet-music-to-performance.md),
  [batch podcast cleanup](cookbook/batch-podcast-cleanup.md), [pipeline files](cookbook/pipeline-files.md).
* **Coming from FFmpeg?** The [FFmpeg equivalents](cookbook/ffmpeg-equivalents.md) table
  translates common commands.
* **Looking up a command?** Every command has a reference page under *Command reference*, and
  `ferraus docs <command>` prints the same page offline. Background on units, loudness, pan
  laws, stems, notation, sound folders, and sidecars is under *Concepts*.
* **Something wrong?** See [Troubleshooting](troubleshooting.md) and the [error catalog](errors.md).

Command pages are generated from the same definitions as the binary, and every command-line
example on them is checked against the CLI in CI. The tutorials and cookbook were written by
running every command shown against ferraus 0.1.0; their outputs are trimmed copies of what it
printed.
