# Pipeline files

A pipeline file describes a whole job (inputs, stages, outputs) in one YAML or JSON document.
`ferraus pipeline run` executes it, caches every stage by content hash, and on a re-run
recomputes only the stages whose inputs or arguments changed. It is the reproducible form of a
multi-step recipe, and the easiest way for an agent to hand a person a job they can re-run.
Run with ferraus 0.1.0; JSON is trimmed.

## The file

```yaml
# job.yaml: clean a vocal take, tune it, put it over a beat, and master the bounce
version: 1
inputs:
  vocal: vocal.wav
  beat: beat.wav
stages:
  - id: clean
    op: denoise
    in: vocal
    args: { auto: true, reduction: 12dB }
  - id: tuned
    op: tune
    in: clean
    args: { key: C, scale: major, strength: 85%, retune_speed: 30ms }
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
    args: { gains: [0dB, -6dB] }
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

Rules:

* `op` is any command name (`denoise`, `tune`, `separate`, `midi quantize`, ...) and `args`
  keys are its flags with `_` instead of `-` (`retune_speed` for `--retune-speed`). A flag
  without a value is `true` (`auto: true`).
* `in` names an input or an earlier stage's `id`. A stage with several outputs (`separate`) is
  addressed as `id.role`, for example `stems.vocals`.
* A list value (`gains: [0dB, -6dB]`) is the same as the comma list on the command line
  (`--gains 0dB,-6dB`).
* `fx` stages take `chain`, a list of `{op, ...args}`.
* `outputs[]` choose which stage results are written. The format follows the extension, and
  each output can set `bit_depth`, `sample_rate`, and `bitrate` (other keys are ignored in this
  release; convert channels in a stage, for example `{op: channels, to: mono}`).

## Validate and explain

```bash
ferraus pipeline validate job.yaml
```

```json
{"valid": true, "order": ["clean", "tuned", "vox_fx", "mix", "master"],
 "stages": [{"id": "clean", "op": "denoise", "in": ["vocal"],
             "args": {"auto": "true", "reduction": "12dB", "mode": "profile", "sensitivity": "6", ...}}, ...],
 "outputs": [{"from": "master", "path": "out/song.wav", "bit_depth": 24}, {"from": "tuned", "path": "out/vocal_tuned.wav"}]}
```

`pipeline explain job.yaml` prints the same plan with every default filled in. Both catch
unknown ops, unknown or invalid arguments, unknown references, and cycles before anything
runs. `pipeline run job.yaml --dry-run --json` returns the same plan plus the output paths it
would write, and refuses outputs that already exist unless `--overwrite` is given.

## Run

```bash
ferraus pipeline run job.yaml --json
```

```json
{"ok": true, "command": "pipeline run",
 "outputs": [{"path": "out/song.wav", "role": "master", "duration_s": 21.2}, {"path": "out/vocal_tuned.wav", "role": "tuned"}],
 "result": {"pipeline": "job.yaml", "plan": [
   {"id": "clean", "op": "denoise", "cached": false, "timing_ms": 99, "key": "21a95ab8...",
    "result": {"mode": "profile", "noise_region": {"range": "0.085s..1.077s", "method": "auto"}}},
   {"id": "tuned", "op": "tune", "cached": false, "result": {"key": "C major", "scale": "C major"}},
   {"id": "mix", "op": "mix", "args": {"gains": "0dB,-6dB", ...},
    "result": {"inputs": [{"gain_db": 0.0}, {"path": "beat.wav", "gain_db": -6.0}]}}, ...]}}
```

`out/job.ferraus.json` records the resolved plan and timings, and each output gets its own
sidecar.

## Change one thing and re-run

`--set` overrides any field without editing the file (stages are addressed by `id`):

```bash
ferraus pipeline run job.yaml --set stages.tuned.args.strength=100% --overwrite --json
```

Only `tuned` and the stages after it run again; `clean` reports `"cached": true` (and a stage
whose arguments match an earlier run, even an interrupted one, is reused too). Other
overrides: `--set inputs.vocal=take2.wav`, `--set outputs.0.path=out/song_v2.wav`, or a whole
replacement chain as JSON (`--set 'stages.master.args.chain=[{"op":"normalize","lufs":-16}]'`).
`--no-cache` recomputes everything.

Outputs are never overwritten without `--overwrite`, so the second run above needs it. Give
it, or change *every* output path: outputs are written in order, and a run that stops at an
existing file has already written the outputs listed before it.
