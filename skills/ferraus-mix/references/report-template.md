# MIX_NOTES.md template

Keep it short and numeric. The producer should be able to find any decision in `mix.yaml`
from this page.

```markdown
# Mix notes: <song title>

**Session:** <n> stems, <sample rate>, <length>; genre <genre> (<given | inferred: reason>);
tempo <bpm> (<source>); key <key>; reference <file or none>.

## Result

| Gate | Value | Window | |
|---|---|---|---|
| mix verify | pass / fail (<failed checks>) | | ✅ / ❌ |
| balance stage corrections | all within ±0.5 dB (departures: <stem +x dB, why>) | ±0.5 dB | ✅ |
| integrated loudness | -17.2 LUFS | -24..-14 | ✅ |
| true peak | -3.4 dBTP | ≤ -3 | ✅ |
| vocal-to-music | -1.8 LU | -4..0 (pop) | ✅ |
| space: vocal returns vs dry vocal / instrument returns vs dry | -14.2 / -20.4 LU | -18..-10 / -24..-14 | ✅ |
| stereo: balance / correlation / mono loss | +0.3 dB / 0.41 / 1.6 dB | ±1 / ≥0.2 / ≤3 | ✅ |
| low-end correlation (<120 Hz) | 0.97 | ≥ 0.9 | ✅ |

## Decisions

- **Intake:** <stems treated as mono, stems skipped (silent), problems fixed and why>.
- **Balance:** anchor <role> at <LUFS>; departures from the template and why.
- **Processing:** one line per stem with the moves that matter and their measurements
  (e.g. "bass: 4.1 dB mean GR, tube saturation 25% → +2.6 dB at 700 Hz-1.5 kHz").
- **Conflicts:** masking pairs fixed (score before → after), ducks (key, amount).
- **Stereo:** placement map in one line.
- **Space:** returns, decay and pre-delay, send levels.
- **Bus:** glue GR, trim.

## Convention-based choices (judgment, not measurement)

- <e.g. "hats placed right, shaker left (audience perspective)"> → stage `drums_bus`.
- <e.g. "plate on vocal rather than hall (pop convention)"> → stage `vox_verb`.

## Open issues

- <gates left failing, with numbers and suggested fixes at the source>

## Files

`mix.wav`, `mix.yaml`, `prints/instrumental.wav`, `prints/acapella.wav`, bus prints.
Next: master with the `ferraus-master` skill or
`ferraus master mix.wav --target streaming --genre <genre> -o master.wav`.
```

# LISTEN.md template

One page per round, written for someone with headphones on: what to play, what changed,
what to decide. Every pair of clips is matched in loudness.

```markdown
# Listen: <song title>, v<N>

Play `mix/versions/v<N>.wav`, or the clips below.

## Since v<N-1>

- <the change> (stage `<id>`): <measurement before → after>

## Questions

1. **<Section>, <m:ss>-<m:ss>: <what to listen for>.** A = `listen/q1-a.wav` (<current>),
   B = `listen/q1-b.wav` (<alternative>). A or B?
   <What differs, with the numbers: "B cuts the guitars at 400 Hz by 2 dB instead of 4;
   vocal-to-music -1.9 LU instead of -2.1">.
2. ...

## What I need from you

- <material that would fix a source problem, and the measurement that shows it: "the kick
  track is a trigger mic with nothing below 120 Hz; the kick-in mic or a sample would give
  the low end the pop curve expects">

## How to answer

"1: B", "vocal up a hair", "at 2:10 the guitars are too loud", or a new file.
```
