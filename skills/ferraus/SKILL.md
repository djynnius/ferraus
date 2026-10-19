---
name: ferraus
description: Edit, repair, separate, tune, transcribe, notate, and master audio with the ferraus CLI. Use when the user wants to convert or trim audio, apply effects, remove noise, split a song into stems, pitch-correct a vocal, turn humming into MIDI or an instrument part, make sheet music from audio or MIDI, perform a sheet-music PDF, host CLAP/VST3 instruments, or master a track to a loudness target.
---

# ferraus

ferraus is installed as the `ferraus` command. It is non-interactive and machine-friendly.

## Always

- Add `--json` and read the envelope: `ok`, `outputs[].path`/`role`, `params`, `metrics`,
  `warnings`, `error.hint`.
- Start with `ferraus info <file> --json` and, for repair work, `ferraus analyze problems <file> --json`.
- Use `--dry-run --json` to preview long or destructive work. Pass `--overwrite` only when
  replacing an output is intended.
- If a command exits with code 5, run the `error.hint` command (or retry with `-y` if the user
  agreed to downloads).
- Find exact flags with `ferraus docs <command>` or `ferraus schema <command>`; list every
  command with `ferraus describe --agent`.

## Common tasks

| Goal | Command |
| --- | --- |
| Convert / trim / join | `ferraus convert a.mp3 -o a.flac` · `ferraus trim a.wav --range 0:30..1:00` · `ferraus concat a.wav b.wav -o ab.wav` |
| Loudness | `ferraus normalize a.wav --lufs -14` · `ferraus analyze loudness a.wav --json` |
| Effects chain | `ferraus fx in.wav -o out.wav --fx "highpass:freq=80Hz" --fx "compress:preset=vocal" --fx "reverb:mix=18%"` |
| Noise | `ferraus denoise take.wav --auto` · `ferraus denoise take.wav --noise-range 0s..2s` · `ferraus dehum take.wav` · `ferraus declick vinyl.wav --auto` |
| Stems | `ferraus separate song.mp3 --stems 4 -y` (`--vocal-split`, `--drum-split`, `--instrumental`) |
| Pitch | `ferraus tune vocal.wav --key auto` · `ferraus pitch a.wav --semitones -2` · `ferraus stretch a.wav --factor 1.1` |
| MIDI | `ferraus transcribe hum.wav -o part.mid` · `ferraus midi quantize part.mid --grid 1/16` · `ferraus render part.mid --instrument strings` · `ferraus hum idea.wav --to piano` |
| Notation | `ferraus score song.wav --layout lead-sheet -o song.musicxml` · `ferraus score part.mid --solfa only --solfa-txt part.txt` · `ferraus score perform piece.pdf -o piece.wav` |
| Mastering | `ferraus master analyze mix.wav --json` → `ferraus master plan mix.wav --json > plan.json` → `ferraus master mix.wav --plan plan.json -o master.wav` → `ferraus master verify master.wav --target streaming` |
| Check a result | `ferraus compare before.wav after.wav --json` |

## Notes

- Outputs default to `<input>.<command>.<ext>` beside the input; `-o` takes templates
  `{name}`, `{stem}`, `{index}`, `{ext}`.
- Units: `1.5s`, `4b`, `0s..2s`, `-6dB`, `-14LUFS`, `2kHz`, `C4`, `4:1`, `80%`, `1/16`.
- Each output has a `<output>.ferraus.json` sidecar recording exactly what was done.
