#!/usr/bin/env bash
# Batch podcast cleanup: for every episode, diagnose it, build an fx chain from the problems
# ferraus reports, run cleanup and loudness in one pass, verify against the podcast spec, and
# print a table.
#
#   examples/shell/batch-podcast.sh [SAMPLES_DIR] [OUT_DIR]
#
# Uses samples/podcast/ep01.wav (mains hum), ep02.wav (quiet, hissy), ep03.flac (clean, loud).
# No models are needed.
set -euo pipefail
source "$(dirname "$0")/common.sh"
setup_dirs "$@"
mkdir -p "$OUT/clean"

# Podcast loudness is -16 LUFS for stereo and -19 LUFS for mono: a mono file plays from both
# speakers and sounds about 3 dB louder, and `master verify --target podcast` checks exactly that.
target_for() { [[ $1 == 1 ]] && echo -19 || echo -16; }

rows=()
for ep in "$SAMPLES"/podcast/ep*.wav "$SAMPLES"/podcast/ep*.flac; do
  name=$(basename "${ep%.*}")
  say "$name"

  frx "$name.info" info "$ep" --loudness
  channels=$(jget "$ENV" .result.channels)
  before=$(jget "$ENV" .result.loudness.integrated_lufs)
  lufs=$(target_for "$channels")

  # Each problem comes with a suggested_fix that is already an fx stage string.
  frx "$name.problems" analyze problems "$ep"
  chain=()
  fixes=()
  while read -r p; do
    fix=$(jfield "$p" suggested_fix)
    note "$(jfield "$p" severity) $(jfield "$p" problem) -> $fix"
    if [[ $fix != null ]]; then
      chain+=(--fx "$fix")
      fixes+=("$fix")
    fi
  done < <(jlines "$ENV" .result.problems)
  [[ ${#fixes[@]} -gt 0 ]] || note "no problems found: loudness only"

  # Always: a gentle leveler for speech, then loudness last so nothing after it changes the level.
  chain+=(--fx leveler --fx "normalize:lufs=$lufs")
  out="$OUT/clean/$name.wav"
  frx "$name.fx" fx "$ep" "${chain[@]}" -o "$out" --overwrite

  frx "$name.after" analyze loudness "$out"
  after=$(jget "$ENV" .result.integrated_lufs)
  peak=$(jget "$ENV" .result.true_peak_dbtp)

  fr "$name.verify" master verify "$out" --target podcast
  if [[ $RC -eq 0 ]]; then verdict=pass; else verdict="FAIL ($(jget "$ENV" .error.message))"; fi

  fixes_text=$(IFS=,; echo "${fixes[*]:-none}")
  rows+=("$name|$channels|$fixes_text|$before|$after|$peak|$verdict")
done

say "Report"
{
  echo "episode|ch|fixes applied|LUFS before|LUFS after|true peak|podcast spec"
  printf '%s\n' "${rows[@]}"
} | table
echo
note "cleaned files: $OUT/clean/"
note "when every file gets the same chain, one call does the whole folder (quote the glob):"
note "  ferraus fx \"podcast/*.wav\" --fx dehum --fx \"normalize:lufs=-19\" --outdir clean --json"
listen "ep01: no 60 Hz buzz under the voice; ep02: as loud as the others, with less hiss;"
listen "ep03: unchanged in tone, just brought to the same level"

for r in "${rows[@]}"; do [[ $r == *"|pass" ]] || exit 1; done
