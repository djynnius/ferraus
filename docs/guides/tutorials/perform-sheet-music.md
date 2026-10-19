# Perform a sheet-music file

`ferraus score perform` reads written music and plays it: it turns the notation into a MIDI
performance (repeats unrolled, dynamics and articulations applied) and renders that with an
instrument. This walkthrough uses ABC and MusicXML files. Outputs are trimmed copies of what
ferraus 0.1.0 printed.

**You need:** a notation file: MusicXML (`.musicxml`, `.xml`, `.mxl`), ABC (`.abc`), MEI,
LilyPond (`.ly`), MIDI, or ferraus score JSON. Most notation apps export MusicXML, and
thousands of folk tunes are published as ABC.

**Printed sheet music (PDF or photos)** needs optical music recognition (`ferraus score read`),
which uses the oemer and homr models. Their downloads are not verified in this release, so
recognition stops with exit code 5 or 6:

```json
{"ok": false, "error": {"code": "model_not_cached",
  "message": "the builtin OMR models (oemer segmentation + homr) are not in the model cache",
  "hint": "ferraus models pull omr/oemer", "exit_code": 5}}
{"ok": false, "error": {"code": "model_unverified",
  "message": "omr/oemer@checkpoints-2021-11-25 has no verified download (status: unverified, no SHA-256 recorded); refusing to fetch an unchecked file",
  "exit_code": 6}}
```

Until those models are published, export MusicXML from the app that produced the PDF, or scan
the page with a dedicated OMR app (Audiveris, MuseScore's import, PlayScore) and feed the
MusicXML to ferraus. `--engine audiveris` uses an installed Audiveris
(`ferraus doctor --install-audiveris`).

## 1. Write or get a score

Save this as `tune.abc` (the first lines of Beethoven's *Ode to Joy*):

```text
X:1
T:Ode to Joy (excerpt)
C:Beethoven
M:4/4
L:1/4
Q:1/4=96
K:D
!mf! F F G A | A G F E | D D E F | F3/2 E/ E2 |
F F G A | A G F E | D D E F | !p! E3/2 D/ D2 |]
```

Check that ferraus reads it the way you meant:

```bash
ferraus score check tune.abc
```

```json
{"ok": true, "errors": 0, "warnings": 0, "parts": 1, "measures": 8, "notes": 30, "issues": [], "failing_measures": []}
```

`check` finds measures whose durations do not add up, voices that overlap, and constructs
ferraus cannot play, with the measure number for each.

## 2. Perform it

```bash
ferraus score perform tune.abc --instrument piano -o tune.wav --json
```

```json
{"ok": true, "command": "score perform",
 "outputs": [{"path": "tune.musicxml", "role": "musicxml"}, {"path": "tune.mid", "role": "midi"},
             {"path": "tune.wav", "role": "audio", "duration_s": 22.0}],
 "params": {"tempo": "auto", "repeats": "on", "dynamics": "on", "articulation": "on", "humanize": "0%", ...},
 "result": {"engine": "none (abc input)",
            "performance": {"duration_s": 20.0, "notes": 30, "measures_played": 8, "order": [1, 2, 3, 4, 5, 6, 7, 8], "tempo_bpm": 96.0},
            "render": {"instrument": "builtin:piano", ...}}}
```

Beside the audio you get the MusicXML and the MIDI performance, so you can open either in
another program. `order` lists the measures in the order they were played, which is where
you check that repeats and first/second endings were followed.

**Listen for:** the tempo marking (96 here; `--tempo 120` overrides it), the `!mf!` to `!p!`
dynamic change in the last line, and the dotted rhythm in bars 4 and 8.

## 3. Shape the performance

```bash
ferraus score perform tune.abc --tempo 120 --swing 50% --humanize 30% --seed 7 --instrument strings -o tune-strings.wav
```

* `--humanize 30%` adds small random timing and velocity changes (seeded, so the same seed
  gives the same performance).
* `--swing 50%` delays the off-beat eighths halfway to a triplet feel.
* `--instrument-map "P1=piano,P2=strings"` chooses an instrument per part in a multi-part score.
* `--repeats off`, `--dynamics off`, and `--articulation off` switch each interpretation step off.
* `--click on` adds a metronome track.

For better instruments than the built-in synth, install the General MIDI sound bank once
(31 MB) and use General MIDI names:

```bash
ferraus sounds install gm-basic -y
ferraus score perform melody.musicxml --instrument "Acoustic Grand Piano" -o melody.wav
```

## 4. Just the MIDI, or another format

```bash
ferraus score play tune.abc -o tune-perf.mid --click on      # the performance as MIDI, no audio
ferraus score convert tune.abc -o tune.musicxml              # ABC to MusicXML
ferraus score to-json tune.abc -o tune.score.json            # the editable JSON model
```

The JSON model is what an agent edits: change a note's pitch or duration, then
`ferraus score from-json tune.score.json -o fixed.musicxml` and perform it again. Small
corrections can also be applied as an edit list with
`ferraus score read tune.musicxml --fix edits.json -o fixed.musicxml`.

## Reference

[score perform](../commands/score_perform.md) · [score play](../commands/score_play.md) ·
[score check](../commands/score_check.md) · [score read](../commands/score_read.md) ·
[score convert](../commands/score_convert.md) · [score to-json](../commands/score_to_json.md) ·
[render](../commands/render.md)
