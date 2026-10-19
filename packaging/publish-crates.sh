#!/bin/sh
# Publish workspace crates to crates.io in dependency order (CI only).
set -eu
order="ferraus-core ferraus-codec ferraus-media ferraus-dsp ferraus-edit ferraus-restore ferraus-analysis \
ferraus-ml ferraus-models ferraus-pitch ferraus-denoise ferraus-separate ferraus-midi ferraus-synth \
ferraus-score ferraus-plugins ferraus-master ferraus-mcp ferraus"
for c in $order; do
  cargo publish -p "$c" --locked || echo "skipping $c (already published?)"
  sleep 20
done
