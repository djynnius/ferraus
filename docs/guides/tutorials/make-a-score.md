# Make a score from a song or a MIDI file

`ferraus score` writes readable sheet music: a lead sheet with chord symbols, a grand staff
for piano, a drum score, or tonic sol-fa. MusicXML is the main output; MuseScore, Finale,
Sibelius, Dorico, and most notation apps open it. This walkthrough starts from MIDI (the
reliable path), then goes from a recording. Outputs are trimmed copies of what ferraus 0.1.0
printed.

**You need:** a MIDI file, or a recording plus the separation model (see
[Split a song into stems](split-a-song-into-stems.md)). PDF, SVG, and PNG pages also need an
engraver (step 4).

## 1. A melody to a lead sheet

> **Making a test file.** A MIDI file can be written from a few lines of JSON:
>
> ```bash
> cat > melody.json <<'EOF'
> {"tempo_map": [{"beat": 0, "bpm": 100}],
>  "tracks": [{"notes": [
>    {"beat": 0, "duration_beats": 1, "name": "E5", "velocity": 100},
>    {"beat": 1, "duration_beats": 1, "name": "D5", "velocity": 100},
>    {"beat": 2, "duration_beats": 1, "name": "C5", "velocity": 100},
>    {"beat": 3, "duration_beats": 1, "name": "D5", "velocity": 100}]}]}
> EOF
> ferraus midi from-json melody.json -o melody.mid
> ```
>
> The examples below used a 16-note version of this melody.

```bash
ferraus score melody.mid -o melody.musicxml --title "Little Tune" --svg melody.svg --abc melody.abc --solfa-txt melody.solfa.txt --json
```

```json
{"ok": true, "command": "score",
 "outputs": [{"path": "melody.musicxml", "role": "musicxml"}, {"path": "melody.abc", "role": "abc"},
             {"path": "melody.solfa.txt", "role": "solfa"}, {"path": "melody.svg", "role": "svg"}],
 "params": {"engraver": "native", "key": "A minor", "time": "4/4", "quantize": "1/16", ...},
 "result": {"measures": 4, "check": {"ok": true, "errors": 0, "warnings": 0}, "low_confidence": [],
            "notation": {"key": "A minor", "key_source": "estimated", "key_confidence": 1.0, "tempo_bpm": 100.0,
                         "parts": [{"id": "P1", "name": "Piano", "layout": "lead-sheet", "notes": 16}]}}}
```

The ABC and sol-fa outputs are plain text:

```text
X:1
T:Little Tune
M:4/4
L:1/8
Q:1/4=100
K:Am
e2 d2 c2 d2 | e2 e2 e2 c2 | c2 B2 A2 B2 | c2 c2 A2 A2 |]
```

```text
Little Tune
Key: Doh is C (A minor, lah is A)
Time: 4/4
Tempo: 100 bpm

|m :r :d :r |m :m :m :d |d :t, :l, :t, |d :d :l, :l, ||
```

**Check:** open `melody.musicxml` in a notation app (or `melody.svg` in a browser) and play
it back. The key was *estimated* from the notes (`key_source`); this melody sits equally well
in C major and its relative minor, A minor, and ferraus chose A minor. If you know the key,
give it: `--key C`. Notes are snapped to `--quantize 1/16` (use `1/8` for simpler rhythms,
`1/32` for fast passages).

Sol-fa only, with no staff (`--solfa only`), or fixed do (`--solfa fixed`):

```bash
ferraus score melody.mid --solfa only -o melody-solfa.txt
```

Transpose for another instrument or singer, with guitar chord diagrams:

```bash
ferraus score melody.mid --chords diagrams --transpose 3 -o lead.musicxml
```

## 2. A full arrangement

A multi-track MIDI file becomes one part per track: the drum channel (10) gets a drum score,
wide-range parts get a grand staff, and the rest get lead sheets (in the bass clef when low).

```bash
ferraus score song.mid -o song.musicxml --midi song.score.mid --json
```

```json
{"result": {"parts": [{"id": "P1", "name": "lead", "staves": 1}, {"id": "P2", "name": "piano", "staves": 2},
                      {"id": "P3", "name": "bass", "staves": 1}, {"id": "P4", "name": "drums", "staves": 1, "percussion": true}],
            "measures": 8, "check": {"ok": true, "errors": 0, "warnings": 0},
            "notation": {"key": "C major", "key_confidence": 0.9, "time": "4/4", "tempo_bpm": 100.0,
                         "chords": ["C", "Am", "F", "G", "Gadd9", "C", "Am", "F", "G"]}}}
```

