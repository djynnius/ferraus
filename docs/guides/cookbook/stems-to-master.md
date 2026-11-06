# Stems to master

A producer's stems to a mix that is ready for mastering, then to verified masters, with every
decision taken from a measurement. This is the workflow the `ferraus-mix` and
`ferraus-master` skills give an agent; here it is by hand. Numbers below come from a 10-stem,
80-second pop session (kick, snare, hats, bass, keys, pad, guitar arpeggio, lead vocal,
backing vocals, synth lead at 100 BPM in A minor). JSON is trimmed.

## 1. Intake

```bash
ferraus info "stems/05 Keys.wav" --json
ferraus analyze stereo "stems/05 Keys.wav" --json
ferraus analyze problems "stems/04 Bass.wav" --min-severity medium --json
```

Three findings from that session show why a stem is read differently from a finished song:

* `05 Keys.wav` is stereo with `correlation` 1.0 and `width` 0: a mono source in a stereo
  file. Collapse it (`channels --to mono`) to pan it, or decorrelate it with `chorus` to make
  it wide: `widen` alone cannot, because there is no side signal to scale.
* Most stems report `silence_head`: a part that enters at bar 9 starts with silence. Trimming
  it would break the stems' shared start, so leave it.
* The bass reports `hum` at high severity. Its G1 notes sit at 49 Hz, which looks like mains
  hum; `dehum` would notch the bass line. Hum is real only if it is there while the part is
  silent.

### The song map, and a bad bar

A rough sum of the stems (section 2 writes one with `-o`) gives the song's structure:

```bash
ferraus analyze sections rough.wav --bpm 100 --json
```

```json
{"result": {"tempo_bpm": 100.0, "tempo_source": "given", "sections": [
  {"label": "A", "role": "intro", "start_s": 0.0, "end_s": 9.6},
  {"label": "B", "role": "verse", "start_s": 9.6, "end_s": 28.8},
  {"label": "C", "role": "chorus", "start_s": 28.8, "end_s": 48.01},
  {"label": "B", "role": "verse", "start_s": 48.01, "end_s": 57.6},
  {"label": "C", "role": "chorus", "start_s": 57.6, "end_s": 80.01}],
  "repeats": [{"a": 1, "b": 3, "label": "B", "similarity": 0.84, "offset_s": 38.41},
              {"a": 2, "b": 4, "label": "C", "similarity": 0.97, "offset_s": 28.81}]}}
```

Every boundary is where the session's arrangement changes. The fourth section is a break that
reuses the verse's vocal and bass, so it shares the verse's letter. With `--vocal` (the lead
vocal stem) each section also reports how much of it is sung, and the role guesses firm up.

`repeats` is what fixes a bad bar. To try it, a 0.4 s noise burst was added to the bass at
62.0 s, in the second chorus. `analyze problems` does not flag a burst like that (it looks
for clicks), but the bass has no business above 3 kHz, so its high band shows it:

```bash
ferraus highpass bass_bad.wav --freq 3kHz --order 4 -o bass_hi.wav --json
ferraus analyze loudness bass_hi.wav --timeline --step 100ms --json
```

Eight 100 ms steps from 62.1 s rise from below -70 to as high as -15 LUFS. The twin of the
bar 60.0-62.4 s is 28.81 s earlier, in the first chorus. The bars on either side match
theirs (`analyze similarity` 0.93 and 0.94, `same`; the bass is humanized, so the bars never
null), and `patch` copies the clean bar over the damaged one:

```bash
ferraus patch bass_bad.wav --range 60s..62.4s --from 31.2s -o bass_fixed.wav --json
```

The result reports `level_difference_db` 0.22 (the two passes were played at the same level).
The 4-8 kHz band of that second is back to about -105 dB, as in the clean take, the file is
still 80.0 s long, and everything outside the edit nulls against the input at -142 dB.

## 2. Faders from a genre template

```bash
ferraus mix balance stems/*.wav --genre pop --anchor-lufs -21LUFS --render --json
```

