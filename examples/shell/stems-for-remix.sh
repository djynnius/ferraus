#!/usr/bin/env bash
# Stems for a remix: measure tempo and key, split the song into four stems with the fast tier,
# and check each stem's quality.
#
#   examples/shell/stems-for-remix.sh [SAMPLES_DIR] [OUT_DIR]
#   FERRAUS_ALLOW_DOWNLOADS=1 examples/shell/stems-for-remix.sh     # really separate
#
# Separation needs a model (htdemucs, 166 MB, downloaded once and verified by SHA-256). It is
# downloaded only when FERRAUS_ALLOW_DOWNLOADS=1 (this script then passes -y). Otherwise the
# script plans the run with --dry-run, shows what a call without -y returns, and stops there.
set -euo pipefail
source "$(dirname "$0")/common.sh"
setup_dirs "$@"
song=$SAMPLES/song.wav

say "1. Tempo and key (no model needed)"
frx tempo analyze tempo "$song"
bpm=$(jget "$ENV" .result.bpm)
note "tempo $bpm BPM (confidence $(jget "$ENV" .result.confidence)), time signature $(jget "$ENV" .result.time_signature)"
frx key analyze key "$song"
note "key $(jget "$ENV" .result.key) (confidence $(jget "$ENV" .result.confidence))"

say "2. Plan the separation"
frx plan separate "$song" --stems 4 --tier fast --outdir "$OUT/stems" --dry-run
note "model: $(jget "$ENV" .result.plan[0].model) (already cached: $(jget "$ENV" .result.plan[0].cached))"
while read -r o; do note "would write $(jfield "$o" stem): $(jfield "$o" path)"; done < <(jlines "$ENV" .result.outputs)

if ! downloads_allowed; then
  say "3. Without permission to download"
  # Without -y ferraus never downloads; it fails with exit code 5 and a runnable hint.
  fr no_download separate "$song" --stems 4 --tier fast --outdir "$OUT/stems"
  if [[ $RC -eq 5 ]]; then
    note "exit 5 ($(jget "$ENV" .error.code)): $(jget "$ENV" .error.message)"
    note "hint: $(jget "$ENV" .error.hint)"
  elif [[ $RC -eq 0 ]]; then
    note "separation ran without -y: the model was already cached (or models.auto_download is on)"
  else
    note "unexpected exit $RC: $(jget "$ENV" .error.message)"
    exit 1
  fi
  say "Skipped the separation itself: set FERRAUS_ALLOW_DOWNLOADS=1 to download the model and run it"
  exit 0
fi

say "3. Separate (downloads the model on first use)"
frx separate separate "$song" --stems 4 --tier fast --outdir "$OUT/stems" -y --overwrite
note "took $(jget "$ENV" .timing_ms) ms; stems add back to the mix: $(jget "$ENV" .result.sum_to_mix.applied)"

say "4. Check each stem"
# quality.score is the model's mask confidence (0..1, uncalibrated): a low score means that
# stem is mostly bleed or the instrument is absent, so skip or flag it before building on it.
rows=("stem|quality|LUFS|path")
low=0
while read -r s; do
  stem=$(jfield "$s" stem)
  path=$(jfield "$s" path)
  score=$(awk -v q="$(jfield "$(jfield "$s" quality)" score)" 'BEGIN { printf "%.3f", q }')
  frx "loud.$stem" analyze loudness "$path"
  rows+=("$stem|$score|$(jget "$ENV" .result.integrated_lufs)|$(basename "$path")")
  if awk -v q="$score" 'BEGIN { exit !(q < 0.3) }'; then
    note "low quality: $stem ($score), check it by ear before using it"
    low=$((low + 1))
  fi
done < <(jlines "$OUT/json/separate.json" .result.stems)
printf '%s\n' "${rows[@]}" | table

# The stems should add back to the original: compare reports the residual after alignment.
frx sum mix "$OUT"/stems/song.{vocals,drums,bass,other}.wav -o "$OUT/stems-sum.wav" --overwrite
frx compare compare "$song" "$OUT/stems-sum.wav"
note "stems summed vs original: SI-SDR $(jget "$ENV" .metrics.si_sdr_db) dB (higher is closer; above 40 dB is a null)"

say "Done: $OUT/stems/"
listen "song.drums.wav: the kit alone; song.other.wav: keys and the lead line"
note "these samples are synthesized: htdemucs hears the synth lead and the built-in bass as 'other',"
note "so vocals and bass come out nearly empty and the quality check above flags exactly that."
note "on a real song vocals and bass score high, and a low score means bleed or an absent part."
note "next steps: examples/pipelines/remix-prep.yaml stretches the stems to 110 BPM and mixes a sketch"
