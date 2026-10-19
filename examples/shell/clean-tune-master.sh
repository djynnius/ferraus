#!/usr/bin/env bash
# Clean, tune, and master a vocal over a beat, deciding each step from the JSON the previous
# step returned:  analyze problems -> denoise -> analyze key -> tune -> vocal fx -> mix ->
# master -> master verify.
#
#   examples/shell/clean-tune-master.sh [SAMPLES_DIR] [OUT_DIR]
#
# Uses samples/vocal.wav (a flat, noisy take) and samples/beat.wav. No models are needed.
set -euo pipefail
source "$(dirname "$0")/common.sh"
setup_dirs "$@"
vocal=$SAMPLES/vocal.wav
beat=$SAMPLES/beat.wav

say "1. Diagnose the take"
frx problems analyze problems "$vocal"
note "clean: $(jget "$ENV" .result.clean)"
while read -r p; do
  note "$(jfield "$p" severity) $(jfield "$p" problem): $(jfield "$p" details) -> fix: $(jfield "$p" suggested_fix)"
done < <(jlines "$ENV" .result.problems)

say "2. Denoise, learning the noise from the quietest steady second"
# Only worth it when step 1 found noise or hiss; on a clean take denoise --auto would learn
# "noise" from silence and just turn the level down.
frx denoise denoise "$vocal" --auto -o "$OUT/vocal.clean.wav" --overwrite
note "noise learned from $(jget "$ENV" .params.noise_region.range) (it must be a pause, not singing)"
note "input $(jget "$ENV" .metrics.input_rms_dbfs) dBFS RMS, removed $(jget "$ENV" .metrics.removed_rms_dbfs) dBFS RMS"
listen "vocal.clean.wav: the hiss in the first 1.5 s is gone and the voice is not watery"

say "3. Find the key on the beat (polyphonic material gives the surest estimate)"
frx key analyze key "$beat"
tonic=$(jget "$ENV" .result.tonic)
mode=$(jget "$ENV" .result.mode)
confidence=$(jget "$ENV" .result.confidence)
note "key $tonic $mode, confidence $confidence"
# Below 0.6 the key is a guess (a warning says so); let tune detect from the take instead.
if awk -v c="$confidence" 'BEGIN { exit !(c >= 0.6) }'; then
  key_args=(--key "$tonic" --scale "$mode")
else
  key_args=(--key auto)
fi

say "4. Tune"
frx tune tune "$OUT/vocal.clean.wav" "${key_args[@]}" --strength 85% --retune-speed 30ms \
  --notes-out "$OUT/notes.json" -o "$OUT/vocal.tuned.wav" --overwrite
note "notes $(jget "$ENV" .metrics.notes), corrected $(jget "$ENV" .metrics.corrected_notes), mean deviation $(jget "$ENV" .metrics.mean_abs_deviation_cents) cents"
note "per-note detail (detected pitch, target, correction): $OUT/notes.json"
listen "vocal.tuned.wav: notes land in tune with the beat; long notes keep their vibrato"

say "5. Vocal effects in one pass, from presets"
frx vocal_fx fx "$OUT/vocal.tuned.wav" --fx "highpass:freq=90Hz" --fx "compress:preset=vocal" --fx deess \
  --fx "reverb:type=plate,mix=15%" -o "$OUT/vocal.fx.wav" --overwrite

say "6. Mix over the beat"
# The take starts singing 1.5 s in, so the beat starts 1.5 s later to line up.
frx mix mix "$OUT/vocal.fx.wav" "$beat" --gains 0dB,-5dB --offsets 0s,1.5s -o "$OUT/mix.wav" --overwrite
note "mix: $(jget "$ENV" .result.duration_s) s, $(jget "$ENV" .result.channels) channels"

say "7. Master for streaming"
frx master master "$OUT/mix.wav" --target streaming --report "$OUT/master-report.json" -o "$OUT/master.wav" --overwrite
note "$(jget "$ENV" .metrics.integrated_lufs) LUFS, true peak $(jget "$ENV" .metrics.true_peak_dbtp) dBTP, target reached: $(jget "$ENV" .metrics.target_reached)"
while read -r w; do note "warning $(jfield "$w" code): $(jfield "$w" message)"; done < <(jlines "$ENV" .warnings)

say "8. Verify the master independently"
# Exit code 9 means a check failed: an answer to read, not a crash.
fr verify master verify "$OUT/master.wav" --target streaming
if [[ $RC -eq 0 ]]; then
  note "PASS: every streaming check passed"
else
  note "FAIL (exit $RC): $(jget "$ENV" .error.message)"
  note "fix: $(jget "$ENV" .error.hint)"
  exit 1
fi

say "Done: $OUT/master.wav"
listen "the vocal sits on top of the beat at streaming loudness, with no pumping or harsh S sounds"
note "every output has a .ferraus.json sidecar; 'ferraus replay <sidecar>' rebuilds that file"
