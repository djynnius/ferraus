# Sound folders and instrument switching

Instruments come from the sounds registry (`ferraus sounds install piano`), from folders you
point at with `--sound-path <dir>` (repeatable), `FERRAUS_SOUND_PATH`, or `[sounds] paths` in
the config, and from the standard CLAP/VST3 plugin directories.

An instrument name resolves in this order:

1. an alias (`ferraus sounds alias piano=<id>`),
2. an installed or scanned sound whose name or tags match (`strings`, `nylon guitar`),
3. a General MIDI program (`gm:48`, `"String Ensemble 1"`) on the default GM soundfont,
4. an explicit source: `plugin:"Surge XT"`, `sf2:<path>`, `sfz:<path>`.

An ambiguous name lists the candidates with a runnable hint for each.

```bash
ferraus render part.mid --instrument strings -o part.strings.wav
ferraus render song.mid --map "1=piano,2=strings,10=drums" -o song.wav
ferraus render song.mid --swap piano=strings -o song.wav
ferraus hum idea.wav --to ensemble -o idea.wav
```

The resolved instrument (file, plugin id, preset) is written to the sidecar, so a render is
reproducible even if aliases change later.
