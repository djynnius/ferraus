# Using ferraus from an AI agent

ferraus is a music-editing CLI built to be driven by agents. This file is the short
operating manual; `ferraus describe --agent` prints the full command catalog (one line per
command, generated from the binary, also in [docs/agents/catalog.md](docs/agents/catalog.md)),
and `ferraus docs <command>` prints any command's full reference page.

## Contract

* Add `--json` to every call. stdout then holds exactly one JSON envelope:
  `ok`, `command`, `inputs`, `outputs` (paths with `role`s such as `vocals`), `params` (every
  parameter after defaults, presets, and run-time detection), `models`, `metrics`, `warnings`,
  `result` (command-specific data), `timing_ms`; on failure `error` with `code`, `message`,
  `hint` (a runnable command when one exists), and `exit_code`.
* Logs and progress go to stderr. `--progress json` gives NDJSON events.
* `ferraus schema <command>` prints the JSON Schema of its arguments and result.
* Never prompts. Never overwrites without `--overwrite`. Never writes outside the paths you
  give it (MCP: outside the `--allow` directories).
* Every output gets `<output>.ferraus.json` (provenance). Read it to learn what was detected
  (key, tempo, noise region, model hashes) instead of re-running analysis. `ferraus replay
  <sidecar>` reproduces it; `ferraus verify <sidecar>` checks hashes.
* Exit codes: 0 ok, 1 processing, 2 usage, 3 input missing, 4 unsupported format, 5 model not
  cached (run the hint), 6 download/verification, 7 plugin, 8 cancelled/timeout, 9 output
  verification failed, 10 network, 70 bug.

## Working habits that pay off

1. Inspect first: `ferraus info x.wav --json`, `ferraus analyze problems x.wav --json` (each
   problem comes with a `suggested_fix` you can run or paste into an fx chain).
2. Plan with `--dry-run --json` (and `--explain`) before long operations.
3. Chain effects in one pass: `ferraus fx in.wav -o out.wav --fx "denoise:auto" --fx "compress:preset=vocal" --fx "normalize:lufs=-14"`.
   Every effect command is an fx stage with the same parameter names.
4. Multi-step jobs belong in a pipeline file (`ferraus pipeline run job.yaml`): stages are
   cached by content hash, so editing one stage recomputes only what changed.
5. Check your work: `ferraus compare original.wav processed.wav --json` (alignment, RMS
   difference, SI-SDR) and, for masters, `ferraus master verify master.wav --target streaming`.
6. Long work: `--async` returns a job id (`ferraus jobs wait <id>`); over MCP use
   `ferraus_job_start`.
7. Models download only with `-y` or `models.auto_download = true`. On exit code 5, run the
   `hint` (`ferraus models pull ...`) or retry with `-y`.

## Units (same everywhere)

`1.5s` `1500ms` `01:23.5` `4b` (beats) `-10s` (from the end) `0s..2s` (range) · `-6dB` `0.5x`
`-14LUFS` `-1dBTP` · `120Hz` `2.5kHz` `C4` `midi:60` · `4:1` · `80%` or `0.8` · `1/16` `1/8T`
`1/8D` · `+200cents` `2st`.

## Recipes

```bash
# clean, tune, and master a vocal over a beat
ferraus denoise vocal.wav --auto -o vocal.clean.wav --json
ferraus tune vocal.clean.wav --key auto --strength 85% -o vocal.tuned.wav --json
ferraus mix vocal.tuned.wav beat.wav --gains 0dB,-3dB -o mix.wav --json
ferraus master mix.wav --target streaming -o master.wav --report report.json --json

# stems for a remix (--tier fast for a quick sketch; the default auto picks balanced; --tier quality for the cleanest vocals)
ferraus separate song.mp3 --stems 4 -y --json

# hummed idea to an instrument
ferraus hum idea.wav --to piano -o idea.piano.wav --json

# score from MIDI / read printed sheet music (PDF/PNG) / perform a score
ferraus score song.mid --chords diagrams -o song.musicxml --json
ferraus score read minuet.pdf -o minuet.musicxml --review -y --json
ferraus score perform sonata.musicxml --instrument piano -o sonata.wav --json
```

## MCP

```bash
claude mcp add ferraus -- ferraus mcp serve --allow ~/music
```

Tools are `ferraus_<command>` (`ferraus_denoise_profile`), arguments are the command's flags
with `_` instead of `-` plus `inputs` and `output`. Prompts: `clean-and-tune-vocal`,
`extract-stems-for-remix`, `hum-to-instrument`, `master-song`. See
[docs/guides/concepts/mcp.md](docs/guides/concepts/mcp.md) and the agent recipes in
[docs/guides/cookbook/](docs/guides/cookbook/).

## Working on the ferraus codebase

See [docs/dev/AUTHORING.md](docs/dev/AUTHORING.md) (how commands are defined),
[docs/decisions/](docs/decisions/) (ADRs), and [docs/dev/RELEASING.md](docs/dev/RELEASING.md).
Never build or publish release artifacts locally; releases come only from GitHub Actions.
