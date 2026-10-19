# ADR 0007: `separate --tier auto` is the default

Status: accepted (2026-10-19)

## Context
The PRD makes `quality` (RoFormer-class) the default separation tier. The best open RoFormer
and SCNet checkpoints either lack a clear license or are published only as PyTorch weights that
still need exporting and hosting (see `crates/ferraus-models/registry/registry.json`; G3, the
model license audit, is open). With `quality` as the default, `ferraus separate song.wav -y`
failed with exit code 6 for every user, and so did `score <audio>` and `master --stems`.

## Decision
The default tier is `auto`: the best tier whose every model is verified (SHA-256 and a download
URL in the registry, or built in), checked in the order quality → balanced → fast. The chosen
tier is recorded in `params.tier` (e.g. `"fast (auto)"`) and a `tier_fallback` warning says
when it is below `quality`. `--tier quality|balanced|max` stay available and fail explicitly
(exit 5/6 with the exact `models pull` hint) until their models are verified.

## Consequences
* Separation works out of the box today with the MIT-licensed htdemucs ONNX export (`fast`).
* As soon as the registry pins verified `quality` checkpoints, `auto` picks them with no code
  change, which restores the PRD's intended default.
