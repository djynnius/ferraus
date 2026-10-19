# Turn a hum into an instrument part

Hum, sing, or whistle a melody into your phone, and ferraus turns it into a MIDI part played
by a real instrument sound. This walkthrough shows the one-step command, then the steps
underneath it so you can fix wrong notes. Outputs are trimmed copies of what ferraus 0.1.0
printed.

**You need:** a recording of one melody line (`idea.wav`): humming, "la la la", whistling, or
a single instrument. Record somewhere quiet, close to the microphone, one note at a time. No
downloads are needed.

> **Making a test file.** Without a recording, render a melody with a soft lead sound and
> roll off its brightness so it behaves roughly like a hum (`melody.mid` is any one-line
> MIDI melody; see [Make a score](make-a-score.md) for how to write one from JSON):
>
> ```bash
> ferraus render melody.mid --instrument builtin:lead --tail 0.5s -o lead.wav
> ferraus fx lead.wav -o idea.wav --fx "lowpass:freq=900Hz" --fx "tremolo:rate=5.5,depth=15%" --channels 1
> ```

## 1. One step: hum to piano

```bash
ferraus hum idea.wav --to piano -o idea-piano.wav --json
```

```json
{"ok": true, "command": "hum",
 "outputs": [{"path": "idea-piano.mid", "role": "midi"}, {"path": "idea-piano.wav", "duration_s": 11.77}],
 "params": {"bpm": "99.84 (audio)", "grid": "1/16", "strength": "80%", "key": "A minor (confidence 0.91; snapped)", "to": "piano", ...},
 "metrics": {"notes": 11, "tempo_confidence": 1.0, "mean_note_confidence": 0.995},
 "result": {"mode": "mono", "engine": "pyin", "bpm": 99.84, "key": "A minor", "pitch_range": ["A4", "E5"],
            "midi": "idea-piano.mid", "audio": "idea-piano.wav", "instrument": "piano", ...}}
```

ferraus tracked the pitch, found the tempo (99.84 BPM; the melody was played at 100) and the
key, split the line into notes, nudged them 80% of the way onto a sixteenth-note grid, and
played them with a piano. It also wrote the MIDI part, `idea-piano.mid`, so you can edit or
re-voice it.

**Listen for:** the right notes in the right order, and the rhythm of your hum (not a
machine-straight version of it, unless you asked for that).

## 2. Missing notes? Split repeated notes

Our test melody has 16 notes but ferraus found 11. The missing ones are **repeated notes**:
when you hum the same pitch twice without a clear break, it sounds like one long note.
`--onset-sensitivity` controls how readily a note is split on a small dip in level or pitch:

```bash
ferraus transcribe idea.wav -o idea.mid                            # 11 notes (default 50%)
ferraus transcribe idea.wav --onset-sensitivity 90% -o idea90.mid  # 13 notes
ferraus transcribe idea.wav --onset-sensitivity 100% -o idea100.mid
```

The note lists, from each `.notes.json` file:

```text
50%:  E5 D5 C5 D5 E5 C5 B4 A4 B4 C5 A4
90%:  E5 D5 C5 D5 E5 E5 C5 B4 A4 B4 C5 A4 A4
100%: E5 D5 C5 D5 E5 E5 E5 C5 C5 B4 A4 B4 C5 C5 A4 A4    (all 16)
```

`hum` takes the same flag: `ferraus hum idea.wav --to piano --onset-sensitivity 100% -o idea-piano.wav`.
Going the other way, if a wobbly hum is split into extra short notes, lower the sensitivity
or raise `--min-note 120ms`. Humming each note on "da" or "ta" instead of "mm" gives the
clearest onsets of all.

## 3. Check every note

`transcribe` writes a JSON list beside the MIDI file with a confidence for every note:

```bash
ferraus transcribe idea.wav -o idea.mid --json
```

```json
{"ok": true, "command": "transcribe",
 "outputs": [{"path": "idea.mid"}, {"path": "idea.notes.json", "role": "notes"}],
 "result": {"mode": "mono", "notes": 11, "bpm": 99.84, "key": "A minor", "key_confidence": 0.91,
            "key_snapped": true, "notes_snapped": 0, "pitch_range": ["A4", "E5"], "mean_confidence": 0.995, ...}}
```

```json
{"start_s": 0.01, "end_s": 0.6197, "pitch": 76, "name": "E5", "velocity": 92, "confidence": 0.993, ...}
```

Notes with low confidence are the ones to listen to. Out-of-key notes are moved to the
nearest note of the key when the key confidence is above 0.6 (`--key-snap off` keeps them).

## 4. Choose the instrument, tempo, and feel

```bash
ferraus hum idea.wav --to strings --grid 1/8 --strength 100% --bpm 100 -o idea-strings.wav
ferraus hum bass-idea.wav --to "Acoustic Bass" --bpm 92 -o bass.wav
```

* `--to` takes any instrument name `render` understands: `piano`, `strings`, a General MIDI
  name such as `"Acoustic Bass"` or `"Flute"`, `gm:48`, an installed sound, or a plugin
  (`plugin:"Surge XT"`). With no sounds installed, a built-in synth plays it; install the
  free General MIDI sound bank for much better instruments: `ferraus sounds install gm-basic -y`
  (31 MB).
* `--grid 1/8 --strength 100%` snaps fully to eighth notes (tight and mechanical);
  `--grid off` keeps your timing exactly.
* `--bpm` fixes the tempo instead of detecting it, so the part lines up with a song at that
  tempo.
* `--octave auto` (the default) moves the part into the instrument's comfortable range, so a
  hummed bass line plays in the bass register.

## 5. Re-voice the MIDI without re-humming

```bash
ferraus render idea-piano.mid --instrument "Acoustic Bass" -o idea-bass.wav
ferraus midi transpose idea-piano.mid --semitones -12 -o idea-low.mid
ferraus midi quantize idea-piano.mid --grid 1/8 --strength 100% -o idea-tight.mid
```

Any DAW can open the `.mid` files too.

## Reference

[hum](../commands/hum.md) · [transcribe](../commands/transcribe.md) ·
[render](../commands/render.md) · [midi quantize](../commands/midi_quantize.md) ·
[midi transpose](../commands/midi_transpose.md) · [sounds install](../commands/sounds_install.md)
