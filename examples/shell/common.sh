# Shared helpers for the shell examples. Source it; do not run it.
#
# Every example takes the same two optional arguments:
#   example.sh [SAMPLES_DIR] [OUT_DIR]
# SAMPLES_DIR defaults to fresh samples made by ../make-samples.sh in a temp dir; OUT_DIR
# defaults to a new temp dir. An example writes nothing outside OUT_DIR (and that temp dir).

FERRAUS=${FERRAUS:-ferraus}
EXAMPLES_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

say() { printf '\n== %s\n' "$*"; }
note() { printf '   %s\n' "$*"; }
listen() { printf '   listen: %s\n' "$*"; }

# Set SAMPLES and OUT from the arguments, making whichever is missing.
setup_dirs() {
  SAMPLES=${1:-}
  OUT=${2:-}
  if [[ -z $SAMPLES ]]; then
    SAMPLES=$(mktemp -d "${TMPDIR:-/tmp}/ferraus-samples.XXXXXX")
    "$EXAMPLES_DIR/make-samples.sh" "$SAMPLES" >/dev/null
  fi
  [[ -n $OUT ]] || OUT=$(mktemp -d "${TMPDIR:-/tmp}/ferraus-example.XXXXXX")
  mkdir -p "$OUT/json"
  SAMPLES=$(cd "$SAMPLES" && pwd)
  OUT=$(cd "$OUT" && pwd)
  note "samples: $SAMPLES"
  note "output:  $OUT"
}

# table: format '|'-separated lines from stdin as aligned columns (a portable `column -t`).
table() {
  awk -F'|' '
    { for (i = 1; i <= NF; i++) { cell[NR, i] = $i; if (length($i) > w[i]) w[i] = length($i) }
      if (NF > nf) nf = NF }
    END { for (r = 1; r <= NR; r++) {
            line = ""
            for (i = 1; i <= nf; i++) line = line sprintf("%-" (w[i] + 2) "s", cell[r, i])
            sub(/ +$/, "", line)
            print "   " line } }'
}

downloads_allowed() { [[ ${FERRAUS_ALLOW_DOWNLOADS:-0} == 1 ]]; }

# fr NAME ARGS...: run `ferraus ARGS... --json`, keep the envelope in $OUT/json/NAME.json and
# stderr (logs, progress) in $OUT/json/NAME.log. Sets RC to the exit code and never aborts, so
# the caller decides what a non-zero code means (9 from `master verify` is an answer, not a crash).
fr() {
  local name=$1
  shift
  RC=0
  "$FERRAUS" "$@" --json >"$OUT/json/$name.json" 2>"$OUT/json/$name.log" || RC=$?
  ENV="$OUT/json/$name.json"
}

# frx NAME ARGS...: like fr, but stop the example when the command fails, printing the
# error's message and hint (the hint is a runnable command when one exists).
frx() {
  fr "$@"
  if [[ $RC -ne 0 ]]; then
    echo "ferraus ${*:2} failed with exit code $RC:" >&2
    echo "  $(jget "$ENV" .error.message)" >&2
    echo "  hint: $(jget "$ENV" .error.hint)" >&2
    exit "$RC"
  fi
}

# jget FILE PATH: one value from a JSON file, for simple paths such as .result.key or
# .outputs[0].path. Uses jq when it is installed and python3 otherwise. Prints "null" for a
# missing value, true/false for booleans, and compact JSON for objects and arrays.
jget() {
  if command -v jq >/dev/null 2>&1; then
    jq -c -r "$2" "$1"
  else
    python3 - "$1" "$2" <<'PY'
import json, re, sys
v = json.load(open(sys.argv[1]))
for key, idx in re.findall(r'\.([A-Za-z_][\w-]*)|\[(\d+)\]', sys.argv[2]):
    if key:
        v = v.get(key) if isinstance(v, dict) else None
    else:
        v = v[int(idx)] if isinstance(v, list) and int(idx) < len(v) else None
    if v is None:
        break
if v is None:
    print("null")
elif isinstance(v, bool):
    print(str(v).lower())
elif isinstance(v, (dict, list)):
    print(json.dumps(v, separators=(",", ":")))
else:
    print(v)
PY
  fi
}

# jlines FILE PATH: each element of an array on its own line (compact JSON for objects).
jlines() {
  if command -v jq >/dev/null 2>&1; then
    jq -c -r "$2 // [] | .[]" "$1"
  else
    python3 - "$1" "$2" <<'PY'
import json, re, sys
v = json.load(open(sys.argv[1]))
for key, idx in re.findall(r'\.([A-Za-z_][\w-]*)|\[(\d+)\]', sys.argv[2]):
    v = (v.get(key) if isinstance(v, dict) else None) if key else (v[int(idx)] if isinstance(v, list) and int(idx) < len(v) else None)
    if v is None:
        break
for item in v or []:
    print(json.dumps(item, separators=(",", ":")) if isinstance(item, (dict, list)) else item)
PY
  fi
}

# jfield JSON_TEXT KEY: one top-level field of a compact JSON object (from jlines).
jfield() {
  if command -v jq >/dev/null 2>&1; then
    jq -c -r ".$2" <<<"$1"
  else
    python3 -c 'import json,sys; v=json.loads(sys.argv[1]).get(sys.argv[2]); print("null" if v is None else (str(v).lower() if isinstance(v,bool) else (json.dumps(v) if isinstance(v,(dict,list)) else v)))' "$1" "$2"
  fi
}
