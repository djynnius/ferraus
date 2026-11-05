# Space, ducking, and movement

## Tempo math

| Musical value | ms at tempo T |
|---|---|
| 1/4 note | 60000 / T |
| 1/8 note | 30000 / T |
| dotted 1/8 | 45000 / T |
| 1/16 note | 15000 / T |
| 1/64 note | 3750 / T |
| one bar of 4/4 | 240000 / T |

Write delay times and pre-delays in milliseconds in the pipeline (with the note value in a
comment). Beat values such as `time: 1/4` also work, but only when `pipeline run` gets
`--bpm`; without it ferraus warns `no_tempo` and assumes 160 BPM.

## Reverb sends

One reverb per family, fed by a `mix` stage of the dry stems at send levels, processed with
`mix: 100%` (wet only), then summed with the buses. Starting values:

| Return | Type | Decay | Pre-delay | lowcut / highcut | Fed by (send level) |
|---|---|---|---|---|---|
| vocal plate | plate | 2-4 beats, 0.8-2.2 s | 1/64 note (20-50 ms) | 200 Hz / 7 kHz | lead -12..-18 dB, BVs -6..-10 dB |
| room (drums) | room | 0.6-1.2 s | 0-10 ms | 250 Hz / 8 kHz | snare -12..-18 dB, toms -14..-20 dB |
| hall (music) | hall | 1.6-3 s | 10-30 ms | 250 Hz / 6 kHz | pads -10..-14 dB, keys/guitars -14..-20 dB |

Never send kick, bass, sub or 808 to a reverb in a modern mix; the lowcut on every return
backs that up. Dark (low highcut) and late (long pre-delay) pushes a part back; bright and
early pulls it forward.

## Delay sends

| Use | Time | Feedback | Filter | Level |
|---|---|---|---|---|
| vocal depth (subtle) | 1/4 or dotted 1/8 | 20-30% | lowpass:4-6kHz | send -16..-22 dB; return 12-18 dB under the vocal |
| vocal throw (one word) | 1/4 or 1/2 | 30-45% | lowpass:4kHz | `envelope` on the send to open for one word |
| slapback (rock, rockabilly, rap doubles) | 80-140 ms | 0-10% | lowpass:5kHz | send -12..-16 dB |
| ping-pong width | 1/8 or dotted 1/8 | 20-35% | lowpass:6kHz | `pingpong: true` |

## Ducking (external sidechain)

`compress` with `sidechain: $<key stage>` turns the stage's input down while the key plays.
The amount is the stage's `gain_reduction_max_db`; set `threshold` relative to the key's
level: start about 12 dB under the key's post-fader true peak (the detector follows RMS over
10 ms, which sits well above the key's LUFS readings on drums) and adjust in 2 dB steps until
the reduction is in range.
Keyed by a kick, 808 or bass, set `sidechain_hpf: 0Hz`: the default 80 Hz detector
high-pass would hide most of the key. Keyed by a vocal, the default is fine. With an external
key, `makeup: auto` resolves to 0 dB (the input did not drive the reduction).

| What ducks | Key | Amount | Attack / release | When |
|---|---|---|---|---|
| bass | kick | 2-4 dB | 1 ms / 80-120 ms | kick and bass co-active (masking `co_activity` ≥ 0.3) |
| sub, 808 | kick | 3-6 dB | 1 ms / 60-100 ms | hiphop, edm |
| pads, synths (pump) | kick | 4-8 dB | 1 ms / about a 1/8 note | edm, house: an effect, not a fix |
| pads, keys, guitars | lead vocal | 1-3 dB | 10 ms / 150-250 ms | masking regions in 1-4 kHz you did not want to EQ away |
| synth lead playing under the vocal | lead vocal | 3-4 dB | 10 ms / 200 ms | arrangement clash; note it in the report |
| reverb and delay returns | lead vocal | 3-6 dB | 5 ms / 200-400 ms | always worth it: clarity while singing, bloom between phrases |

Ducking by more than 8 dB on a musical part is audible as an effect; fine in EDM, not in an
acoustic ballad.

## Movement and automation

- Gain rides: `envelope` with time=gain points on a bus or a stem (gates.md has the
  per-section vocal ride).
- Pan movement: `pan --points` or `autopan` on fx, risers, arps.
- Width contrast between sections: give chorus-only parts (BVs, pads, doubled guitars) the
  widest settings; verses then feel narrower without automation.
- Fades: `fade` on the mix bus for intros and endings the arrangement did not resolve.
