# Clean a podcast

This walkthrough takes a raw spoken-word recording with mains hum and background hiss to a
clean episode at podcast loudness. Every command was run against ferraus 0.1.0 on a test
recording; the outputs shown are trimmed copies of what it printed.

**You need:** ferraus, a recording (`episode.wav` here; any format ferraus reads works), and
headphones. No models or downloads are needed for steps 1 to 6.

> **Making a test file.** If you want to follow along without your own recording, this builds
> one: speech (macOS `say`; any voice recording works), pink hiss, and 60 Hz hum.
>
> ```bash
> say -o speech.aiff "Welcome back to the show. Today we are talking about home recording."
> ferraus convert speech.aiff -o voice.wav --sample-rate 48000
> ferraus silence pad voice.wav --start 1.5s -o voice-pad.wav       # room tone before the first word
> ferraus generate noise --color pink --dur 13.5s --amplitude -48dB --sample-rate 48000 -o hiss.wav
> ferraus generate tone --freq 60Hz --dur 13.5s --amplitude -36dB --sample-rate 48000 -o hum.wav
> ferraus mix voice-pad.wav hiss.wav hum.wav -o episode.wav
> ```

## 1. Listen, then ask ferraus what is wrong

Play the first ten seconds. **Listen for:** a low buzz under everything (hum), a steady "shhh"
in the pauses (hiss), and level jumps between sentences.

```bash
ferraus analyze problems episode.wav
```

```json
{
  "clean": false,
  "counts": {"high": 0, "medium": 2, "low": 1},
  "problems": [
    {"problem": "hum", "severity": "medium",
     "details": "60 Hz hum (harmonics [1]) at -23.1 dB relative to the signal",
     "suggested_fix": "dehum:freq=60Hz"},
    {"problem": "hiss", "severity": "medium",
     "details": "high-frequency noise floor of -57.7 dBFS (6–16 kHz) in quiet passages",
     "suggested_fix": "dehiss"},
    {"problem": "sibilance", "severity": "low",
     "details": "6% of loud frames are dominated by 5-10 kHz energy (peak -16.5 dBFS)",
     "suggested_fix": "deess"}
  ]
}
```

Every `suggested_fix` is an `fx` stage you can paste into a chain. Also note the starting
loudness so you can compare later:

```bash
ferraus analyze loudness episode.wav
```

```json
{"integrated_lufs": -16.4262, "loudness_range_lu": 1.7741, "true_peak_dbtp": -1.8811, ...}
```

## 2. Remove the hum

```bash
ferraus dehum episode.wav -o 1-dehum.wav --json
```

```json
{"ok": true, "command": "dehum", "params": {"freq": "60.001Hz", "harmonics": "8", "depth": "-40dB", ...},
 "metrics": {"freq_hz": 60.001, "hum_reduction_db": 16.7}, ...}
```

ferraus found the mains frequency itself (60.001 Hz; in Europe and most of Africa and Asia it
will find 50 Hz). **Listen for:** the buzz is gone in the pauses, and the voice has not lost
its body. If a deep voice sounds thin, reduce the number of notched harmonics:
`--harmonics 4`. To hear exactly what was removed, add `--residual hum-only.wav`.

## 3. Remove the hiss

Noise reduction works best when ferraus learns what the noise sounds like from a stretch
that contains only noise. Most recordings have a second or two of room tone before the
first word; `--auto` finds the quietest steady second for you.

```bash
ferraus denoise profile 1-dehum.wav --auto -o room.noiseprof --json
```

```json
{"ok": true, "command": "denoise profile",
 "params": {"noise_region": {"range": "0.331s..1.323s", "level_db": -73.1, "method": "auto"}, ...},
 "result": {"profile": "room.noiseprof", "fft_size": 2048, "mean_level_db": -81.2, ...}}
```

Check that `noise_region` really is a pause (play that range). If it landed on a word, give
the region yourself: `--range 0s..1.2s`. Then apply the profile and keep what was removed:

```bash
ferraus denoise 1-dehum.wav --profile room.noiseprof -o 2-denoised.wav --residual 2-removed.wav --json
```

```json
{"ok": true, "command": "denoise",
 "metrics": {"mean_gain_db": -7.1, "input_rms_dbfs": -17.4, "output_rms_dbfs": -17.4, "removed_rms_dbfs": -53.3}, ...}
```

