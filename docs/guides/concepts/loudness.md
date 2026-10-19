# Loudness standards

ferraus measures loudness with ITU-R BS.1770-4 / EBU R128: K-weighted, gated integrated
loudness in LUFS (loudness units relative to full scale), short-term (3 s) and momentary
(400 ms) loudness, loudness range (LRA, in LU), and true peak in dBTP (measured after 4x
oversampling, so it catches peaks between samples).

| Target | Integrated | True-peak ceiling | Where it applies |
| --- | --- | --- | --- |
| `streaming` | -14 LUFS | -1 dBTP | Spotify, YouTube, Tidal, Amazon normalize around this level |
| `apple` | -16 LUFS | -1 dBTP | Apple Music and Apple Podcasts |
| `youtube` | -14 LUFS | -1 dBTP | alias of streaming |
| `club` | -8 LUFS | -0.3 dBTP | DJ and club playback, no normalization |
| `cd` | -10 LUFS | -0.3 dBTP | 16-bit 44.1 kHz with dither |
| `broadcast` | -23 LUFS | -1 dBTP | EBU R128 (`--standard atsc`: -24 LKFS, -2 dBTP) |
| `podcast` | -16 LUFS stereo, -19 LUFS mono | -1 dBTP | speech |

* `ferraus analyze loudness song.wav` reports every measure; `--timeline` adds curves.
* `ferraus normalize song.wav --lufs -14` reaches a target within 0.3 LU and never exceeds
  the true-peak ceiling (a lookahead limiter catches overshoot).
* `ferraus master mix.wav --target streaming` does tonal balance, dynamics, stereo, and loudness.

Louder is not better on streaming services: they turn loud masters down, so excessive
limiting only costs dynamics.
