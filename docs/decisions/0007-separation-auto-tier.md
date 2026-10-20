# ADR 0007: `separate --tier auto` is the default

Status: accepted (2026-10-19). Update 2026-10-19: every tier is now verified (the fine-tuned
Demucs specialists, `htdemucs-6s`, and an MIT-licensed hosted ONNX export of Kimberley
Jensen's Mel-Band RoFormer), so `auto` resolves to `quality`, the PRD's intended default; the
fallback below still applies to registries that lack a tier's models.

Update 2026-10-19 (release decision): until counsel reviews the RoFormer weights' license (MIT
on the author's repository, GPL-3.0 on another conversion; `docs/legal/LICENSE-REVIEW.md`), the
registry's `quality` tier carries `auto_skip`, so `auto` resolves to `balanced` without a
warning. `--tier quality` and `--tier max` still run when named. Deleting the `auto_skip` field
restores `quality` as the default.

Update 2026-10-20 (maintainer decision): the RoFormer weights' MIT license, as stated on the
author's repository, is accepted (a third party's conversion label does not override it). The
`auto_skip` field is removed, so `auto` resolves to `quality` again. The mechanism stays for
future holds.

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
* Separation worked out of the box from the start with the MIT-licensed htdemucs ONNX export
  (`fast`).
* When the registry pinned verified `quality` checkpoints, `auto` picked them with no code
  change, which restored the PRD's intended default.