`--midi song.score.mid` also writes the MIDI performance *of the notation*, so you can hear
exactly what was notated. Validate any score file with:

```bash
ferraus score check song.musicxml
```

```json
{"ok": true, "errors": 0, "warnings": 0, "parts": 4, "measures": 8, "notes": 184, "issues": [], "failing_measures": []}
```

Force a layout when the automatic choice is wrong: `--layout grand --split C4` for piano
with a fixed hand split, `--layout drums`, `--layout bass`, `--layout lead-sheet`.

## 3. From a recording

### The one-command route

```bash
ferraus score take.wav --parts vocals,bass,drums -o take.musicxml --midi take.mid -y
```

This runs `analyze tempo`, `analyze key`, `analyze chords`, `separate`, `transcribe` per
stem, and a native drum transcription, then notates. `--dry-run` shows that plan. It separates
with the default tier (`balanced` in this release: four fine-tuned Demucs models, about 660 MB
on the first run; see [Stems and separation tiers](../concepts/stems.md)). Measured with the
`quality` tier (the Mel-Band RoFormer for the vocals) on a 7-second excerpt of a MUSDB18 test
song, it took 52 s on a laptop CPU and peaked at 3.2 GB of memory:

```json
{"parts": [{"id": "P1", "name": "Vocals"}, {"id": "P2", "name": "Bass"}, {"id": "P3", "name": "Drums", "percussion": true}],
 "measures": 6, "check": {"ok": true, "errors": 0, "warnings": 0},
 "audio": {"stems": [{"role": "vocals", "notes": 20, "mode": "mono"}, {"role": "bass", "notes": 16, "mode": "mono"},
                     {"role": "drums", "notes": 67, "method": "native onset drum transcription"}]}}
```

A stem with nothing pitched in it (a part the song does not have) is reported with a
`no_notes` warning and left out of the score.

### The step-by-step route (faster, pitched parts)

Do the same steps yourself with the `fast` tier, which needs one 166 MB model.
Measure the tempo first and pass it to every transcription, so all the parts share one tempo
map:

```bash
ferraus analyze tempo song.wav                       # "bpm": 100.0, "confidence": 1.0
ferraus separate song.wav --tier fast -y
ferraus transcribe song.bass.wav --mode mono --program 33 --bpm 100 -o bass.mid
ferraus transcribe song.other.wav --mode poly --bpm 100 -y -o other.mid      # chords: Basic Pitch model, 230 kB
ferraus midi merge other.mid bass.mid -o parts.mid
ferraus score parts.mid --title "From audio" -o parts.musicxml
```

```json
{"notation": {"key": "C major", "time": "4/4", "tempo_bpm": 100.0, "measures": 9,
              "parts": [{"id": "P1", "layout": "grand", "notes": 159}, {"id": "P2", "layout": "lead-sheet", "notes": 33}],
              "chords": ["C", "Am", "F", "G", "C", "Am", "F", "Bm7b5/F", "G"]}}
```

**Check:** the bass line (one note at a time) transcribes very accurately (33 notes for 32
played). Polyphonic parts (`--mode poly`, chords) are best-effort: the mean note confidence
was 0.52, and the chord list has one wrong chord (`Bm7b5/F` where the song plays F). Open the
score, play it back against the recording, and fix what you hear. Drum audio is not
transcribed on this route; `analyze chords song.wav` gives chord symbols with times if you
only need a chord chart.

## 4. PDF, SVG, and PNG pages

MusicXML, ABC, LilyPond (`--ly`), MEI (`--mei`), and sol-fa are written natively. `--svg` falls
back to a minimal built-in renderer. Proper engraving (`--pdf`, `--png`, and good SVG) needs
the `ferraus-engrave` helper (shipped in the release packages; from source:
`cargo build -p ferraus-engrave --release`) or LilyPond. Without either, ferraus says so:

```json
{"ok": false, "error": {"code": "engraver_missing",
  "message": "no engraver found: neither the ferraus-engrave helper (Verovio) nor LilyPond is available; SVG, PDF, and PNG output need one", ...}}
```

`ferraus doctor` reports which engraver it found (`"engraver": {"helper": ..., "lilypond": ...}`).

## Reference

[score](../commands/score.md) · [score check](../commands/score_check.md) ·
[score convert](../commands/score_convert.md) · [transcribe](../commands/transcribe.md) ·
[midi merge](../commands/midi_merge.md) · [analyze chords](../commands/analyze_chords.md) ·
[Notation and solfa](../concepts/notation.md)
