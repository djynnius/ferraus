# Master a track

Mastering takes a finished stereo mix and prepares it for release: balanced tone, controlled
dynamics, a safe stereo image, and the right loudness for where it will be played, without
clipping. `ferraus master` does all of it in one command, and every stage is an ordinary
ferraus command you can inspect, edit, and re-run. Outputs are trimmed copies of what
ferraus 0.1.0 printed for a 21-second test mix.

**You need:** a stereo mix (`mix.wav`) with some headroom (peaks below about -3 dBFS, no
limiter on the mix bus). No downloads are needed.

## 1. Measure the mix

```bash
ferraus master analyze mix.wav --json
```

```json
{"ok": true, "command": "master analyze",
 "metrics": {"integrated_lufs": -19.61, "true_peak_dbtp": -4.94, "looks_mastered": false},
 "result": {"loudness": {"integrated_lufs": -19.61, "loudness_range_lu": 0.84, "true_peak_dbtp": -4.94, "crest_factor_db": 17.02, "plr_db": 14.67},
            "tonal_balance": {"against": "genre curve: acoustic", "rms_deviation_db": 3.23,
                              "largest_deviations": [{"center_hz": 10000.0, "delta_db": 8.2, "direction": "too much"},
                                                     {"center_hz": 800.0, "delta_db": -7.0, "direction": "too little"}, ...]}, ...}}
```

This is the report the automatic master decides from: how loud the mix is, how much peak
room it has, and how its tone compares with a typical curve for the genre (guessed here as
"acoustic"; set it with `--genre pop|hiphop|edm|rock|acoustic|classical|jazz|podcast`).

**Listen for** the same things before you start: harsh or dull top end, boomy or thin bass,
and whether the loud parts already sound squashed.

## 2. Master it

```bash
ferraus master mix.wav -o master.wav --report report.json --ab --json
```

```json
{"ok": true, "command": "master",
 "outputs": [{"path": "master.wav"}, {"path": "master.ab-before.wav", "role": "ab-before"},
             {"path": "master.ab-after.wav", "role": "ab-after"}, {"path": "report.json", "role": "report"}],
 "params": {"target": "streaming", "genre": "acoustic", "intensity": "medium", "warmth": "15%", "width": "100%", ...},
 "metrics": {"integrated_lufs": -14.0, "true_peak_dbtp": -1.02, "limiter_gain_reduction_db": 0.3,
             "target_reached": true, "verify_pass": true, "ab_loudness_difference_lu": 0.0}}
```

The default target is `streaming`: -14 LUFS integrated, true peak at or below -1 dBTP, which
is where Spotify, YouTube, Tidal, and Amazon normalize playback. The master also trimmed the
2-second silent tail of the test mix (`--trim-silence -70dB`; `off` keeps it).

**Listen to the A/B pair, not the master against the mix.** `master.ab-before.wav` and
`master.ab-after.wav` are matched in loudness (here within 0.0 LU), so you hear what mastering
changed rather than "louder sounds better". Listen for: a clearer, more even tone; bass that
is solid but not boomy; no pumping (the level dipping on every kick); no harshness on loud
cymbals or vocals.

## 3. Check it against the delivery spec

```bash
ferraus master verify master.wav --target streaming
```

```json
{"pass": true, "target": {"name": "streaming", "lufs": -14.0, "true_peak_dbtp": -1.0}, "tolerance_lu": 0.5,
 "checks": [
   {"check": "loudness", "status": "pass", "value": -14.0, "limit": "-14 LUFS ± 0.5 LU"},
   {"check": "true_peak", "status": "pass", "value": -1.02, "limit": "<= -1 dBTP (4x oversampled)"},
   {"check": "clipping", "status": "pass", "value": 0},
   {"check": "dc_offset", "status": "pass", "value": 0.00011},
   {"check": "out_of_phase_bass", "status": "pass", "value": 1.0},
   {"check": "correlation", "status": "pass", "value": 1.0}, ...]}
```

A failing file exits with code 9 and a runnable fix for each failed check. The unmastered mix,
for example:

```bash
ferraus master verify mix.wav --target streaming --json
```

```json
{"ok": false, "metrics": {"pass": false, "failed_checks": ["loudness"],
  "checks": [{"check": "loudness", "status": "fail", "value": -19.61,
              "details": "integrated loudness -19.61 LUFS is -5.61 LU from the -14 LUFS target",
              "fix": "normalize:lufs=-14LUFS"}, ...]},
 "error": {"code": "verification_failed", "exit_code": 9, ...}}
```

