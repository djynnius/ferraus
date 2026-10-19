# Split a song into stems

Stem separation splits a finished mix into its parts: vocals, drums, bass, and everything
else ("other"). Use it to make a karaoke or instrumental version, pull a vocal for a remix,
practise along with the drums muted, or clean up one part. Outputs below are trimmed copies of
what ferraus 0.1.0 printed for a 21-second test song.

**You need:** a song (`song.wav`; MP3, FLAC, and M4A work too), about 170 MB of disk space for
the model, and a network connection the first time.

## Which tier to use

Separation uses neural-network models from the ferraus model registry, and every tier has a
verified download. The default tier, `auto`, picks `quality` (a Mel-Band RoFormer for the
vocals plus fine-tuned Demucs for drums, bass, and other: about 960 MB of models, about 16
minutes for a 4-minute song on a laptop CPU). The commands below use **`--tier fast`** (Demucs
v4 `htdemucs`, 166 MB, MIT license, about 2 minutes for a 4-minute song) to keep the first run
quick; drop `--tier fast` for the cleanest vocals. See
[Stems and separation tiers](../concepts/stems.md) for all tiers and their measured quality.

## 1. See what would happen

```bash
ferraus separate song.wav --tier fast --dry-run --json
```

```json
{"ok": true, "command": "separate", "dry_run": true,
 "result": {"plan": [{"model": "separate/htdemucs@v4-onnx.2026-05-21", "weight": 1.0, "cached": false}],
            "outputs": [{"stem": "vocals", "path": "song.vocals.wav"}, {"stem": "drums", "path": "song.drums.wav"},
                        {"stem": "bass", "path": "song.bass.wav"}, {"stem": "other", "path": "song.other.wav"}]}}
```

Without `-y` ferraus never downloads anything; it stops with exit code 5 and prints the exact
command that fetches the model:

```json
{"ok": false, "error": {"code": "model_not_cached",
  "message": "model is not cached and downloads are disabled: separate/htdemucs@v4-onnx.2026-05-21",
  "hint": "ferraus models pull separate/htdemucs@v4-onnx.2026-05-21", "exit_code": 5}}
```

## 2. Separate

```bash
ferraus separate song.wav --tier fast -y --json
```

```text
warning: downloading separate/htdemucs@v4-onnx.2026-05-21 (165.6 MB, license MIT)
```

```json
{"ok": true, "command": "separate",
 "outputs": [{"path": "song.vocals.wav", "role": "vocals"}, {"path": "song.drums.wav", "role": "drums"},
             {"path": "song.bass.wav", "role": "bass"}, {"path": "song.other.wav", "role": "other"}],
 "models": [{"name": "separate/htdemucs", "version": "v4-onnx.2026-05-21", "sha256": "d05c269d..."}],
 "metrics": {"quality_vocals": 0.018, "quality_drums": 0.901, "quality_bass": 0.516, "quality_other": 0.869},
 "result": {"sum_to_mix": {"applied": true, "target": "other", "residual_peak_db": -15.3, "clipped": 0}, ...}}
```

The model is cached after the first run (`ferraus models list --cached`), and later runs work
offline. On an Apple-silicon laptop the 21-second song took about 11 seconds on the CPU.

**Listen to each stem:**

* **vocals**: the voice alone. Expect some cymbal "wash" and reverb tails from other parts
  (bleed); a little is normal.
* **drums**: kick, snare, and cymbals. Listen for vocal "s" sounds leaking in.
* **bass**: should be solid in the low end; very low synth parts sometimes land in other.
* **other**: everything else (guitars, keys, pads, synth leads).

The `quality_*` numbers are the model's own confidence per stem (0 to 1). Our test song is a
synthesized instrumental, so `quality_vocals` is 0.018: there is no voice, and the lead synth
went to other. A low score on a stem you know is present means that stem needs a careful
listen.

## 3. The stems add back to the song

With `--sum-to-mix on` (the default) whatever the model missed goes into `other`, so the four
files add back to the original exactly. You can check that:

```bash
ferraus mix song.vocals.wav song.drums.wav song.bass.wav song.other.wav -o sum.wav
ferraus compare song.wav sum.wav
```

```json
{"rms_diff_db": -240.0, "si_sdr_db": 200.0, "identical": true, "verdict": "identical"}
```

## 4. Common variations

```bash
ferraus separate song.wav --tier fast --stems 2 --outdir two         # vocals + instrumental
ferraus separate song.wav --tier fast --instrumental --outdir karaoke # one file with the vocals removed
ferraus separate song.wav --tier fast --only drums,bass --outdir rhythm
ferraus separate song.wav --tier fast --stems 6 --outdir six         # adds guitar and piano
ferraus separate song.wav --tier fast --drum-split --outdir kit      # drums → kick, snare, toms, cymbals
```

```text
two/song.vocals.wav  two/song.instrumental.wav
karaoke/song.instrumental.wav
rhythm/song.drums.wav  rhythm/song.bass.wav
six/song.vocals.wav  six/song.drums.wav  six/song.bass.wav  six/song.guitar.wav  six/song.piano.wav  six/song.other.wav
kit/song.vocals.wav ... kit/song.kick.wav  kit/song.snare.wav  kit/song.cymbals.wav  kit/song.toms.wav
```

`--stems 2 --tier quality` runs only the RoFormer vocal model, so a clean karaoke track costs
about two thirds of a full `quality` run. `--drum-split` downloads the DrumSep model (335 MB) on
first use; it has no separate hi-hat output (hi-hats land in cymbals), and `--dry-run` lists
the extra outputs before anything runs.

`--outdir` keeps each run's files apart; without it, a second run stops with
`output_exists` instead of overwriting (pass `--overwrite` to replace).

## 5. Long songs, agents, and batches

Separation is the slowest command in ferraus. Memory does not grow with the song: models run
one at a time over the song window by window, and an hour-long input spills its intermediate
signals to temporary files (removed afterwards) so it stays under 4 GB. `--max-memory 8GB`
lets ferraus keep everything in memory and load models in their fastest mode when you have the
RAM. Run long jobs in the background and collect the result later:

```bash
ferraus separate song.wav --tier fast --outdir stems --async --json
```

```json
{"ok": true, "job_id": "1792400395782-593d"}
```

```bash
ferraus jobs status 1792400395782-593d   # state, percent
ferraus jobs wait 1792400395782-593d     # blocks, then prints the separate result
```

## What next

* Make a remix from the stems: [Stems for a remix](../cookbook/stems-for-remix.md).
* Transcribe a stem to MIDI (`ferraus transcribe song.bass.wav -o bass.mid`), or notate it:
  [Make a score](make-a-score.md).
* Tune the vocal stem: [Tune a vocal](tune-a-vocal.md).

## Reference

[separate](../commands/separate.md) · [models list](../commands/models_list.md) ·
[models pull](../commands/models_pull.md) · [compare](../commands/compare.md) ·
[jobs wait](../commands/jobs_wait.md)