**Listen for:** the pauses are quiet, and there is no "underwater" or chirping sound on the
voice (that sound is called musical noise). Then play `2-removed.wav`: it should be hiss
only. If you can hear words in it, the reduction is too strong; lower it with
`--reduction 8dB` or raise `--artifact-control 70%`. If hiss remains, raise `--reduction 18dB`.

For a recording with no clean pause, or a changing background (traffic, a fan that speeds
up), use a speech model instead of a profile:

```bash
ferraus denoise episode.wav --mode ml --model rnnoise -o clean.wav    # built in, no download
ferraus denoise episode.wav --mode ml -y -o clean.wav                  # DeepFilterNet 3, 8 MB download once
```

## 4. Polish the voice

`enhance-voice` is a preset chain for speech: a rumble filter, a de-plosive for p and b pops,
a de-esser, gentle compression, and a presence lift. Pick `male`, `female`, or `music`.

```bash
ferraus enhance-voice 2-denoised.wav --mode female -o 3-voice.wav --json
```

```json
{"metrics": {"stages": [
  {"op": "highpass", "args": {"freq_hz": 100.0, "order": 4}},
  {"op": "de-plosive", "args": {"below_hz": 150, "ducked_ms": 0.0}},
  {"op": "deess", "args": {"center": "7kHz", "threshold": "-24dB"}},
  {"op": "compress", "args": {"threshold": "-20dB", "ratio": "3:1", "attack": "5ms", "release": "80ms"}},
  {"op": "eq", "args": {"band": ["peak:250Hz:-1dB:q=1", "peak:4kHz:+2dB:q=1", "highshelf:12kHz:+2dB"]}}]}}
```

**Listen for:** clearer consonants, softer "s" sounds, and no boom on plosives. If it sounds
too processed, blend it back: `--mix 60%`.

## 5. Even out the level, then set the loudness

Process the last steps in one `fx` pass. Each separate command writes an integer WAV file,
and a stage that adds gain (the leveler does) can push peaks past full scale and clip in the
file it writes. Inside one `fx` run the audio stays in floating point until the final
`normalize`, whose limiter catches the peaks.

```bash
ferraus fx 3-voice.wav -o episode.clean.wav --fx leveler --fx "normalize:lufs=-16" --json
```

`leveler` raises quiet sentences and lowers loud ones slowly, like a careful engineer riding
a fader. `normalize --lufs -16` sets the integrated loudness to -16 LUFS (the usual target for
stereo podcasts; the podcast spec for **mono** is -19 LUFS, see
[Loudness standards](../concepts/loudness.md)) and limits true peaks to -1 dBTP.

**Listen for:** every sentence at a similar level, with no pumping (the background swelling
in the pauses). If the pauses get noisy, the leveler is boosting room tone: raise its noise
gate, `--fx "leveler:gate=-50dB"`.

## 6. Check the result

```bash
ferraus analyze problems episode.clean.wav
ferraus analyze loudness episode.clean.wav
```

```json
{"clean": true, "counts": {"high": 0, "medium": 0, "low": 2}, ...}
{"integrated_lufs": -16.0249, "loudness_range_lu": 1.1295, "true_peak_dbtp": -1.0191, ...}
```

## The whole thing in one command

Once you know the settings that suit your room, run every stage in one pass (the `fx` stage
names and keys are the command names and flags):

```bash
ferraus fx episode.wav -o episode.clean.wav \
  --fx dehum --fx "denoise:auto" --fx leveler --fx "enhance-voice:mode=female" --fx "normalize:lufs=-16"
```

To trim long pauses as well, add `ferraus silence trim episode.clean.wav --all --threshold -50dB
--max-gap 700ms -o tight.wav`. Choose the threshold from `ferraus silence detect`: it should sit
a few dB above the floor level that command reports for the pauses.

For a whole folder of episodes, see the [batch podcast cleanup](../cookbook/batch-podcast-cleanup.md)
recipe.

## Reference

[analyze problems](../commands/analyze_problems.md) ·
[dehum](../commands/dehum.md) · [denoise profile](../commands/denoise_profile.md) ·
[denoise](../commands/denoise.md) · [enhance-voice](../commands/enhance_voice.md) ·
[leveler](../commands/leveler.md) · [normalize](../commands/normalize.md) ·
[fx](../commands/fx.md) · [silence trim](../commands/silence_trim.md)
