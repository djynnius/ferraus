#!/usr/bin/env bash
# Generates this case's inputs with ferraus (no audio is committed): see tools/eval/make_session.py.
set -eu
repo="$(cd "$(dirname "$0")/../../../.." && pwd)"
python3 "$repo/tools/eval/make_session.py" stems --out .
