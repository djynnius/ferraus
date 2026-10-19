# JSON results and provenance sidecars

## The result envelope

With `--json`, every command prints exactly one JSON object on stdout:

```json
{
  "ok": true,
  "command": "separate",
  "ferraus_version": "0.1.0",
  "schema_version": 1,
  "inputs": [{"path": "song.wav", "duration_s": 21.2, "sample_rate": 48000, "channels": 2}],
  "outputs": [{"path": "song.vocals.wav", "role": "vocals", "duration_s": 21.2, "sample_rate": 48000, "channels": 2}],
  "params": {"stems": "4", "tier": "fast", "overlap": "25%", "sum_to_mix": "on", "execution_provider": "cpu"},
  "models": [{"name": "separate/htdemucs", "version": "v4-onnx.2026-05-21", "sha256": "d05c269d..."}],
  "metrics": {"quality_vocals": 0.018, "quality_drums": 0.901},
  "warnings": [],
  "result": {"stems": [...], "tier": "fast"},
  "timing_ms": 11065
}
```

`params` holds every parameter after defaults and presets, plus values resolved at run time
(a detected key, the noise region chosen by `--auto`). Errors use the same envelope with
`"ok": false` and an `error` object: `code` (stable snake_case), `message`, `hint` (a runnable
command when one exists), and `exit_code`.

With several inputs (a list or a glob), `result` is an array with one entry per input:
`{"input": "...", "metrics": {...}, "result": {...}}`, or `{"input": "...", "ok": false,
"error": {...}}` for an input that failed. If any input failed, the top-level `ok` is false
and the exit code is that input's, so check every entry.

In examples throughout these docs, `...` marks fields left out for brevity.

`ferraus schema <command>` prints the JSON Schema (draft 2020-12) of a command's arguments and
of its result. `schema_version` increments only on breaking changes.

## Progress events

`--progress json` writes NDJSON to stderr:

```json
{"event":"progress","stage":"separate","percent":0.0,"eta_s":null}
{"event":"stage_start","stage":"separate/htdemucs@v4-onnx.2026-05-21","index":1,"of":1}
{"event":"progress","stage":"separate","percent":45.0,"eta_s":9}
{"event":"output","path":"prog/song.vocals.wav","role":"vocals"}
{"event":"warning","code":"low_key_confidence","message":"..."}
```

## Sidecars

Each output gets `<output>.ferraus.json` recording the command, argv, resolved parameters,
input and output SHA-256 hashes, models with hashes, plugins and their state, host, whether
the run was deterministic, and timing. The sidecar of `ferraus transcribe idea.wav -o idea.mid`:

```json
{
  "ferraus_version": "0.1.0",
  "schema_version": 1,
  "command": "transcribe",
  "argv": ["transcribe", "idea.wav", "-o", "idea.mid", "--json"],
  "params": {"bpm": "99.84 (audio)", "mode": "mono", "tracker": "pyin", "key": "A minor (confidence 0.91; snapped)", "...": "..."},
  "inputs": [{"path": "idea.wav", "sha256": "932ac21f...", "duration_s": 10.07}],
  "outputs": [{"path": "idea.mid", "sha256": "1a746192..."},
              {"path": "idea.notes.json", "sha256": "0c5d9a3e...", "role": "notes"}],
  "models": [],
  "plugins": [],
  "host": {"os": "macos", "arch": "aarch64", "device": "auto", "threads": 10},
  "deterministic": true,
  "warnings": [],
  "timing_ms": 89
}
```

Values resolved at run time (the detected tempo and key above) are recorded, so a replay uses
the same values even if detection would change. `--no-sidecar` skips the file.

* `ferraus replay <sidecar>` re-runs the recorded command with the recorded parameters.
  Deterministic commands reproduce the output bit-exactly on the same machine.
* `ferraus verify <sidecar>` checks that inputs and outputs still match their hashes:

  ```json
  {"verified": true, "files": [
    {"kind": "input", "path": "episodes/ep01.wav", "expected": "2cae956e...", "actual": "2cae956e...", "match": true},
    {"kind": "output", "path": "clean/ep01.wav", "expected": "cd52fcf3...", "actual": "cd52fcf3...", "match": true}]}
  ```
* If an output exists and its sidecar matches the command, parameters, inputs, and version,
  ferraus returns the existing result instantly (warning `reused_output`, `"result":
  {"reused": true}`). To recompute, pass `--force --overwrite`. `replay` also needs
  `--overwrite` (or `-o` to a new path) when the recorded output still exists.
* Attach the sidecar when you report a bug: it is everything needed to reproduce the run.
