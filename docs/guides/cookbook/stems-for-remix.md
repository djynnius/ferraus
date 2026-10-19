# Stems for a remix

An agent recipe: split a song into stems, move them to a new tempo and key, process each
stem, mix a remix, cut loops, and master it loud for a DJ set. Run with ferraus 0.1.0 on a
21-second, 100 BPM test song; JSON is trimmed.

## 1. Separate

```bash
ferraus separate song.wav --tier fast -y --json
```

```json
{"ok": true, "command": "separate",
 "outputs": [{"path": "song.vocals.wav", "role": "vocals"}, {"path": "song.drums.wav", "role": "drums"},
             {"path": "song.bass.wav", "role": "bass"}, {"path": "song.other.wav", "role": "other"}],
 "metrics": {"quality_vocals": 0.018, "quality_drums": 0.901, "quality_bass": 0.516, "quality_other": 0.869},
 "result": {"stems": [{"stem": "vocals", "path": "song.vocals.wav", "quality": {"score": 0.018, "method": "mask_confidence", "calibrated": false}}, ...],
            "sum_to_mix": {"applied": true, "target": "other", "residual_peak_db": -15.3}}}
```

**Decide:** read `result.stems[].quality.score`; skip or flag stems that score low (this test
song has no vocals, hence 0.018). This recipe uses `--tier fast` for speed; the default
`--tier auto` picks `quality` (RoFormer vocals, about 8× slower; see
[Stems and separation tiers](../concepts/stems.md)), worth it when the vocal is the hook of
the remix. For a long song, add `--async` and poll `ferraus jobs wait <job_id> --json`; memory
stays bounded however long the song is. Need the kit apart for a flip? Add `--drum-split` for
kick, snare, toms, and cymbals stems.

## 2. Measure tempo and key

```bash
ferraus analyze tempo song.drums.wav --json      # result.bpm: 100.0, result.confidence: 1.0
ferraus analyze key song.other.wav --json        # result.key: "C major", result.confidence: 0.623
```

Tempo is most reliable on the drum stem and key on the harmonic stem ("other" or "piano").

## 3. New tempo and key

```bash
ferraus stretch song.drums.wav --bpm 100 --to-bpm 110 -o remix/drums110.wav --json
ferraus stretch song.bass.wav --bpm 100 --to-bpm 110 -o remix/bass110.wav
ferraus stretch song.other.wav --bpm 100 --to-bpm 110 -o remix/other110.wav
ferraus pitch remix/bass110.wav --semitones 2 -o remix/bass110-up2.wav
ferraus pitch remix/other110.wav --semitones 2 -o remix/other110-up2.wav
```

```json
{"ok": true, "command": "stretch", "metrics": {"factor": 0.9091, "engine": "hybrid", "transients": 64, "anchored_transients": 64}}
```

`stretch` keeps pitch and `pitch` keeps duration. Do not pitch-shift the drums: they have no
key, and shifting them changes their character. `anchored_transients` equal to `transients`
means every drum hit stayed sharp.

## 4. Process each stem

```bash
ferraus fx remix/drums110.wav --fx "saturate:type=tape,drive=6dB" --fx "highpass:freq=40Hz" -o remix/drums-fx.wav
ferraus fx remix/other110-up2.wav --fx chorus --fx "delay:time=1/8D,feedback=35%,mix=25%" --bpm 110 -o remix/other-fx.wav
```

Beat-based values (`1/8D`, a dotted eighth) need `--bpm` with the new tempo.

## 5. Mix

```bash
ferraus mix remix/drums-fx.wav remix/bass110-up2.wav remix/other-fx.wav --gains 0dB,-2dB,-4dB --pans 0,0,+20 -o remix/remix.wav --json
```

```json
{"ok": true, "command": "mix", "metrics": {"peak_db": -9.27, "duration_s": 19.27}}
```

## 6. Cut loops

```bash
ferraus split remix/remix.wav --every 4b --bpm 110 -o 'remix/loops/loop{stem}.wav' --json
```

```json
{"ok": true, "command": "split",
 "outputs": [{"path": "remix/loops/loop001.wav", "role": "001", "duration_s": 2.179},
             {"path": "remix/loops/loop002.wav", "role": "002", "duration_s": 2.183}, ...]}
```

In an output template, `{stem}` is each part's role (`001`, `002`, ...) and `{index}` is the
position of the *input* in a batch, so use `{stem}` to number the parts of one file. Without
`-o`, `--outdir loops` names them `remix.001.wav`, `remix.002.wav`, .... Split points snap to
the nearest zero crossing within 5 ms (`--snap off` for exact samples), which is why the loops
are a few samples apart in length.

## 7. Master for a DJ set

```bash
ferraus master remix/remix.wav --target club --genre edm -o remix/remix.master.wav --json
```

```json
{"ok": true, "command": "master",
 "metrics": {"integrated_lufs": -13.15, "true_peak_dbtp": -0.32, "limiter_gain_reduction_db": 4.0,
             "target_reached": false, "verify_pass": false},
 "warnings": [{"code": "target_not_reached", "message": "landed at -13.2 LUFS, 5.2 LU short of the -8.0 LUFS target: --intensity medium allows at most 4 dB of limiter gain reduction and reaching the target needs more. Use --intensity heavy, or accept the quieter master"},
              {"code": "verify_failed", "message": "master verify failed: loudness: integrated loudness -13.15 LUFS is -5.15 LU from the -8 LUFS target"}]}
```

**Decide:** `target_reached: false` with a `target_not_reached` warning that names the fix. Either re-run with
`--intensity heavy` (more limiting, less punch) or accept -13 LUFS. ferraus never limits harder
than the intensity allows on its own.
