# JSON results and provenance sidecars

## The result envelope

With `--json`, every command prints exactly one JSON object on stdout:

```json
{
  "ok": true,
  "command": "separate",
  "ferraus_version": "0.1.0",
  "schema_version": 1,
  "inputs": [{"path": "song.mp3", "duration_s": 214.3, "sample_rate": 44100, "channels": 2}],
  "outputs": [{"path": "song.vocals.wav", "role": "vocals"}],
  "params": {"stems": "4", "tier": "quality"},
  "models": [{"name": "separate/htdemucs", "version": "4", "sha256": "..."}],
  "metrics": {},
  "warnings": [],
  "timing_ms": 48210
}
```

`params` holds every parameter after defaults and presets, plus values resolved at run time
(a detected key, the noise region chosen by `--auto`). Errors use the same envelope with
`"ok": false` and an `error` object: `code` (stable snake_case), `message`, `hint` (a runnable
command when one exists), and `exit_code`.

`ferraus schema <command>` prints the JSON Schema (draft 2020-12) of a command's arguments and
of its result. `schema_version` increments only on breaking changes.

## Progress events

`--progress json` writes NDJSON to stderr:

```json
{"event": "stage_start", "stage": "separate", "index": 1, "of": 3}
{"event": "progress", "stage": "separate", "percent": 42.1, "eta_s": 31}
{"event": "output", "path": "song.vocals.wav", "role": "vocals"}
{"event": "warning", "code": "low_key_confidence", "message": "..."}
```

## Sidecars

Each output gets `<output>.ferraus.json` recording the command, argv, resolved parameters,
input and output SHA-256 hashes, models with hashes, plugins and their state, host, whether
the run was deterministic, and timing.

* `ferraus replay <sidecar>` re-runs the recorded command with the recorded parameters.
  Deterministic commands reproduce the output bit-exactly on the same machine.
* `ferraus verify <sidecar>` checks that inputs and outputs still match their hashes.
* If an output exists and its sidecar matches the command, parameters, inputs, and version,
  ferraus returns the existing result instantly (warning `reused_output`); `--force` recomputes.
