# Notation and solfa

`ferraus score` turns audio or MIDI into notation and `ferraus score read` / `score perform`
go the other way. MusicXML is the hub format; MIDI is always produced first.

* **Lead sheet**: melody on a treble staff, chord symbols above, lyrics below.
* **Grand staff**: treble and bass clefs for piano, with a hand split (`--split auto` or a note
  such as `C4`).
* **Drum score**: percussion clef with the standard drum map (kick in the bottom space, snare
  in the third space, hi-hat and cymbals as x noteheads above the staff).
* **Solfa**: tonic sol-fa with movable do (`d r m f s l t`), octave marks, and Curwen rhythm
  punctuation; `--solfa only` writes solfa text and no staff; `--solfa fixed` uses fixed do.
* **Chord symbols**: `C`, `Am7`, `G/B` (slash chords carry the bass); `--chords diagrams` adds
  guitar fretboard diagrams.

Every note carries a confidence and every measure an aggregate; low-confidence measures are
listed with the reason so you (or an agent) know which bars to check.
