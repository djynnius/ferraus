#!/usr/bin/env bash
# Generates this case's inputs with ferraus (no audio is committed): see tools/eval/make_session.py.
set -eu
repo="$(cd "$(dirname "$0")/../../../.." && pwd)"
python3 "$repo/tools/eval/make_session.py" stems --out .
mkdir -p mix
cp "$repo/skills/ferraus-mix/references/pipeline-template.yaml" mix/mix.yaml
(cd mix && ferraus pipeline run mix.yaml --bpm 100 --json > /dev/null)