## 4. Change what the master does

Plan without writing audio, edit the plan, then run the edited plan:

```bash
ferraus master plan mix.wav --target apple --intensity light -o plan-apple.json
```

```json
{"version": 1,
 "target": {"name": "apple", "lufs": -16.0, "true_peak_dbtp": -1.0},
 "intensity": {"name": "light", "limiter_max_gain_reduction_db": 2.0},
 "stages": [
   {"stage": "preflight", "op": "dc-remove", "args": {"freq": "5Hz"}, "why": "remove DC offset (5 Hz high-pass)"},
   {"stage": "preflight", "op": "silence trim", "args": {"threshold": "-70dB", "keep": "10ms"}, ...},
   {"stage": "corrective_eq", "op": "eq", "args": {"band": ["peak:25Hz:+3dB:q=4.32", ...], "linear_phase": "true"},
    "why": "match-eq toward the acoustic genre curve at 50% (1/3-octave bands, at most 4 dB per band; ...)"},
   {"stage": "corrective_eq", "op": "highpass", "args": {"freq": "20Hz", "order": "4"}, ...},
   {"stage": "dynamics", "op": "multiband", "args": {"crossovers": "120Hz,2kHz,10kHz", "ratio": "1.5:1", ...}, ...},
   {"stage": "dynamics", "op": "compress", "args": {"threshold": "-22dB", "ratio": "1.5:1", ...}, ...},
   ...]}
```

Every stage is a ferraus command with its arguments, and `why` explains the choice. Change a
value (say, the compressor threshold), delete a stage you do not want, then:

```bash
ferraus master mix.wav --plan plan-apple.json -o master-apple.wav
ferraus master verify master-apple.wav --target apple
```

Common adjustments without editing a plan:

| You hear | Try |
| --- | --- |
| Too bright or too dull compared with a record you like | `--reference ref.wav --match-amount 75%` |
| Squashed, lifeless | `--intensity light` |
| Needs to be louder (club, DJ) | `--target club --intensity heavy` |
| Too wide, weak in mono | `--width 90%` or `--no-widen` |
| Muddy low end in stereo | `--bass-mono 150Hz` |
| Added grit you do not want | `--warmth 0%` |

Targets: `streaming` (-14 LUFS), `apple` (-16), `youtube` (-14), `club` (-8, -0.3 dBTP), `cd`
(-10, 16-bit 44.1 kHz), `broadcast` (-23, EBU R128; `--standard atsc` for -24 LKFS),
`podcast` (-16 stereo, -19 mono), or `custom` with `--lufs` and `--true-peak`. See
[Loudness standards](../concepts/loudness.md).

## 5. Write the release files

```bash
ferraus master deliver master.wav --tags title=Demo,artist=Ferraus --loudness-sheet -o release/demo.wav
```

```text
release/demo.wav24.wav   release/demo.wav16.wav   release/demo.mp3
release/demo.loudness.json   release/demo.loudness.csv
```

```text
format,path,sample_rate,channels,bit_depth,bitrate,duration_s,integrated_lufs,loudness_range_lu,short_term_max_lufs,true_peak_dbtp,sample_peak_dbfs,plr_db,measured
wav24,release/demo.wav24.wav,48000,2,24,,19.597,-14.0,0.25,-13.74,-1.02,-1.11,12.98,decoded deliverable
wav16,release/demo.wav16.wav,44100,2,16,,19.597,-14.02,0.26,-13.76,-1.05,-1.32,12.97,decoded deliverable
mp3,release/demo.mp3,44100,2,,320000,19.597,-14.04,0.26,-13.78,-0.85,-0.85,13.2,decoded deliverable
```

Every deliverable is decoded and measured again. Note the MP3's true peak (-0.85 dBTP): lossy
encoding adds small overshoots, so check the sheet before you upload. Add `flac` and `aac`
with `--formats wav24,flac,aac` (AAC needs FFmpeg), and an ISRC with `--isrc`.

## Reference

[master](../commands/master.md) · [master analyze](../commands/master_analyze.md) ·
[master plan](../commands/master_plan.md) · [master verify](../commands/master_verify.md) ·
[master deliver](../commands/master_deliver.md) · [normalize](../commands/normalize.md)
