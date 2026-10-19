# Stems and separation tiers

`ferraus separate` splits a mix into stems: 2 (vocals, instrumental), 4 (vocals, drums, bass,
other), or 6 (adds guitar and piano). The models are neural networks from the ferraus model
registry, run through ONNX Runtime on the CPU or a GPU.

## Tiers

A tier is a named recipe of registry models. This table is what `ferraus separate --dry-run`
plans and `ferraus models list --task separate` reports:

| Tier | Models (4 stems) | Download |
| --- | --- | --- |
| `fast` | Demucs v4 `htdemucs` (one model for all four stems) | 166 MB, MIT |
| `balanced` | Demucs v4 fine-tuned specialists `htdemucs-ft-drums`, `-bass`, `-other`, `-vocals` (one per stem) | 4 × 166 MB, MIT |
| `quality` | Mel-Band RoFormer `mel-roformer-kj` (Kimberley Jensen's vocal model) for vocals, plus the fine-tuned Demucs drums, bass, and other | 464 MB + 3 × 166 MB, MIT |
| `max` | weighted ensemble: RoFormer 0.8 + `htdemucs-ft-vocals` 0.2 for vocals; fine-tuned Demucs 0.6 + `htdemucs` 0.4 for drums, bass, and other; 2 test-time-augmentation shifts, each also channel-swapped | all of the above, MIT |

Every model in every tier has a recorded SHA-256 and a pinned download URL, so all four tiers
work. The RoFormer's training data is not disclosed by its author (its weights are MIT);
`ferraus models list` shows each entry's license note.

* `--stems 6` uses `htdemucs-6s` in every tier (136 MB, MIT). Guitar and piano are
  best-effort quality.
* `--stems 2` runs only the models that produce vocals; the instrumental is the mix minus the
  vocals (exactly, with `--sum-to-mix on`).
* `--model <name or .onnx file>` pins one model instead of a tier; `--ensemble a,b --weights
  0.6,0.4` builds your own ensemble from registry models.
* Several registry entries (`bs-roformer-viperx-1297`, `bs-roformer-sw`, `scnet-xl`,
  `mdx-kim-vocal-2`, `mdx-kara-2`) are listed with the flag `no_clear_license` and belong to no
  tier; `--model` with `--accept-license` uses one explicitly.

**The default tier is `auto`**: the best tier whose models are all verified, tried in the order
quality → balanced → fast. Every tier is verified in this registry, so `auto` means `quality`
and the result records `"tier": "quality (auto)"`. A registry without the RoFormer falls back
to `balanced` or `fast` and says so in a `tier_fallback` warning (ADR 0007).

## Quality

Separation quality is measured as SDR (signal-to-distortion ratio, in dB; higher is better).
Published Multisong results put fine-tuned Demucs v4 at about 8.4 dB on vocals and Kimberley
Jensen's Mel-Band RoFormer at about 11 dB. On the first 12 MUSDB18 test excerpts (the
7-second CC BY 4.0 set; the RoFormer's training data is unknown, so it may have heard them)
ferraus measured the median SI-SDR per stem, in dB:

| Tier | Vocals | Drums | Bass | Other |
| --- | --- | --- | --- | --- |
| `fast` | 8.6 | 7.9 | 7.8 | 3.3 |
| `balanced` | 9.0 | 7.7 | 8.1 | 3.3 |
| `quality` | 11.9 | 7.7 | 8.1 | 4.8 |
| `max` | 12.1 | 8.1 | 8.2 | 4.8 |

SI-SDR is scale-invariant SDR against the true stems; MUSDB18's published SDR figures use a
different metric (BSSEval), so compare tiers with each other rather than with papers. The
`quality` tier beats `balanced` on vocals by 2.9 dB here; `max` adds about 0.2 dB on vocals and up
to 0.4 dB elsewhere for about 4.5 times the work.

The `fast` tier is good enough for practice tracks, karaoke, and remix sketches; expect some
bleed (cymbals in the vocal, reverb tails in the wrong stem). `quality` gives clearly cleaner
vocals; drums, bass, and other come from the same fine-tuned Demucs models as `balanced`.

Each stem's JSON result carries a `quality` estimate between 0 and 1 (mask confidence for a
single model, agreement between models for an ensemble). It is not calibrated to SDR, but a
low value flags a stem to check by ear. A stem that is absent from the mix (no vocals in an
instrumental) scores near 0.

## Speed and memory

Measured on a 10-core Apple-silicon laptop's CPU with a 4-minute song, at the default
`--max-memory 2GB`:

| Tier | Time | Peak memory |
| --- | --- | --- |
| `fast` | 1.9 min (1.7 min with `--max-memory 8GB`) | 2.3 GB |
| `--stems 6` | 1.9 min | 1.9 GB |
| `balanced` | 7.2 min | 2.2 GB |
| `quality` | 15.6 min | 3.4 GB |
| `max` | about 4.5× `quality` (4 passes of 6 models) | as `quality` |

Separation runs the models one at a time and streams each over the song window by window, so
memory does not grow with the song: a 60-minute input stays under 4 GB in every tier. Long
inputs spill their intermediate signals to temporary files (removed afterwards, also on error
or cancel). When a model's own working memory exceeds `--max-memory`, ferraus loads it in a
leaner, somewhat slower mode; `result.memory` reports the plan (`spilled`, `lean_sessions`,
`fits`). Raise the budget (`--max-memory 8GB`) for the fastest run on a machine with RAM to
spare. A supported GPU (`ferraus accelerate install`, see
[Troubleshooting](../troubleshooting.md)) helps most for `quality` and `max`.

## Options that matter

* `--sum-to-mix on` (default) assigns whatever the models missed to `other`, so the stems add
  back to the original exactly: `ferraus mix` of the four stems against the input gives
  `"verdict": "identical"` in `ferraus compare`.
* `--only vocals,drums` writes only those stems; `--instrumental` writes one file without the
  vocals.
* `--drum-split` splits the drum stem into kick, snare, toms, and cymbals with DrumSep (a
  hybrid Demucs fine-tune, MIT; hi-hats land in cymbals). `--extended guitar,piano` takes guitar
  and piano out of `other` with `htdemucs-6s`. `--vocal-split` (lead and backing vocals) has no
  license-clean model yet: pass one with `--cascade-models vocal-split=<model>
  --accept-license`. Each cascade output carries a quality flag, and `--dry-run` lists the
  cascade stages with their models and outputs.
* `--overlap 25%` and `--segment auto` control the processing windows; `--shifts 2` adds
  test-time augmentation (slower, slightly cleaner).
* Models download on first use with `-y`, or pull them first:
  `ferraus models pull --all-for separate --tier quality`. `ferraus models list --cached` shows
  what is on disk.

See the [tutorial](../tutorials/split-a-song-into-stems.md) and the
[remix recipe](../cookbook/stems-for-remix.md).
