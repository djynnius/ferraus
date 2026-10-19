#!/usr/bin/env bash
# Generate every test file the examples use, with ferraus itself: no recorded, licensed, or
# binary audio is committed to the repository.
#
#   examples/make-samples.sh [SAMPLES_DIR]        (default: ./samples)
#
# Written (about 5 s of work and 45 MB; re-running replaces these files and nothing else):
#   melody.mid        G major tune, 100 BPM, 32 beats (the "song" melody)
#   backing.mid       piano chords + bass + drums for the same 32 beats
#   beat.wav          backing.mid rendered with the built-in instruments (stereo)
#   vocal.wav         a voice-like line: the melody on a vibrato synth lead, 35 cents flat,
#                     over room noise with a 1.5 s noise-only lead-in (what denoise --auto learns)
#   vocal.dry.wav     the same line without the noise and detuning (the reference for compare)
#   hum.wav           the melody hummed: a soft rounded tone with a slow wobble, over light noise
#   song.wav          vocal.dry.wav mixed over beat.wav (input for separation and remix prep)
#   podcast/ep01.wav  "speech" with 60 Hz mains hum
#   podcast/ep02.wav  quieter "speech" with hiss
#   podcast/ep03.flac a clean but loud episode, as FLAC
#   podcast/intro.wav, podcast/outro.wav   short chimes
#   album/track1.wav, album/track2.wav, album/track3.wav   three short "mixes" at different levels
#   tone440.wav, pink.wav   plain test signals
set -euo pipefail

FERRAUS=${FERRAUS:-ferraus}
OUT=${1:-samples}
mkdir -p "$OUT/podcast" "$OUT/album"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Quiet wrapper: the envelope goes to a scratch log; a failure prints it and stops the script.
f() {
  local log="$work/last.json"
  if ! "$FERRAUS" "$@" --overwrite --json >"$log" 2>"$work/last.err"; then
    echo "make-samples: ferraus $* failed:" >&2
    cat "$log" "$work/last.err" >&2
    exit 1
  fi
}

echo "== generating samples in $OUT"

