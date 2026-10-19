# Sheet music to a performance

An agent recipe: take a notation file, validate it, fix problems in the score model, and
render a performance. Run with ferraus 0.1.0; JSON is trimmed. The plain-language version is
the [tutorial](../tutorials/perform-sheet-music.md).

**Printed scores:** recognising a PDF or photo (`score read`, `score perform page.pdf`) needs
the oemer/homr models, which have no verified download in this release (exit code 5, then 6
with `FERRAUS_ALLOW_UNVERIFIED=1` as the hint). Ask the user for MusicXML, or use
`--engine audiveris` with an installed Audiveris. Everything below works on MusicXML, MXL,
ABC, MEI, LilyPond, MIDI, and ferraus score JSON.

## 1. Validate

```bash
ferraus score check tune.abc --json
```

```json
{"ok": true, "command": "score check",
 "result": {"ok": true, "errors": 0, "warnings": 0, "parts": 1, "measures": 8, "notes": 30,
            "issues": [], "failing_measures": []}}
```

**Decide:** if `failing_measures` is not empty, each issue names the measure, voice, and what
is wrong (for example, a measure whose durations do not add up to the time signature). Fix
those before performing; `--strict` turns warnings into a failing exit code.

## 2. Read and edit the score model

```bash
ferraus score to-json tune.abc -o tune.score.json
```

```json
{"version": 1, "title": "Ode to Joy (excerpt)", "composer": "Beethoven", "divisions": 2,
 "parts": [{"id": "P1", "name": "Voice 1", "program": 0,
   "staves": [{"number": 1, "measures": [{"number": 1, "key": {"fifths": 2, "mode": "major"},
     "time": {"beats": 4, "beat_type": 4}, "clef": {"sign": "G", "line": 2}, "tempo": [{"offset": 0, "bpm": 96.0}],
     "voices": [{"number": 1, "events": [
       {"duration": 2, "notes": [{"pitch": {"step": "F", "alter": 1, "octave": 4}, "midi": 66}], "dynamic": "mf"}, ...]}]}]}]}]}
```

`duration` is in `divisions` per quarter note (here 2, so 2 = a quarter note). Edit pitches,
durations, dynamics, or the tempo, then write it back:

```bash
ferraus score from-json tune.score.json -o tune.fixed.musicxml
```

For a few targeted corrections, an edit list is shorter than the whole model:

```bash
ferraus score read tune.musicxml --fix edits.json -o tune.fixed.musicxml --review
```

where `edits.json` is `[{"measure": 4, "voice": 1, "note": 1, "set": {"pitch": "F#4"}}]`.

## 3. Perform

```bash
ferraus score perform tune.abc --instrument piano -o tune.wav --json
```

```json
{"ok": true, "command": "score perform",
 "outputs": [{"path": "tune.musicxml", "role": "musicxml"}, {"path": "tune.mid", "role": "midi"},
             {"path": "tune.wav", "role": "audio", "duration_s": 22.0}],
 "result": {"engine": "none (abc input)",
            "performance": {"duration_s": 20.0, "notes": 30, "measures_played": 8, "order": [1, 2, 3, 4, 5, 6, 7, 8], "tempo_bpm": 96.0, "tracks": 1},
            "render": {"instrument": "builtin:piano", ...}}}
```

**Checks:** `performance.order` is the measure sequence after unrolling repeats and endings;
compare it with the form you expect. `performance.tempo_bpm` is the tempo used (`--tempo`
overrides the score). `render.instrument` shows what played it.

Variations: `--instrument-map "P1=piano,P2=strings"` per part, `--humanize 30% --seed 7` for
a less mechanical but reproducible take, `--swing 50%`, `--click on` for a metronome track,
`ferraus score play` for the MIDI alone.
