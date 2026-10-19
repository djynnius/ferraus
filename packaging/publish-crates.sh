#!/bin/sh
# Publish workspace crates to crates.io in dependency order (CI only).
# A version that is already on crates.io is skipped; crates.io's rate limit for new crates
# (HTTP 429) is waited out; any other failure stops the release.
set -eu
order="ferraus-core ferraus-codec ferraus-media ferraus-dsp ferraus-edit ferraus-restore ferraus-analysis \
ferraus-pitch ferraus-models ferraus-ml ferraus-denoise ferraus-separate ferraus-midi ferraus-synth \
ferraus-score ferraus-plugins ferraus-plughost ferraus-master ferraus-mcp ferraus"
log=$(mktemp)
for c in $order; do
  attempt=1
  while :; do
    if cargo publish -p "$c" --locked 2>"$log"; then
      cat "$log" >&2
      break
    fi
    cat "$log" >&2
    if grep -qiE "already (exists|uploaded)" "$log"; then
      echo "$c: this version is already on crates.io, skipping" >&2
      break
    fi
    if grep -qE "429|Too Many Requests|too many" "$log" && [ "$attempt" -lt 30 ]; then
      echo "$c: crates.io rate limit, retrying in 10 minutes (attempt $attempt)" >&2
      attempt=$((attempt + 1))
      sleep 600
      continue
    fi
    echo "$c: publish failed" >&2
    exit 1
  done
done
