# Stems and separation tiers

`ferraus separate` splits a mix into stems: 2 (vocals, instrumental), 4 (vocals, drums, bass,
other), or 6 (adds piano and guitar).

| Tier | Models | Typical 4-stem SDR | CPU time (4-min song) |
| --- | --- | --- | --- |
| `fast` | Demucs v4 htdemucs | ~9.1 dB | ~2 min |
| `balanced` | Demucs v4 htdemucs_ft | ~9.3 dB | ~4 min |
| `quality` (default) | best open RoFormer checkpoints | ~11.5 dB | ~15 min (≈1 min on GPU) |
| `max` | RoFormer + SCNet + Demucs ensemble, TTA | ~12.5 dB | ~45 min (≈3 min on GPU) |

SDR (signal-to-distortion ratio) is the standard quality measure; higher is better.

* `--sum-to-mix on` (default) assigns what the models missed to `other`, so the stems add back
  to the original exactly.
* `--vocal-split` adds lead and backing vocals; `--drum-split` adds kick, snare, toms, hi-hat,
  cymbals; `--extended guitar,piano,strings` extracts more from `other` (best-effort, each
  with a quality flag).
* Each stem's `quality` estimate is in the JSON metrics.
* Models download on first use with `-y`, or pull them first: `ferraus models pull --all-for separate`.
