# ferraus examples

Runnable examples for scripts and AI agents. Each one works on test audio that ferraus
generates itself (tones, noise, a drum-and-keys backing, a voice-like line, a hummed tune, a
MIDI file), so nothing licensed or binary is checked in. CI runs every example on each push
(`examples/run-all.sh`).

## Prerequisites

* `ferraus` on your `PATH`, or `FERRAUS=/path/to/ferraus` in the environment (every script
  uses `${FERRAUS:-ferraus}`).
* bash and Python 3 (standard library only, except `agents/claude_tool_use.py`, which needs
  `pip install anthropic` for its agent loop).
* `jq` is optional: the shell scripts use it when it is installed and fall back to python3.
* No models: everything runs without downloads. The two separation examples download the
  fast-tier model (htdemucs, 166 MB) only when you set `FERRAUS_ALLOW_DOWNLOADS=1`; otherwise
  they plan the run with `--dry-run` and say so.

## Quick start

```bash
examples/make-samples.sh samples           # about 5 s and 45 MB; writes ./samples
examples/shell/clean-tune-master.sh samples out/vocal
examples/run-all.sh                        # everything, in a temp dir, with a pass/fail summary
```

Every shell script takes `[SAMPLES_DIR] [OUT_DIR]`. Without them it makes fresh samples and an
output folder in a temp dir and prints where. Scripts write only into those folders, keep the
JSON envelope of every call in `OUT_DIR` (under `json/`, with stderr beside it), and tell you
what to listen for. Shared helpers (`jget`, `frx`, ...) are in [shell/common.sh](shell/common.sh).

## What is here

| Example | Shows | Models |
| --- | --- | --- |
| [make-samples.sh](make-samples.sh) | `generate tone/noise`, `midi from-json`, `render`, `pitch`, `mix`, `trim`: building test material from nothing | none |
| [shell/inspect.sh](shell/inspect.sh) | reading the envelope with jq (or python3): `info`, `analyze problems`, `fx`, `compare`, the provenance sidecar, `verify`, an error envelope and its exit code | none |
| [shell/clean-tune-master.sh](shell/clean-tune-master.sh) | analyze problems → denoise → analyze key → tune → vocal fx from presets → mix → master → master verify, each step decided from the last one's JSON | none |
| [shell/batch-podcast.sh](shell/batch-podcast.sh) | a loop over episodes: problems → an fx chain built from each `suggested_fix` → loudness for mono or stereo → `master verify --target podcast` → a report table | none |
| [shell/hum-to-piano.sh](shell/hum-to-piano.sh) | transcribe → check the notes → midi quantize → render on a piano → score to MusicXML → score check; then the one-command `hum` | none |
| [shell/stems-for-remix.sh](shell/stems-for-remix.sh) | tempo and key, separation planned with `--dry-run`, what exit code 5 and its hint look like; with downloads: separate (fast tier), a quality table per stem, a null test of the summed stems | fast tier, opt-in |
| [pipelines/vocal-chain.yaml](pipelines/vocal-chain.yaml) | a multi-stage vocal chain with presets; stage caching and `--set` | none |
| [pipelines/podcast-episode.yaml](pipelines/podcast-episode.yaml) | cleanup, intro/outro concat with crossfades, the mono podcast loudness target, two output formats | none |
| [pipelines/master-album.yaml](pipelines/master-album.yaml) | three tracks mastered to one target in parallel; 24-bit WAV masters and 16-bit 44.1 kHz FLAC copies | none |
| [pipelines/remix-prep.yaml](pipelines/remix-prep.yaml) | analysis stages, separation addressed as `stems.drums`, per-stem stretch and pitch, a sketch mix | fast tier, opt-in |
| [mcp/client.py](mcp/client.py) | an MCP client over stdio: initialize, tools/list, `ferraus_info`, `ferraus_fx`, prompts/list, a background job polled to completion, the sandbox refusing a path | none |
| [mcp/claude-desktop-config.json](mcp/claude-desktop-config.json) | the server entry for Claude Desktop or any client that reads `mcpServers` | none |
| [agents/parse_envelope.py](agents/parse_envelope.py) | running commands from Python and handling `ok`, `error.code`/`hint`, exit codes (5: model not cached), `warnings`, batches, and `outputs[].role` | none |
| [agents/claude_tool_use.py](agents/claude_tool_use.py) | an agent loop with the Anthropic Python SDK: tool definitions built from `ferraus schema`, tool calls run through the CLI, results fed back until the model stops | none (needs an API key) |