# --- MIDI written as JSON, then converted by ferraus -------------------------------------
# One phrase of 16 beats, played twice. Notes are a little shorter than their slot so the
# "singer" breathes between them, which helps note tracking.
phrase=(G4:1 A4:1 B4:1 D5:1  C5:1 B4:1 A4:2  B4:1 G4:1 E4:1 F#4:1  G4:4)
notes=""
for rep in 0 16; do
  beat=$rep
  for n in "${phrase[@]}"; do
    name=${n%%:*} len=${n##*:}
    dur=$(awk -v l="$len" 'BEGIN { printf "%.2f", l - 0.15 }')
    notes+="{\"beat\":$beat,\"duration_beats\":$dur,\"name\":\"$name\",\"velocity\":96},"
    beat=$((beat + len))
  done
done
cat >"$work/melody.json" <<EOF
{"tempo_map":[{"beat":0,"bpm":100}],"time_signatures":[{"beat":0,"numerator":4,"denominator":4}],
 "tracks":[{"name":"melody","notes":[${notes%,}]}]}
EOF
f midi from-json "$work/melody.json" -o "$OUT/melody.mid"

# Backing: G | C | Em D | G, twice. Chords on track 1, bass on track 2, drums on track 3
# (channel 10, General MIDI kick 36, snare 38, closed hat 42).
chords=(0:G3,B3,D4:4 4:C4,E4,G4:4 8:E3,G3,B3:2 10:D3,F#3,A3:2 12:G3,B3,D4:4)
roots=(G2 G2 G2 G2 C3 C3 C3 C3 E2 E2 D2 D2 G2 G2 G2 G2)
piano="" bass="" drums=""
for rep in 0 16; do
  for c in "${chords[@]}"; do
    IFS=: read -r at pitches len <<<"$c"
    IFS=, read -ra ps <<<"$pitches"
    for p in "${ps[@]}"; do
      piano+="{\"beat\":$((rep + at)),\"duration_beats\":$len,\"name\":\"$p\",\"velocity\":70},"
    done
  done
  for i in "${!roots[@]}"; do
    bass+="{\"beat\":$((rep + i)),\"duration_beats\":0.9,\"name\":\"${roots[$i]}\",\"velocity\":90,\"channel\":2},"
    if ((i % 2 == 0)); then drum=36; else drum=38; fi
    drums+="{\"beat\":$((rep + i)),\"duration_beats\":0.25,\"pitch\":$drum,\"velocity\":110,\"channel\":10},"
    drums+="{\"beat\":$((rep + i)),\"duration_beats\":0.25,\"pitch\":42,\"velocity\":60,\"channel\":10},"
    drums+="{\"beat\":$((rep + i)).5,\"duration_beats\":0.25,\"pitch\":42,\"velocity\":45,\"channel\":10},"
  done
done
cat >"$work/backing.json" <<EOF
{"tempo_map":[{"beat":0,"bpm":100}],"time_signatures":[{"beat":0,"numerator":4,"denominator":4}],
 "tracks":[{"name":"keys","notes":[${piano%,}]},{"name":"bass","notes":[${bass%,}]},{"name":"drums","notes":[${drums%,}]}]}
EOF
f midi from-json "$work/backing.json" -o "$OUT/backing.mid"

# --- audio ---------------------------------------------------------------------------------
f render "$OUT/backing.mid" --map "1=piano,2=bass,3=drums" --channels 2 -o "$OUT/beat.wav"

# The "voice": a synth lead with vibrato, darkened so it sits like a sung vowel.
f render "$OUT/melody.mid" --instrument lead --tail 1s --channels 1 \
  --fx "vibrato:rate=5.5Hz,depth=25cents" --fx "lowpass:freq=3kHz" --fx "normalize:peak=-6dB" \
  -o "$OUT/vocal.dry.wav"
# A take: sung 35 cents flat (something for tune to fix), starting 1.5 s into the recording,
# over constant room noise (something for denoise to learn from the lead-in).
f pitch "$OUT/vocal.dry.wav" --cents -35 -o "$work/flat.wav"
f silence pad "$work/flat.wav" --start 1.5s -o "$work/flat.padded.wav"
f generate noise --color pink --dur 23s --amplitude -42dB --seed 1 -o "$work/room.wav"
f mix "$work/flat.padded.wav" "$work/room.wav" -o "$work/vocal.long.wav"
f trim "$work/vocal.long.wav" --range 0s..21.5s -o "$OUT/vocal.wav"

# Humming: a rounded tone (organ through a low-pass) with a slower, wider wobble.
f render "$OUT/melody.mid" --instrument organ --tail 1s --channels 1 \
  --fx "vibrato:rate=4.5Hz,depth=30cents" --fx "lowpass:freq=1200Hz" --fx "normalize:peak=-8dB" \
  -o "$work/hum.clean.wav"
f generate noise --color pink --dur 21s --amplitude -55dB --seed 2 -o "$work/hum.noise.wav"
f mix "$work/hum.clean.wav" "$work/hum.noise.wav" -o "$work/hum.long.wav"
f trim "$work/hum.long.wav" --range 0s..20.5s -o "$OUT/hum.wav"

f mix "$OUT/vocal.dry.wav" "$OUT/beat.wav" --gains 0dB,-4dB --normalize peak --ceiling -1dB -o "$OUT/song.wav"

# --- podcast episodes: the voice line stands in for speech -----------------------------------
f generate tone --freq 60Hz --dur 12s --amplitude -30dB -o "$work/hum60.wav"
f mix "$OUT/vocal.dry.wav" "$work/hum60.wav" -o "$work/ep01.long.wav"
f trim "$work/ep01.long.wav" --range 0s..12s -o "$OUT/podcast/ep01.wav"
f generate noise --dur 12s --amplitude -48dB --seed 3 -o "$work/hiss.wav"
f gain "$OUT/vocal.dry.wav" --by -12dB -o "$work/quiet.wav"
f mix "$work/quiet.wav" "$work/hiss.wav" -o "$work/ep02.long.wav"
f trim "$work/ep02.long.wav" --range 0s..12s -o "$OUT/podcast/ep02.wav"
f gain "$OUT/vocal.dry.wav" --by +4dB -o "$work/loud.wav"
f trim "$work/loud.wav" --range 0s..12s -o "$OUT/podcast/ep03.flac"
cat >"$work/chime.json" <<'EOF'
{"tempo_map":[{"beat":0,"bpm":120}],
 "tracks":[{"notes":[{"beat":0,"duration_beats":1,"name":"G5","velocity":90},
                     {"beat":0.5,"duration_beats":1,"name":"B5","velocity":90},
                     {"beat":1,"duration_beats":2,"name":"D6","velocity":100}]}]}
EOF
f midi from-json "$work/chime.json" -o "$work/chime.mid"
f render "$work/chime.mid" --instrument bell --tail 1s --channels 1 -o "$OUT/podcast/intro.wav"
f reverse "$OUT/podcast/intro.wav" -o "$work/chime.rev.wav"
f fade "$work/chime.rev.wav" --in 500ms -o "$OUT/podcast/outro.wav"

# --- album: three short "mixes" at deliberately different levels -----------------------------
f gain "$OUT/song.wav" --by -6dB -o "$OUT/album/track1.wav"
f gain "$OUT/beat.wav" --by -12dB -o "$OUT/album/track2.wav"
f render "$OUT/backing.mid" --map "1=strings,2=bass,3=drums" --channels 2 -o "$OUT/album/track3.wav"

# --- plain test signals ----------------------------------------------------------------------
f generate tone --freq A4 --dur 3s -o "$OUT/tone440.wav"
f generate noise --color pink --dur 3s --seed 4 -o "$OUT/pink.wav"

# Every ferraus output gets a .ferraus.json provenance sidecar showing the command that made it.
# (Intermediate files lived in a temp dir, so only the first steps can be replayed from them.)
echo "== samples ready:"
(cd "$OUT" && find . -type f ! -name '*.ferraus.json' | sort | sed 's|^\./|   |')
