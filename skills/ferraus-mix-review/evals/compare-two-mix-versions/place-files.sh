#!/usr/bin/env bash
# Generates this case's inputs with ferraus (no audio is committed): see tools/eval/make_session.py.
set -eu
repo="$(cd "$(dirname "$0")/../../../.." && pwd)"
python3 "$repo/tools/eval/make_session.py" mix --out .
ferraus mix stems/*.wav --gains -2dB,-2dB,-6dB,-3dB,-8dB,-10dB,-9dB,+3dB,-8dB,-9dB --pans 0,0,30,0,-25,0,40,0,-50,-35 -o mix_v2.wav --bit-depth 24 --no-sidecar --json > /dev/null
rm -rf stems