## Pipelines

Pipeline paths are relative to the pipeline file, and the files expect the samples next to
them:

```bash
examples/make-samples.sh examples/pipelines/samples
ferraus pipeline run examples/pipelines/vocal-chain.yaml --json                 # writes examples/pipelines/out/
ferraus pipeline run examples/pipelines/vocal-chain.yaml --overwrite --json     # every stage "cached": true
ferraus pipeline run examples/pipelines/vocal-chain.yaml --overwrite --json \
  --set stages.tuned.args.strength=100%                                        # only tuned and later stages rerun
ferraus pipeline run examples/pipelines/remix-prep.yaml --dry-run --json        # the plan, nothing computed
```

Expected: the first run reports `"computed": 5`; the second `"computed": 0, "cached": 5`; the
`--set` run recomputes four stages and keeps `clean`. Outputs are never overwritten without
`--overwrite`.

## MCP

Claude Code:

```bash
claude mcp add ferraus -- ferraus mcp serve --allow ~/music
```

Claude Desktop and other clients: merge [mcp/claude-desktop-config.json](mcp/claude-desktop-config.json)
into the client's config (`claude_desktop_config.json` for Claude Desktop) and change the
`--allow` folder to an absolute path of yours (`~` is not expanded there). Every file a tool
reads or writes must be inside an `--allow` folder.

To see the protocol without a client:

```bash
python3 examples/mcp/client.py --samples samples
```

## The Claude agent

```bash
pip install anthropic
export ANTHROPIC_API_KEY=...
python3 examples/agents/claude_tool_use.py --samples samples        # default task: clean a podcast episode
python3 examples/agents/claude_tool_use.py --task "Master $PWD/samples/song.wav for streaming as song.master.wav"
python3 examples/agents/claude_tool_use.py --print-tools            # the tool definitions; no API call, no SDK needed
```

The model is `claude-opus-5-5` unless `--model` or `ANTHROPIC_MODEL` says otherwise. The
agent can use six commands (`info`, `analyze problems`, `denoise`, `fx`, `normalize`,
`master`); its outputs go to `--workdir` (default: a temp dir), it cannot overwrite files,
and it cannot download models.

## Expected output

`examples/run-all.sh` ends with a summary like this (a debug build on a laptop; a release
build is several times faster):

```text
== summary
   ok    make-samples                                         4s
   ok    shell/inspect.sh                                     1s
   ok    shell/clean-tune-master.sh                           6s
   ok    shell/batch-podcast.sh                               2s
   ok    shell/hum-to-piano.sh                                1s
   ok    shell/stems-for-remix.sh                             0s
   ok    pipelines/vocal-chain.yaml (run twice, cached)       5s
   ok    pipelines/podcast-episode.yaml (run twice, cached)   1s
   ok    pipelines/master-album.yaml (run twice, cached)      7s
   ok    pipelines/vocal-chain.yaml (--set one stage)         4s
   ok    pipelines/remix-prep.yaml                            0s
   ok    mcp/client.py                                        5s
   ok    agents/parse_envelope.py                             1s
   ok    agents/claude_tool_use.py --print-tools              0s
   total 37s, 0 failed; logs in /tmp/ferraus-examples.XXXXXX/logs
```

Each step's full output is in its log. `run-all.sh` uses a private ferraus home inside its
work folder, so it never reads or fills your own caches; pass a folder as its first argument
to keep everything afterwards.
