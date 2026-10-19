#!/usr/bin/env bash
# Hum to piano: transcribe a hummed idea to MIDI, check the notes, quantize them, render the
# part on a piano, and write it out as a score (MusicXML).
#
#   examples/shell/hum-to-piano.sh [SAMPLES_DIR] [OUT_DIR]
#
# Uses samples/hum.wav (the G major tune hummed, slightly wobbly, over light noise). No models
# are needed: transcription uses the built-in pYIN tracker and rendering the built-in piano
# (`ferraus sounds install gm-basic -y` would make it a sampled piano).
set -euo pipefail
source "$(dirname "$0")/common.sh"
setup_dirs "$@"
hum=$SAMPLES/hum.wav

say "1. Transcribe"
# Transcribing first, instead of the one-shot `hum`, gives a note list to check before
# anything is rendered.
frx transcribe transcribe "$hum" -o "$OUT/idea.mid" --overwrite
note "$(jget "$ENV" .result.notes) notes, $(jget "$ENV" .result.bpm) BPM, key $(jget "$ENV" .result.key) (confidence $(jget "$ENV" .result.key_confidence))"
note "pitch range $(jget "$ENV" .result.pitch_range), mode $(jget "$ENV" .result.mode) ($(jget "$ENV" .result.engine))"
note "mean note confidence $(jget "$ENV" .metrics.mean_note_confidence); the note list is in $(jget "$ENV" .result.notes_output)"
# What to look for: fewer notes than you hummed (repeated notes merged: raise
# --onset-sensitivity), many tiny notes (a wobbly hum split up: raise --min-note), or mode
# poly (chords or background music: separate the voice first).

say "2. Quantize to sixteenth notes"
frx quantize midi quantize "$OUT/idea.mid" --grid 1/16 --strength 100% -o "$OUT/idea.q.mid" --overwrite
note "moved $(jget "$ENV" .result.notes_moved) notes (mean $(jget "$ENV" .result.mean_shift_ticks) ticks at $(jget "$ENV" .result.ppq) per beat),"
note "dropped $(jget "$ENV" .result.notes_removed_as_duplicates) duplicate(s) that landed on the same grid step"

say "3. Render on a piano"
frx render render "$OUT/idea.q.mid" --instrument piano -o "$OUT/idea.piano.wav" --overwrite
note "played by: $(jget "$ENV" .result.instrument) ($(jget "$ENV" .result.duration_s) s)"
listen "idea.piano.wav: the hummed tune, in time, on a piano"

say "4. Score"
frx score score "$OUT/idea.q.mid" --title "Hummed idea" -o "$OUT/idea.musicxml" --overwrite
note "key $(jget "$ENV" .result.notation.key), time $(jget "$ENV" .result.notation.time), $(jget "$ENV" .result.measures) measures, layout $(jget "$ENV" .result.notation.parts[0].layout)"
frx check score check "$OUT/idea.musicxml"
note "score check: ok $(jget "$ENV" .result.ok), $(jget "$ENV" .result.errors) errors, $(jget "$ENV" .result.notes) notes"

say "5. The same in one command"
# hum = transcribe + quantize + render. Its MIDI goes next to the audio.
frx hum hum "$hum" --to piano -o "$OUT/idea.hum-piano.wav" --overwrite
note "wrote $(jget "$ENV" .result.audio) and $(jget "$ENV" .result.midi)"

say "Done: open $OUT/idea.musicxml in MuseScore or any notation editor"
