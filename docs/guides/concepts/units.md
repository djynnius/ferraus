# Units and parsing

Every value on the command line, in fx strings, in pipeline files, and in MCP arguments is
parsed by the same unit parser, so `1.5s`, `-6dB`, `C4`, and `1/8T` mean the same thing
everywhere.

## Time

| You write | Meaning |
| --- | --- |
| `1.5s`, `1500ms`, `2` | seconds (a bare number is seconds) |
| `01:23.5`, `1:02:03.250` | mm:ss.fff and hh:mm:ss.fff |
| `44100smp` | samples |
| `4b`, `4b@120` | beats at the detected or `--bpm` tempo, or at an explicit tempo |
| `-10s` | 10 seconds before the end |
| `start`, `end` | the ends of the input |

Ranges use `..`: `--range 0s..2s`, `--range 1:00..end`, `--range ..30s` (from the start).

## Gain

`-6dB`, `+3dB`, `-6` (dB), `0.5x` (a linear factor), `-14LUFS` (integrated loudness, for
`normalize` and mastering targets), `-1dBTP` (true peak, measured with 4x oversampling).

## Frequency and pitch

`120Hz`, `2.5kHz`, note names `C4` (middle C, 261.63 Hz), `A#3`, `Bb2`, MIDI numbers
`midi:60`. Pitch offsets: `20cents`, `+200cents`, `2st` (semitones).

## Ratios and percentages

`4:1` or `4`. `80%` or the fraction `0.8`; `130%` is 1.3. A bare `80` means 80, not 80%.

## Musical grid

`1/16`, `1/8T` (triplet: two thirds of an eighth), `1/8D` (dotted: one and a half eighths).
Delay times also accept grids (`--time 1/4`), resolved against `--bpm` or the detected tempo.

## Sizes and bitrates

`2GB`, `512MB`; `192k`, `320kbps`.

## When parsing fails

You get exit code 2 and a message naming the flag, the value, and what was expected:

```text
error: --threshold: cannot parse gain 'loud': expected a number with dB, x, LUFS, or dBTP
```
