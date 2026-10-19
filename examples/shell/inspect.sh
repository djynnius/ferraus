#!/usr/bin/env bash
# Inspect: how to read the JSON envelope that every ferraus command prints with --json.
#   info -> analyze problems -> fx -> compare -> the provenance sidecar -> an error envelope
#
#   examples/shell/inspect.sh [SAMPLES_DIR] [OUT_DIR]
#
# Reads JSON with jq when it is installed and falls back to python3 otherwise; each query is
# printed in jq syntax so you can copy it. No models are needed.
set -euo pipefail
source "$(dirname "$0")/common.sh" # for setup_dirs, say, note
setup_dirs "$@"
take=$SAMPLES/vocal.wav

if command -v jq >/dev/null 2>&1; then
  note "reading JSON with jq"
else
  note "jq not found: reading JSON with python3"
fi

# q FILE FILTER: print FILTER's value from FILE. FILTER is a jq path (.a.b, .a[0].b, keys).
q() {
  if command -v jq >/dev/null 2>&1; then
    jq -c -r "$2" "$1"
  else
    python3 - "$1" "$2" <<'PY'
import json, re, sys
v = json.load(open(sys.argv[1]))
expr = sys.argv[2]
if expr == "keys":
    v = sorted(v)
else:
    for key, idx in re.findall(r'\.([A-Za-z_][\w-]*)|\[(\d+)\]', expr):
        v = (v.get(key) if isinstance(v, dict) else None) if key else (v[int(idx)] if isinstance(v, list) and int(idx) < len(v) else None)
print("null" if v is None else str(v).lower() if isinstance(v, bool) else json.dumps(v, separators=(",", ":")) if isinstance(v, (dict, list)) else v)
PY
  fi
}
show() { printf '   %-34s %s\n' "$2" "$(q "$1" "$2")"; }

say "1. info: stdout holds exactly one JSON envelope (logs and progress go to stderr)"
"$FERRAUS" info "$take" --loudness --json >"$OUT/info.json"
show "$OUT/info.json" keys
show "$OUT/info.json" .ok
show "$OUT/info.json" .command
show "$OUT/info.json" .result.duration_s
show "$OUT/info.json" .result.sample_rate
show "$OUT/info.json" .result.channels
show "$OUT/info.json" .result.loudness.integrated_lufs
show "$OUT/info.json" .result.loudness.true_peak_dbtp

say "2. analyze problems: each problem names an fx stage that fixes it"
"$FERRAUS" analyze problems "$take" --json >"$OUT/problems.json"
show "$OUT/problems.json" .metrics
show "$OUT/problems.json" .result.problems[0].problem
show "$OUT/problems.json" .result.problems[0].details
show "$OUT/problems.json" .result.problems[0].suggested_fix
fix=$(q "$OUT/problems.json" .result.problems[0].suggested_fix)
[[ $fix != null ]] || fix="highpass:freq=80Hz" # a clean take: use any stage for the demo

say "3. fx: params holds every parameter after defaults; outputs carry a role"
"$FERRAUS" fx "$take" --fx "$fix" -o "$OUT/take.fixed.wav" --overwrite --json >"$OUT/fx.json"
show "$OUT/fx.json" .params
show "$OUT/fx.json" .outputs[0]
show "$OUT/fx.json" .warnings
show "$OUT/fx.json" .timing_ms

say "4. compare: a null test of the original against the processed file"
"$FERRAUS" compare "$take" "$OUT/take.fixed.wav" --json >"$OUT/compare.json"
show "$OUT/compare.json" .metrics.si_sdr_db
show "$OUT/compare.json" .metrics.rms_diff_db
show "$OUT/compare.json" .metrics.verdict
note "a high SI-SDR and a low rms_diff_db mean the fix changed little besides its target"

say "5. The sidecar: every output gets <output>.ferraus.json, how it was made"
sidecar="$OUT/take.fixed.wav.ferraus.json"
show "$sidecar" .command
show "$sidecar" .argv
show "$sidecar" .inputs[0].sha256
note "'ferraus verify' checks the hashes and 'ferraus replay' re-runs the command:"
"$FERRAUS" verify "$sidecar" --json >"$OUT/verify.json"
show "$OUT/verify.json" .ok

say "6. An error is an envelope too, with a code, a hint, and the exit code"
rc=0
"$FERRAUS" info "$OUT/no-such-file.wav" --json >"$OUT/error.json" 2>/dev/null || rc=$?
note "exit code $rc"
show "$OUT/error.json" .ok
show "$OUT/error.json" .error.code
show "$OUT/error.json" .error.message
show "$OUT/error.json" .error.exit_code
[[ $rc -eq 3 ]] # 3 = input missing (see AGENTS.md for the full table)

say "Done: the envelopes are in $OUT/*.json"