```json
{"result": {"anchor": "vocal", "genre": "pop",
  "inputs": [{"path": "stems/01 Kick.wav", "role": "kick", "role_source": "filename", "integrated_lufs": -17.88, "target_lufs": -24.0, "gain_db": -6.1}, ...],
  "gains": ["-6.1dB", "-4.2dB", "-3.4dB", "-9.7dB", "-8.1dB", "-10.3dB", "-6.5dB", "-2.6dB", "-5.3dB", "-5.3dB"],
  "render": {"integrated_lufs": -17.62, "true_peak_dbtp": -3.34, "headroom_ok": true}}}
```

Roles come from the file names; `--roles` overrides them (`auto` keeps the guess for one
input). Here `10 Synth Lead.wav` was read as `lead` (the main melody of an instrumental),
but it plays under the vocal, so the session gives it the `synth` role and a fader 3 dB
lower. The template places each role at a fixed offset from the lead vocal; these are
starting faders.

## 3. The session as a pipeline

The whole mix is one pipeline file: a chain per stem ending in its pan (off-centre mono
parts) and fader, buses, reverb and delay sends taken after the faders, an instrumental and
an a cappella summed into the final mix, and measurement stages. The `ferraus-mix` skill ships a complete, tested
template (`skills/ferraus-mix/references/pipeline-template.yaml`).

```bash
ferraus pipeline run mix/mix.yaml --bpm 100 --overwrite --json
```

Every stage reports what it measured, so one run returns the mix and its numbers:

```json
{"result": {"computed": 45, "plan": [
  {"id": "vox_ride", "result": {"target_lu": -1.23, "offset_before": {"spread_lu": 4.86}, "offset_after": {"spread_lu": 2.54},
    "points": "0s=+4.0dB,9.67s=+4.0dB,9.79s=+3.5dB,9.84s=+2.5dB,9.94s=-0.3dB,...", "loudness_change_lu": -0.13}},
  {"id": "vox_fx", "result": {"chain_metrics": [{"op": "compress", "metrics": {"gain_reduction_mean_db": 4.11, "gain_reduction_max_db": 8.83}}, ...]}},
  {"id": "bass_duck", "result": {"metrics": {"gain_reduction_mean_db": 0.43, "gain_reduction_max_db": 4.22}}},
  {"id": "balance", "result": {"gains": ["0dB", "0dB", "0dB", "0dB", "0dB", "0dB", "0dB", "0dB", "0dB", "0dB"]}},
  {"id": "vocal_level", "result": {"integrated_lufs": -20.3}},
  {"id": "inst_level", "result": {"integrated_lufs": -20.76}}, ...]}}
```

The `balance` stage runs `mix balance` on the processed stems, so its gains are corrections.
They caught two traps while the template was built: the `vocal` compressor preset adds auto
makeup gain (the vocal read 5.6 dB hot until `makeup: 0dB`), and `pans` on a bus `mix` stage
applies a -3 dB pan law to mono inputs (the panned hats, keys and guitar read 3 dB low until
they were panned inside their chains instead). With both fixed, the corrections are all 0 dB.

The `vox_ride` stage is the vocal rider: before its compressors, the lead vocal is turned up
and down word by word against every instrument (`ride --against`), the volume automation an
engineer draws by hand. The spread of vocal-minus-music while singing (p10 to p90) went from
4.86 to 2.54 LU, and `points` holds the ride as `envelope` keyframes that reproduce it
exactly. None of the gates below moved.

The vocal-to-music balance is then set in the `final` stage: a cappella LUFS minus
instrumental LUFS, plus their final gains (0 and -2.5 dB), is -2.0 LU, the centre of the pop
window of -4 to 0 LU.

## 4. Masking and ducking

```bash
ferraus analyze masking "stems/01 Kick.wav" "stems/04 Bass.wav" --json
```

```json
{"metrics": {"overall": 0.604, "co_activity": 0.7852, "regions": 1},
 "result": {"regions": [{"center_hz": 125.0, "margin_db": 0.3, "cut_db": -2.0, "suggested_fix": "eq:band=peak:125Hz:-2dB:q=1.4", "apply_to": "b"}],
  "summary": "04 Bass.wav masks 01 Kick.wav most at 100 Hz–125 Hz (score 0.60, playing along 79% of the time 01 Kick.wav plays): cut 04 Bass.wav by 2 dB at 125 Hz (eq:band=peak:125Hz:-2dB:q=1.4)."}}
```

