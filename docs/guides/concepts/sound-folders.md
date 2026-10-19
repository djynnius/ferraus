# Sound folders and instrument switching

Instruments come from four places:

* the sounds registry: free instruments ferraus downloads, verifies, and installs
  (`ferraus sounds search piano`, `ferraus sounds install gm-basic -y`);
* folders you point at with `--sound-path <dir>` (repeatable), the `FERRAUS_SOUND_PATH`
  environment variable, or `[sounds] paths` in the config: SoundFonts (`.sf2`), SFZ libraries,
  impulse responses, and plugins;
* the standard CLAP and VST3 plugin folders (`ferraus plugin scan`);
* the built-in preview synth, which needs no files, so rendering always works.

`ferraus sounds list` shows what is installed and found; `ferraus sounds list --path <dir>`
shows what one folder contains; `ferraus sounds scan <dir>` indexes a large folder once so
later lookups are fast.

## How a name is resolved

`render --instrument`, `render --map`, `hum --to`, and `score perform --instrument` resolve a
name in this order:

1. an alias (`ferraus sounds alias piano=salamander-grand-piano`);
2. an installed or `--sound-path` SoundFont or SFZ library whose name or category matches
   (`strings` finds `Strings.sf2`);
3. a General MIDI program name or number (`"String Ensemble 1"`, `Flute`, `gm:48`, or a family
   such as `ensemble`), played by the `gm-basic` SoundFont when it is installed, otherwise by
   the built-in synth;
4. an explicit source: `sf2:<path>[@bank:program]`, `sfz:<path>`, `builtin:<timbre>`
   (`piano`, `strings`, `organ`, `lead`, `bass`, `pad`, `bell`, `brass`, `guitar`), or
   `plugin:"Name"`.

Plugin instruments are always named with `plugin:"Name"` (for example
`--instrument 'plugin:"Surge XT"'`): in this release a plugin's plain name is not matched in
step 2, even though `sounds list` shows scanned plugins.

An ambiguous name lists the candidates with a runnable hint for each; an unknown one fails
with exit code 2 and the hint `ferraus sounds list`.

```bash
ferraus render part.mid --instrument strings -o part.strings.wav
ferraus render song.mid --map "1=piano,2=strings,10=drums" -o song.wav
ferraus render song.mid --swap piano=strings -o song.wav
ferraus render part.mid --sound-path ~/my-sounds --instrument strings -o part.wav
ferraus hum idea.wav --to ensemble -o idea.wav
```

The resolved instrument (file, program, plugin id, preset) is recorded in the JSON result and
the sidecar, so a render is reproducible even if aliases or folders change later:

```json
{"instrument": {"given": "Flute", "via": "gm", "program": 73, "program_name": "Flute",
                "source": {"type": "sf2", "path": ".../sounds/gm-basic/GeneralUser-GS.sf2", "preset": {"bank": 0, "program": 73}}}}
```

See [Host a plugin](../tutorials/host-a-plugin.md) for plugins and `sounds install`.
