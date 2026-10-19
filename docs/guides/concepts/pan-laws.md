# Pan laws and stereo placement

When a mono source moves between speakers, its perceived loudness changes unless the gains
follow a pan law. ferraus uses constant power by default.

| `--law` | Center gain per side | Notes |
| --- | --- | --- |
| `-3dB` (default) | -3 dB | constant power: summed power is the same at every position |
| `-4.5dB` | -4.5 dB | compromise used by some consoles |
| `-6dB` | -6 dB | constant amplitude: mono-compatible sum at the center |
| `linear` | -6 dB, linear ramp | simple crossfade |

Positions run from `-100` (hard left) through `0` (center) to `+100` (hard right); `left`,
`center`, `right`, `L30`, `R60` also work.

* `pan --mode pan` places a mono (or mono-summed) source. `--mode balance` scales the
  channels of a stereo source instead, keeping its image.
* Movement: `pan --points 0s=-100,4s=0,8s=100 --curve spline` or `--from -100 --to 100 --over 0s..end`.
* Cyclic movement: `autopan --rate 1/2` (beats) or `--rate 0.5Hz`.
* While mixing: `mix vox.wav gtr.wav keys.wav --gains 0dB,-3dB,-6dB --pans 0,-40,+40`.
* `widen --amount 130% --mono-below 120Hz` widens the image but keeps the bass mono.
