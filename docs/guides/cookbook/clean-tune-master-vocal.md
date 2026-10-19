# Clean, tune, and master a vocal

An agent recipe: take a raw vocal take and a backing track to a mastered song, deciding each
step from the JSON the previous step returned. Every command below was run with ferraus 0.1.0;
JSON is trimmed to the fields an agent reads. Add `--json` to every call and read `ok`,
`error.hint`, `warnings`, and `metrics`.

Inputs: `take.wav` (dry mono vocal), `beat.wav` (instrumental).

## 1. Diagnose

```bash
ferraus analyze problems take.wav --json
```

```json
{"ok": true, "command": "analyze problems",
 "metrics": {"high": 0, "medium": 0, "low": 0, "clean": true},
 "result": {"clean": true, "counts": {"high": 0, "medium": 0, "low": 0},
            "problems": [{"problem": "hum", "severity": "medium", "details": "...", "suggested_fix": "dehum:freq=60Hz"}]}}
```

**Decide:** build the cleanup chain from `result.problems[].suggested_fix` (each is an `fx`
stage string). If `clean` is true and `problems` is empty, skip cleanup: do not denoise a
take that has no noise. On a clean, digitally silent take `denoise --auto` learns "noise" from
silence and still turned the level down by about 6 dB in our test (`output_rms_dbfs` -26.3 vs
`input_rms_dbfs` -20.1), so only run it when the report names hiss or noise.

```bash
ferraus fx take.wav -o take.clean.wav --fx "dehum:freq=60Hz" --fx "denoise:auto" --fx "highpass:freq=80Hz" --json
```

Read back `params.noise_region` (where the noise was learned; it must be a pause) and
`metrics.removed_rms_dbfs` (should be far below `input_rms_dbfs`).

## 2. Find the key and tune

Prefer the key of the full song, measured on the beat (polyphonic material gives a reliable
chroma estimate):

```bash
ferraus analyze key beat.wav --json
```

```json
{"ok": true, "command": "analyze key",
 "metrics": {"confidence": 0.595},
 "warnings": [{"code": "low_key_confidence", "message": "key confidence 0.59 is below 0.6; 'C major' is only a best guess"}],
 "result": {"key": "C major", "tonic": "C", "mode": "major", "confidence": 0.595, "correlation": 0.823,
            "alternatives": [{"key": "G major", "correlation": 0.774}, {"key": "A minor", ...}]}}
```

**Decide:** if `result.confidence` is at least 0.6, pass `result.tonic` and `result.mode`.
Here it is 0.595 with a `low_key_confidence` warning, and the runners-up are the neighbouring
key (G major) and the relative minor (A minor), which share most notes with C major. An agent
should ask the user, or check with `ferraus analyze chords beat.wav` (this song's chords are C,
Am, F, G: C major). Without `--key`, `tune` falls back to the chromatic scale by itself and
warns with the same code.

```bash
ferraus tune take.clean.wav --key C --scale major --strength 85% --retune-speed 30ms --notes-out notes.json -o take.tuned.wav --json
```

```json
{"ok": true, "command": "tune",
 "params": {"key": "C", "scale": "major", "strength": "85%", "retune_speed": "30ms", ...},
 "metrics": {"notes": 14, "corrected_notes": 14, "mean_abs_deviation_cents": 49.44, "max_abs_deviation_cents": 83.77, "resynthesis": "td_psola"}}
```

**Check:** a `mean_abs_deviation_cents` near 50 means notes sit between two scale notes and
may be pulled to the wrong one; open `notes.json` and look at `deviation_cents` and
`confidence` per note. Notes with large deviation and low confidence are tracking errors: lower
`--strength`, try `--tracker crepe -y`, or tune to a written melody with `--target melody.mid`.

## 3. Vocal effects

```bash
ferraus presets list compress --json        # vocal, drums, bus, master, tighten
ferraus fx take.tuned.wav -o take.fx.wav --fx "compress:preset=vocal" --fx deess --fx "reverb:type=plate,mix=18%" --json
```

## 4. Mix with the beat

```bash
ferraus mix take.fx.wav beat.wav --gains 0dB,-6dB -o song.mix.wav --json
```

```json
{"ok": true, "command": "mix",
 "result": {"duration_s": 21.2, "channels": 2, "sample_rate": 48000,
            "inputs": [{"path": "take.fx.wav", "gain_db": 0.0, "pan": null, "offset_s": 0.0},
                       {"path": "beat.wav", "gain_db": -6.0, "pan": null, "offset_s": 0.0}]}}
```

Use `--offsets 0s,2b` (with `--bpm`) to start the vocal later, and `--pans` to place parts.

## 5. Master and verify

```bash
ferraus master song.mix.wav --target streaming -o song.master.wav --report report.json --json
```

```json
{"ok": true, "command": "master",
 "metrics": {"integrated_lufs": -14.01, "true_peak_dbtp": -1.05, "limiter_gain_reduction_db": 1.68,
             "target_reached": true, "verify_pass": true}}
```

stderr may show `warning: the result peaks at +0.55 dBFS and will clip in fixed-point formats`
from inner stages; those stages run in floating point, and the final file is what
`true_peak_dbtp` and `verify_pass` describe.

**Decide:** if `target_reached` is false, the `warnings` array says why (for example, the
limiter bound of `--intensity`), with the flag that fixes it. Confirm independently:

```bash
ferraus master verify song.master.wav --target streaming --json
```

Exit code 0 and `"ok": true` mean every check passed; exit code 9 lists `failed_checks`, each
with a `fix` string.

## The same job as one file

The whole recipe is also a [pipeline file](pipeline-files.md), which caches every stage so
re-running after a change recomputes only what changed. Over MCP, the server's
`clean-and-tune-vocal` prompt walks a model through these steps ([MCP setup](mcp-tool-calls.md)).