The first file keeps its place, the second yields. The score counts a band only while the
yielding part is within 10 dB of the priority part there (`margin_db` is how far the kick
sits above the bass in that band while each plays), and the suggested cut is the one that
takes the band under the threshold. Measure on balanced stems: the pipeline template has
masking stages that read the processed, post-fader stems. On raw stems whose levels mean
nothing yet, `--levels ignore` compares spectral shapes only (here it pointed at 100 Hz, -3
dB). In the session the bass got that 3 dB cut at 100 Hz and the guitar a 3.5 dB cut at
630 Hz (vocal vs guitar), after which the pipeline's masking stages reported no regions on
any priority pair.

Where a cut would hollow a part out, it ducks instead:

```bash
ferraus compress "stems/04 Bass.wav" --sidechain "stems/01 Kick.wav" --sidechain-hpf 0Hz --threshold -20dB --ratio 4:1 --attack 1ms --release 90ms -o bass_duck.wav --json
```

## 5. The gate before mastering

```bash
ferraus mix verify mix/mix.wav --genre pop --json
```

```json
{"ok": true, "metrics": {"pass": true, "failed_checks": []},
 "result": {"summary": "ready for mastering: 14 checks pass",
  "checks": [{"check": "true_peak", "value": -3.41}, {"check": "integrated_lufs", "value": -19.47},
             {"check": "plr", "value": 16.05}, {"check": "mono_bass", "value": 1.0},
             {"check": "tonal_balance", "value": -5.8, "details": "63Hz is 5.8 dB too weak vs genre curve: pop (octave-smoothed, overall level removed; ...)"}, ...]}}
```

A failed check exits with code 9 and a runnable `fix` (an `fx` stage) per check; a mix
that was already limited fails `plr` with the advice to re-bounce without the bus limiter.

## 6. Master

```bash
ferraus master analyze mix/mix.wav --genre pop --bpm 100 --json
ferraus master plan mix/mix.wav --genre pop --target streaming --trim-silence off --bpm 100 -o plan.json --json
```

`--bpm` replaces the detected tempo (this session was detected at 200 BPM, an octave off),
and the planner derives multiband and limiter release times from it. Review the plan: here
the corrective EQ wanted +3.9 dB at 25 Hz, +2.7 dB at 31.5 Hz and +2.8 dB at 40 Hz to chase
the pop curve on a mix that simply has little sub, so those boosts were capped at +2 dB. After editing a plan, `--relevel` searches
the final gain and limiter again so it still lands on the target:

```bash
ferraus master mix/mix.wav --plan plan.json --relevel -o master.wav --report master.report.json --ab --json
ferraus master verify master.wav --target streaming --json
ferraus analyze similarity master.ab-before.wav master.ab-after.wav --json
ferraus master deliver master.wav --formats wav24,wav16,mp3 --loudness-sheet -o release/song.wav --json
```

The report: -19.47 → -14.02 LUFS, true peak -1.04 dBTP, crest factor 18.6 → 15.3 dB, tonal
RMS deviation 3.2 → 1.9 dB, correlation 0.93 → 0.95, 0.8 dB of limiter gain reduction
(relevelled gain +8.78 dB), `master verify` pass. Comparing the loudness-matched A/B pair
(not the mix with the louder master) gave a similarity of 0.87 ("similar") with 1.5 dB of
spectral distance.

## 7. Revisions

"Vocal up a hair" is one value:

```bash
ferraus pipeline run mix/mix.yaml --set stages.final.args.gains=0dB,-2dB --bpm 100 --overwrite --json
```

Only the final sum, the mix bus and the measurements recompute; everything else is cached.
Each full run of a session like this writes about 0.75 GB of stage results to the pipeline
cache; `ferraus pipeline prune --max-size 2GB` (or `--days 0`) clears it when you are done.
