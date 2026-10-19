# Notation and solfa

`ferraus score` turns audio or MIDI into notation and `ferraus score read` / `score perform`
go the other way. MusicXML is the hub format. Readable inputs: MusicXML (`.musicxml`, `.xml`,
`.mxl`), MEI, ABC, LilyPond, MIDI, and the ferraus score JSON; outputs add SVG, PDF, and PNG
pages (through the `ferraus-engrave` helper or LilyPond) and sol-fa text or Markdown.
Printed music (PDF, PNG, JPEG) goes through optical music recognition (`score read`), whose
models are not downloadable in this release.

* **Lead sheet**: melody on a treble staff, chord symbols above, lyrics below.
* **Grand staff**: treble and bass clefs for piano, with a hand split (`--split auto` or a note
  such as `C4`).
* **Drum score**: percussion clef with the standard drum map (kick in the bottom space, snare
  in the third space, hi-hat and cymbals as x noteheads above the staff).
* **Solfa**: tonic sol-fa with movable do (`d r m f s l t`), octave marks, and Curwen rhythm
  punctuation; `--solfa on` prints syllables under the staff, `--solfa only` writes sol-fa text
  and no staff, `--solfa fixed` uses fixed do (do = C). A minor key is written with its
  relative major's doh, so the tonic is `l` (lah). From `ferraus score melody.mid --solfa only`:

  ```text
  Key: Doh is C (A minor, lah is A)
  Time: 4/4
  Tempo: 100 bpm

  |m :r :d :r |m :m :m :d |d :t, :l, :t, |d :d :l, :l, ||
  ```

  `:` separates beats, `|` bars, and a comma after a syllable (`t,`) puts it an octave below.
* **Chord symbols**: `C`, `Am7`, `G/B` (slash chords carry the bass); `--chords diagrams` adds
  guitar fretboard diagrams.

Every note carries a confidence and every measure an aggregate; low-confidence measures are
listed with the reason so you (or an agent) know which bars to check.

Walkthroughs: [Make a score](../tutorials/make-a-score.md) and
[Perform a sheet-music file](../tutorials/perform-sheet-music.md).
