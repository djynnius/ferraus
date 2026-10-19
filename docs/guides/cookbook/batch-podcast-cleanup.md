# Batch podcast cleanup

Clean and level a whole folder of episodes in one call. Every command takes several inputs or
a glob, processes each file the same way, and returns one envelope with a result per file.
Run with ferraus 0.1.0; JSON is trimmed.

```text
episodes/ep01.wav   episodes/ep02.wav   episodes/ep03.flac
```

## 1. Measure them all

```bash
ferraus analyze loudness "episodes/*.wav" "episodes/*.flac" --json
```

```json
{"ok": true, "command": "analyze loudness",
 "inputs": [{"path": "episodes/ep01.wav", ...}, {"path": "episodes/ep02.wav", ...}, {"path": "episodes/ep03.flac", ...}],
 "result": [
   {"input": "episodes/ep01.wav", "metrics": {"integrated_lufs": -16.4262, "loudness_range_lu": 1.7741, "true_peak_dbtp": -1.8811, ...}, "result": {...}},
   {"input": "episodes/ep02.wav", "metrics": {"integrated_lufs": -22.4262, ...}, "result": {...}},
   {"input": "episodes/ep03.flac", ...}]}
```

Quote globs so ferraus expands them (the same on every shell, including PowerShell). **Name
the extensions:** a bare `"episodes/*"` also matches the `.ferraus.json` sidecars that ferraus
writes beside its outputs. Each of those becomes a failed item with `unsupported_format`, and
because one item failed, the envelope's top-level `ok` is false and the exit code is 4, even
though every audio file succeeded. In a batch, `result[]` has one entry per input; failed
entries carry `"ok": false` and an `error` object, so check each entry.

## 2. Clean, level, and normalize in one pass per file

```bash
ferraus fx "episodes/*.wav" "episodes/*.flac" --fx dehum --fx "denoise:auto" --fx leveler \
  --fx "enhance-voice:mode=female" --fx "normalize:lufs=-16" --outdir clean --json
```

```json
{"ok": true, "command": "fx",
 "outputs": [{"path": "clean/ep01.fx.wav"}, {"path": "clean/ep02.fx.wav"}, {"path": "clean/ep03.fx.flac"}],
 "result": [{"input": "episodes/ep01.wav", "metrics": {"freq_hz": 60.001, "hum_reduction_db": 16.7, "noise_floor_db": -81.2, ...}}, ...]}
```

* `--outdir clean` is required for a batch unless `-o` is a template. Outputs keep the input
  name plus the command (`ep01.fx.wav`) and the input's format.
* Use an `-o` template for other names: `-o 'clean/{name}.wav'` (`{name}` is the input file's
  stem, `{index}` its position in the batch, `{ext}` its extension).
* `denoise:auto` learns the noise of each file separately, and `params.noise_region` per item
  says where. Check that each landed on a pause.

If you only need the loudness step (the "normalize every episode to -16 LUFS" job):

```bash
ferraus normalize "episodes/*.wav" --lufs -16 --outdir loud --json
```

`--dry-run` shows the output names first:

```json
{"ok": true, "command": "normalize", "dry_run": true,
 "outputs": [{"path": "loud/ep01.normalize.wav"}, {"path": "loud/ep02.normalize.wav"}],
 "params": {"lufs": "-16", "ceiling": "-1dBTP", "true_peak_limit": "on", ...}}
```

## 3. Verify against the podcast spec

```bash
ferraus master verify "clean/*.wav" "clean/*.flac" --target podcast --json
```

```json
{"ok": false, "result": [
  {"input": "clean/ep01.fx.wav", "ok": false, "error": {"code": "verification_failed",
   "message": "clean/ep01.fx.wav fails the podcast spec: loudness: integrated loudness -16.00 LUFS is +3.00 LU from the -19 LUFS target",
   "hint": "ferraus master clean/ep01.fx.wav --target podcast -o fixed.wav", "exit_code": 9}}, ...]}
```

These episodes are **mono**, and the podcast spec is -16 LUFS for stereo but -19 LUFS for mono
(a mono file plays from both speakers, which sounds about 3 dB louder). Either normalize mono
episodes to -19 (`--fx "normalize:lufs=-19"`) or make them stereo (`--channels 2` on the `fx`
run) and keep -16.

## 4. Encode for publishing

```bash
ferraus convert "clean/*.wav" --format mp3 --bitrate 128k --outdir mp3 --json
```

```text
mp3/ep01.fx.convert.mp3   mp3/ep02.fx.convert.mp3
```

## 5. Long batches

Run the batch in the background and collect it later:

```bash
ferraus fx "episodes/*.wav" --fx dehum --fx "normalize:lufs=-16" --outdir clean --async --json   # {"ok": true, "job_id": "..."}
ferraus jobs wait <job_id> --json
```

Re-running the same command skips files whose output already exists with a matching sidecar
(warning `reused_output`, `"result": {"reused": true}`), so an interrupted batch can simply be
started again. To recompute anyway, pass `--force --overwrite` (`--force` alone stops at the
existing file with `output_exists`).
