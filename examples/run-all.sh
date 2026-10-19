#!/usr/bin/env bash
# Run every example against freshly generated samples in a temp dir and fail if any fails.
# CI runs this after building ferraus; it builds nothing itself.
#
#   FERRAUS=target/debug/ferraus examples/run-all.sh [WORK_DIR]
#
# WORK_DIR (default: a new temp dir, removed after a fully passing run) receives the samples,
# every example's outputs, and a ferraus home for caches, so nothing touches your own ferraus
# cache. With FERRAUS_ALLOW_DOWNLOADS=1 the model-dependent steps really run (and download
# into your normal model cache); without it they are only planned with --dry-run.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
FERRAUS=${FERRAUS:-ferraus}
# Examples run from other directories, so a relative binary path must become absolute.
if [[ $FERRAUS == */* ]]; then FERRAUS=$(cd "$(dirname "$FERRAUS")" && pwd)/$(basename "$FERRAUS"); fi
export FERRAUS
export PYTHONDONTWRITEBYTECODE=1
command -v "$FERRAUS" >/dev/null || { echo "run-all: ferraus not found ($FERRAUS); build it or set FERRAUS" >&2; exit 2; }

created_work=0
if [[ -n ${1:-} ]]; then
  WORK=$1
  mkdir -p "$WORK"
else
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/ferraus-examples.XXXXXX")
  created_work=1
fi
WORK=$(cd "$WORK" && pwd)
# A private ferraus home: an empty stage cache (so the caching checks below mean something)
# and no reads from or writes to your own caches. With downloads allowed, its model folder is
# a link to your real model cache, so models are reused and downloaded only once.
if [[ ${FERRAUS_ALLOW_DOWNLOADS:-0} == 1 ]]; then
  user_models=$("$FERRAUS" doctor --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["paths"]["models"])')
fi
export FERRAUS_HOME=$WORK/ferraus-home
mkdir -p "$FERRAUS_HOME/cache"
if [[ -n ${user_models:-} && ! -e $FERRAUS_HOME/cache/models ]]; then
  mkdir -p "$user_models"
  ln -s "$user_models" "$FERRAUS_HOME/cache/models"
fi
SAMPLES=$WORK/pipelines/samples
LOGS=$WORK/logs
mkdir -p "$WORK/pipelines" "$LOGS"

echo "ferraus: $FERRAUS ($("$FERRAUS" --version 2>/dev/null || echo unknown version))"
echo "work:    $WORK"
echo "downloads: $([[ ${FERRAUS_ALLOW_DOWNLOADS:-0} == 1 ]] && echo allowed || echo off)"

results=()
failed=0
# step NAME COMMAND...: run one step with its output in $LOGS/NAME.log; record pass/fail and time.
step() {
  local name=$1
  shift
  local start=$SECONDS rc=0
  local log
  log=$LOGS/$(printf '%s' "$name" | tr -c 'A-Za-z0-9.-' '_').log
  printf '%-52s ' "$name"
  "$@" >"$log" 2>&1 || rc=$?
  local took=$((SECONDS - start))
  if [[ $rc -eq 0 ]]; then
    echo "ok    ${took}s"
    results+=("ok|$name|${took}s")
  else
    echo "FAIL  ${took}s (exit $rc), log: $log"
    tail -40 "$log" | sed 's/^/    | /'
    results+=("FAIL|$name|${took}s")
    failed=$((failed + 1))
  fi
}

# --- pipeline helpers ----------------------------------------------------------------------------
# A pipeline is run twice: the second run must compute nothing and report every stage cached.
pipeline_twice() {
  local file=$WORK/pipelines/$1
  "$FERRAUS" pipeline run "$file" --json >"$LOGS/$1.run1.json"
  "$FERRAUS" pipeline run "$file" --overwrite --json >"$LOGS/$1.run2.json"
  python3 - "$LOGS/$1.run1.json" "$LOGS/$1.run2.json" <<'PY'
import json, sys
first, second = (json.load(open(p)) for p in sys.argv[1:])
for env in (first, second):
    assert env["ok"], env.get("error")
r = second["result"]
stages = len(r["plan"])
print(f"run 1: computed {first['result']['computed']}, cached {first['result']['cached']}")
print(f"run 2: computed {r['computed']}, cached {r['cached']} of {stages} stages")
assert r["computed"] == 0 and r["cached"] == stages, "second run was not fully cached"
assert all(s["cached"] for s in r["plan"])
assert second["outputs"], "no outputs written"
PY
}

# Changing one stage recomputes it and what follows, and nothing before it.
pipeline_partial() {
  local file=$WORK/pipelines/vocal-chain.yaml
  "$FERRAUS" pipeline run "$file" --overwrite --set stages.tuned.args.strength=100% --json >"$LOGS/vocal-chain.set.json"
  python3 - "$LOGS/vocal-chain.set.json" <<'PY'
import json, sys
env = json.load(open(sys.argv[1]))
assert env["ok"], env.get("error")
plan = {s["id"]: s["cached"] for s in env["result"]["plan"]}
print(plan)
assert plan == {"clean": True, "tuned": False, "vox": False, "mix": False, "master": False}, plan
PY
}

remix_prep() {
  local file=$WORK/pipelines/remix-prep.yaml
  "$FERRAUS" pipeline validate "$file" --json >"$LOGS/remix-prep.validate.json"
  "$FERRAUS" pipeline explain "$file" --json >"$LOGS/remix-prep.explain.json"
  python3 -c 'import json,sys; v=json.load(open(sys.argv[1])); assert v["ok"] and v["result"]["valid"]; print("plan:", " -> ".join(v["result"]["order"]))' "$LOGS/remix-prep.explain.json"
  if [[ ${FERRAUS_ALLOW_DOWNLOADS:-0} == 1 ]]; then
    "$FERRAUS" pipeline run "$file" -y --json >"$LOGS/remix-prep.run1.json"
    "$FERRAUS" pipeline run "$file" -y --overwrite --json >"$LOGS/remix-prep.run2.json"
    python3 -c 'import json,sys; r=json.load(open(sys.argv[1]))["result"]; print(r["computed"], r["cached"]); assert r["computed"] == 0' "$LOGS/remix-prep.run2.json"
  else
    # Without downloads the pipeline is only planned: --dry-run computes nothing.
    "$FERRAUS" pipeline run "$file" --dry-run --json >"$LOGS/remix-prep.dry-run.json"
    python3 -c 'import json,sys; v=json.load(open(sys.argv[1])); assert v["ok"] and v["dry_run"]; o=v["result"]["outputs"]; assert len(o) == 8; print("would write", len(o), "outputs, e.g.", o[-1]["path"])' "$LOGS/remix-prep.dry-run.json"
    # The model the separation stage needs, named without downloading it.
    "$FERRAUS" separate "$SAMPLES/song.wav" --stems 4 --tier fast --outdir "$WORK/out/remix-plan" --dry-run --json >"$LOGS/remix-prep.separate-plan.json"
    python3 -c 'import json,sys; v=json.load(open(sys.argv[1])); assert v["ok"] and v["dry_run"]; print("separation would use", v["result"]["plan"][0]["model"])' "$LOGS/remix-prep.separate-plan.json"
    echo "downloads off: remix-prep.yaml validated and dry-run, not run (FERRAUS_ALLOW_DOWNLOADS=1 runs it)"
  fi
}

claude_tools() {
  # Syntax check without writing a __pycache__ into the repository.
  python3 -c 'import sys; compile(open(sys.argv[1]).read(), sys.argv[1], "exec")' "$here/agents/claude_tool_use.py"
  # --print-tools builds the tool definitions from `ferraus schema` and needs neither the
  # anthropic package nor an API key; the API loop itself never runs here.
  python3 "$here/agents/claude_tool_use.py" --print-tools >"$LOGS/claude-tools.json"
  python3 - "$LOGS/claude-tools.json" <<'PY'
import json, sys
tools = json.load(open(sys.argv[1]))
names = [t["name"] for t in tools]
print(names)
assert len(tools) == 6
for t in tools:
    assert set(t) == {"name", "description", "input_schema"}, t.keys()
    assert t["input_schema"]["type"] == "object" and "inputs" in t["input_schema"]["properties"]
    assert not any(k.startswith("$") for k in t["input_schema"])
PY
  if python3 -c 'import anthropic' 2>/dev/null; then
    echo "anthropic $(python3 -c 'import anthropic; print(anthropic.__version__)') is installed (the API loop is not run here)"
  else
    echo "anthropic is not installed: only --print-tools was checked (pip install anthropic to run the agent)"
  fi
}

# --- run everything ------------------------------------------------------------------------------
echo
step make-samples "$here/make-samples.sh" "$SAMPLES"
for f in "$here"/pipelines/*.yaml; do cp "$f" "$WORK/pipelines/"; done

for ex in inspect clean-tune-master batch-podcast hum-to-piano stems-for-remix; do
  step "shell/$ex.sh" "$here/shell/$ex.sh" "$SAMPLES" "$WORK/out/$ex"
done

for p in vocal-chain podcast-episode master-album; do
  step "pipelines/$p.yaml (run twice, cached)" pipeline_twice "$p.yaml"
done
step "pipelines/vocal-chain.yaml (--set one stage)" pipeline_partial
step "pipelines/remix-prep.yaml" remix_prep

step "mcp/client.py" python3 "$here/mcp/client.py" --samples "$SAMPLES" --out "$WORK/out/mcp"
step "agents/parse_envelope.py" python3 "$here/agents/parse_envelope.py" --samples "$SAMPLES" --out "$WORK/out/envelope"
step "agents/claude_tool_use.py --print-tools" claude_tools

echo
echo "== summary"
printf '%s\n' "${results[@]}" | awk -F'|' '{ printf "   %-5s %-52s %s\n", $1, $2, $3 }'
echo "   total ${SECONDS}s, $failed failed; logs in $LOGS"
if [[ $failed -gt 0 ]]; then exit 1; fi
if [[ $created_work == 1 ]]; then rm -rf "$WORK"; fi
