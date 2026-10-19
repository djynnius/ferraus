# Tune a vocal

Pitch correction moves each sung note toward the nearest note of a key (or toward a written
melody) while keeping the singer's tone. This walkthrough goes from "find out what key it is"
to a natural correction, then the deliberate robotic effect. Outputs are trimmed copies of
what ferraus 0.1.0 printed on a test take.

**You need:** a dry, solo vocal (`vocal.wav`): one voice, no backing track, little reverb.
Pitch tracking follows one note at a time, so a vocal with music underneath gives poor
results; split it out first with [Split a song into stems](split-a-song-into-stems.md).

> **Making a test take.** macOS can sing: `say -v Cellos -o sung.aiff "la la la la la la"`.
> Convert it and push it out of tune by 35 cents so there is something to correct:
>
> ```bash
> ferraus convert sung.aiff -o sung48.wav --sample-rate 48000
> ferraus pitch sung48.wav --cents 35 -o vocal.wav
> ```

## 1. Listen and look at the pitch

**Listen for:** notes that sound sour against where the song sits, especially long held notes
and the ends of phrases. Then look at what the pitch tracker hears:

```bash
ferraus analyze pitch vocal.wav -o curve.csv --json
```

```json
{"ok": true, "command": "analyze pitch",
 "metrics": {"voiced_fraction": 0.9908, "notes": 12},
 "result": {"summary": {"tracker": "pyin", "voiced_fraction": 0.9908, "median_hz": 104.8854, "median_note": "G#2", ...},
            "notes": [{"start_s": 0.01, "end_s": 0.35, "midi": 39.2647, "note": "D#2", ...}, ...]}}
```

`curve.csv` has one row per 10 ms (`time_s,freq_hz,midi,confidence,voiced,rms`) and opens in
any spreadsheet. A `midi` value of 39.26 means the note is 26 cents sharp of D#2.

## 2. Find the key

```bash
ferraus analyze key vocal.wav --method notes
```

```text
warning: key confidence 0.32 is below 0.6; 'Bb minor' is only a best guess
```

```json
{"key": "Bb minor", "confidence": 0.315, "alternatives": [{"key": "Bb major", ...}, {"key": "A major", ...}],
 "method": "notes", "reliable": false}
```

`--method notes` works from the sung notes, which suits a solo vocal; the default (`chroma`)
suits a full mix. A short phrase rarely contains enough notes to pin down a key, which is what
this warning says. If you know the song's key, always pass it; if you have the full mix, run
`ferraus analyze key song.wav` on the mix instead.

## 3. Correct it

With no options, `tune` detects the key and corrects every note fully. When the key
confidence is below 0.6 it falls back to the chromatic scale (every semitone is a valid
target) and tells you, so nothing is pulled to a wrong key:

```bash
ferraus tune vocal.wav --notes-out notes.json -o vocal.tuned.wav --json
```

```text
warning: key detection confidence 0.32 is below 0.6 (best guess Bb minor); correcting to the chromatic scale. Pass --key to choose the key
```

```json
{"ok": true, "command": "tune",
 "params": {"key": "Bb minor (detected, confidence 0.32; not used)", "scale": "chromatic", "strength": "100%", "retune_speed": "25ms", ...},
 "metrics": {"notes": 12, "corrected_notes": 12, "mean_abs_deviation_cents": 21.49, "max_abs_deviation_cents": 27.02, "resynthesis": "td_psola"}}
```

`notes.json` lists every note with what was heard and where it was moved:

```json
{"start_s": 0.01, "end_s": 0.35, "note": "D#2", "detected_hz": 78.98, "target_note": "D#2", "deviation_cents": -26.47, "confidence": 0.67}
```

**Listen for:** held notes now sit in tune, and the voice still sounds like the same person.
Listen hard at note changes: a "warble" or a step in the middle of a slide means the
correction is too fast for this singer.

When you know the key, give it, and back the correction off so the performance keeps some
life:

```bash
ferraus tune vocal.wav --key C --scale major --strength 80% --retune-speed 60ms -o vocal.gentle.wav
```

* `--strength 80%` moves each note 80% of the way to its target.
* `--retune-speed 60ms` glides into the correction instead of snapping (0 ms snaps).
* Vibrato is left alone by default (`--vibrato-preserve on`), so a wobble on a long note is
  not flattened.
* `--humanize 20%` adds a few cents of slow random drift if a fully corrected take sounds
  sterile; it is seeded by `--seed`, so the result is reproducible.

## 4. Tune to a written melody

If you have the melody as MIDI (from a songwriter's demo, or from `ferraus transcribe` of a
guide vocal), tune each note to the written note rather than the nearest note of a scale:

```bash
ferraus tune vocal.wav --target melody.mid --tolerance 100cents --notes-out notes.json -o vocal.tuned.wav
```

Notes more than `--tolerance` away from the written note are left alone and reported, so an
intentional blue note or ad-lib is not dragged somewhere it was never meant to go.

## 5. The hard-tune effect

```bash
ferraus tune vocal.wav --effect hard --key C -o vocal.robot.wav
```

`--effect hard` sets an instant retune, full strength, and no humanize: the stepped,
"robotic" sound used as a deliberate effect in pop and hip-hop. **Listen for:** clean jumps
between notes with no glide.

## Noisy or breathy takes

The built-in tracker (pYIN) is good on clean takes. On breathy, quiet, or noisy takes a
neural tracker can follow the pitch better. They download once with `-y`:

```bash
ferraus tune vocal.wav --tracker crepe -y --key C -o vocal.crepe.wav      # CREPE tiny, 2 MB
ferraus tune vocal.wav --tracker swiftf0 -y --key C -o vocal.swift.wav    # SwiftF0, 135 kB
```

Compare `metrics.notes` and the `notes.json` reports between trackers: on our synthetic test
take CREPE split the line into 24 notes where pYIN found 12, so more notes is not always
better. Pick the one whose notes match what you hear.

## Reference

[tune](../commands/tune.md) · [analyze pitch](../commands/analyze_pitch.md) ·
[analyze key](../commands/analyze_key.md) · [pitch](../commands/pitch.md) ·
[transcribe](../commands/transcribe.md)
