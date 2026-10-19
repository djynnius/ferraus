# Hum to an instrument

An agent recipe: turn a hummed idea into a rendered instrument part, check the notes, and fix
them without asking the user to hum again. Run with ferraus 0.1.0; JSON is trimmed. The
plain-language version is the [tutorial](../tutorials/hum-to-instrument.md).

## 1. Transcribe and inspect

Transcribe first instead of calling `hum` directly: the note list is what you check.

```bash
ferraus transcribe idea.wav -o idea.mid --json
```

```json
{"ok": true, "command": "transcribe",
 "outputs": [{"path": "idea.mid"}, {"path": "idea.notes.json", "role": "notes"}],
 "params": {"bpm": "99.84 (audio)", "key": "A minor (confidence 0.91; snapped)", "mode": "mono", ...},
 "metrics": {"notes": 11, "tempo_confidence": 1.0, "mean_note_confidence": 0.995, "polyphonic_fraction": 0.022},
 "result": {"mode": "mono", "engine": "pyin", "notes": 11, "bpm": 99.84, "key": "A minor", "key_confidence": 0.91,
            "key_snapped": true, "notes_snapped": 0, "pitch_range": ["A4", "E5"],
            "polyphony": {"monophonic": true, ...}, "output": "idea.mid", "notes_output": "idea.notes.json"}}
```

`idea.notes.json`:

```json
{"ferraus_notes": 1, "source": "idea.wav", "mode": "mono", "bpm": 99.84, "key": "A minor",
 "notes": [{"start_s": 0.01, "end_s": 0.6197, "duration_s": 0.6097, "start_beats": 0.0167,
            "pitch": 76, "name": "E5", "velocity": 92, "confidence": 0.993, ...}, ...]}
```

**Checks an agent can make:**

* `result.polyphony.monophonic` false: the recording has chords or background music; use
  `--mode poly` or separate the voice first.
* Fewer notes than the user expects, with long `duration_s`: repeated notes merged. Re-run with
  `--onset-sensitivity 100%` (in our test: 11 notes at 50%, 13 at 90%, all 16 at 100%).
* Many very short notes: a wobbly hum split into fragments. Lower `--onset-sensitivity` or
  raise `--min-note 120ms`.
* Low `confidence` on a note: ask the user about that note, or drop it.

## 2. Render

```bash
ferraus hum idea.wav --to piano --onset-sensitivity 100% -o idea-piano.wav --json
```

```json
{"ok": true, "command": "hum",
 "outputs": [{"path": "idea-piano.mid", "role": "midi"}, {"path": "idea-piano.wav", "duration_s": 11.77}],
 "result": {"notes": 16, "bpm": 99.84, "key": "A minor", "quantize_grid": "1/16", "quantize_strength": 0.8,
            "render": {"instrument": "builtin:piano", "plan": [{"source": {"type": "built_in", "name": "piano"}, "notes": 16}]}}}
```

`result.render.instrument` says what actually played the part. `builtin:*` means no sound bank
is installed; `ferraus sounds install gm-basic -y` (31 MB, free license) makes every General
MIDI name play a sampled instrument.

## 3. Edit the MIDI, not the audio

To fix notes, convert the MIDI to JSON, edit it, and convert it back:

```bash
ferraus midi to-json idea-piano.mid -o idea.json
# edit tracks[0].notes[i].pitch / name / beat / duration_beats
ferraus midi from-json idea.json -o idea-fixed.mid
ferraus render idea-fixed.mid --instrument strings -o idea-strings.wav
```

Minimal JSON is enough for `from-json`; positions can be `beat` (wins), `tick`, or `time_s`:

```json
{"tempo_map": [{"beat": 0, "bpm": 100}],
 "tracks": [{"notes": [{"beat": 0, "duration_beats": 1, "name": "C4", "velocity": 100}]}]}
```

Other one-step edits: `midi transpose --semitones -12`, `midi quantize --grid 1/8 --strength 100%`,
`midi velocity --set 90`, `midi legato`, `midi tempo --set 92`.

Over MCP, the `hum-to-instrument` prompt guides a model through the same steps.
