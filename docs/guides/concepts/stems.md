# Stems and separation tiers

`ferraus separate` splits a mix into stems: 2 (vocals, instrumental), 4 (vocals, drums, bass,
other), or 6 (adds guitar and piano). The models are neural networks from the ferraus model
registry, run through ONNX Runtime on the CPU or a GPU.

## Tiers

A tier is a named recipe of registry models. This table is what `ferraus separate --dry-run`
plans and `ferraus models list --task separate` reports in ferraus 0.1.0:

| Tier | Models (4 stems) | Download status today |
| --- | --- | --- |
| `fast` | Demucs v4 `htdemucs` (one model for all four stems) | **works**: verified, 166 MB, MIT |
| `balanced` | Demucs v4 fine-tuned specialists `htdemucs-ft-drums`, `-bass`, `-other`, `-vocals` (one per stem) | blocked: files have no recorded SHA-256 yet (exit code 6; `FERRAUS_ALLOW_UNVERIFIED=1` overrides, 166 MB each) |
| `quality` (default) | Mel-Band RoFormer `mel-roformer-kj` for vocals, plus the fine-tuned Demucs drums, bass, and other | blocked: the RoFormer ONNX export is not hosted yet, so it cannot be downloaded at all |
| `max` | weighted ensemble: RoFormer 0.6 + `htdemucs-ft-vocals` 0.4 for vocals; fine-tuned Demucs 0.6 + `htdemucs` 0.4 for drums, bass, and other; 2 test-time-augmentation shifts | blocked, as `quality` |

* `--stems 6` uses `htdemucs-6s` in every tier (136 MB; not yet checksum-verified, so blocked
  like `balanced`).
* `--stems 2` runs the 4-stem plan and sums drums, bass, and other into `instrumental`.
* `--model <name or .onnx file>` pins one model instead of a tier; `--ensemble a,b --weights
  0.6,0.4` builds your own ensemble from registry models.
* Several registry entries (`bs-roformer-viperx-1297`, `bs-roformer-sw`, `scnet-xl`,
  `mdx-kim-vocal-2`, `mdx-kara-2`) are listed with the flag `no_clear_license` and belong to no
  tier; `--model` with `--accept-license` uses one explicitly.

**Use `--tier fast` in this release.** The default tier stops with an error that says why:

```json
{"ok": false, "error": {"code": "model_unverified",
  "message": "separate/mel-roformer-kj@2024.08 has no verified download (status: unverified, no SHA-256 recorded); refusing to fetch an unchecked file",
  "exit_code": 6}}
```

Commands that separate internally (`score <audio>`, `master --stems`) use the `quality` tier
and are blocked the same way until those models are published.

## Quality

Separation quality is measured as SDR (signal-to-distortion ratio, in dB; higher is better).
Published results on the MUSDB18-HQ benchmark put Demucs v4 at about 9 dB averaged over four
stems, its fine-tuned variant slightly higher, and the best open RoFormer vocal models around
11 to 12 dB on vocals. The `fast` tier is good enough for practice tracks, karaoke, and remix
sketches; expect some bleed (cymbals in the vocal, reverb tails in the wrong stem).

Each stem's JSON result carries a `quality` estimate between 0 and 1 (mask confidence for a
single model, agreement between models for an ensemble). It is not calibrated to SDR, but a
low value flags a stem to check by ear. A stem that is absent from the mix (no vocals in an
instrumental) scores near 0.

## Speed

On an Apple-silicon laptop's CPU the `fast` tier separated a 21-second song in about 11
seconds, roughly half real time. The `quality` and `max` tiers run several models (and `max`
runs each twice), so they will take several times longer; a supported GPU
(`ferraus accelerate install`, see [Troubleshooting](../troubleshooting.md)) helps most there.

## Options that matter

* `--sum-to-mix on` (default) assigns whatever the models missed to `other`, so the stems add
  back to the original exactly: `ferraus mix` of the four stems against the input gives
  `"verdict": "identical"` in `ferraus compare`.
* `--only vocals,drums` writes only those stems; `--instrumental` writes one file without the
  vocals.
* `--vocal-split` (lead and backing vocals), `--drum-split` (kick, snare, toms, hi-hat,
  cymbals), and `--extended guitar,piano,strings` (more from `other`) are best-effort second
  stages with their own models; each output carries a quality flag.
* `--overlap 25%` and `--segment auto` control the processing windows; `--shifts 2` adds
  test-time augmentation (slower, slightly cleaner).
* Models download on first use with `-y`, or pull them first:
  `ferraus models pull separate/htdemucs`. `ferraus models list --cached` shows what is on disk.

See the [tutorial](../tutorials/split-a-song-into-stems.md) and the
[remix recipe](../cookbook/stems-for-remix.md).
