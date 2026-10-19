# Host a plugin

ferraus runs CLAP and VST3 plugins (effects and instruments) offline, so you can use the
plugins you already own in scripts and agent workflows. Each plugin runs in a separate
`ferraus-plughost` process: a plugin that crashes or hangs cannot take ferraus down with it.
This walkthrough uses CLAP. Outputs are trimmed copies of what ferraus 0.1.0 printed with the
small test plugins from the ferraus repository (`crates/ferraus-test-plugin`: a gain, a
delay, and a sine instrument); with your own plugins only the names change.

**You need:** a CLAP plugin (free ones include Surge XT, Dexed, Vital, and the Airwindows
Consolidated set) installed in a standard folder, or a `.clap` bundle in any folder.

## 1. Find your plugins

```bash
ferraus plugin scan
```

The scan looks in the standard folders for your system (on macOS `~/Library/Audio/Plug-Ins/CLAP`,
`/Library/Audio/Plug-Ins/CLAP`, and the same for VST3; on Linux `~/.clap` and `/usr/lib/clap`;
on Windows `%COMMONPROGRAMFILES%\CLAP` and `%LOCALAPPDATA%\Programs\Common\CLAP`; plus every
folder in `CLAP_PATH`), probes each bundle in a separate process, and caches
the result, so later scans only look at new or changed bundles. Plugins somewhere else:

```bash
ferraus plugin scan --paths ~/plugins --json
```

```json
{"ok": true, "command": "plugin scan", "metrics": {"plugins": 5, "bundles": 1, "probed": 1},
 "result": {"plugins": [
   {"name": "Ferraus Test Gain", "id": "ferraus.test.gain", "vendor": "ferraus", "format": "clap", "kind": "audio-effect", ...},
   {"name": "Ferraus Test Sine", "id": "ferraus.test.sine", "format": "clap", "kind": "instrument", "category": "synth", ...},
   ...], "errors": []}}
```

A plugin that fails to load is listed under `errors` with its path; the scan goes on.

```bash
ferraus plugin list                 # everything cached
ferraus plugin list --instruments   # only instruments
```

## 2. Look inside one

```bash
ferraus plugin info "Ferraus Test Gain"
```

```json
{"plugin": {"name": "Ferraus Test Gain", "id": "ferraus.test.gain", "format": "clap", ...},
 "params": [{"id": 0, "name": "gain", "min": -60.0, "max": 24.0, "default": 0.0, "default_text": "0.00 dB", "unit": "dB", ...}],
 "presets": [{"name": "Quiet", "load_key": "quiet"}, {"name": "Loud", "load_key": "loud"}],
 "audio_inputs": [{"name": "in", "channels": 2, "main": true}], ...}
```

`params` are the names you set with `--param`; `presets` are the names `--preset` loads.

## 3. Process audio through an effect

```bash
ferraus plugin run voice.wav --plugin "Ferraus Test Gain" --param "gain=-6dB" -o voice.gain.wav --json
```

```json
{"ok": true, "command": "plugin run",
 "outputs": [{"path": "voice.gain.wav", "duration_s": 11.298, "sample_rate": 48000, "channels": 2}],
 "metrics": {"latency_samples": 0, "tail_s": 0.0},
 "result": {"params": [{"id": 0, "name": "gain", "value": -6.0, "text": "-6.00 dB"}], "preset": null, ...}}
```

* `--param` takes the display units the plugin shows (`-6dB`, `2.5kHz`, `30%`), or a
  normalized value (`norm:0.3`). Repeat it for more parameters.
* `--preset Quiet` loads a preset first, then applies any `--param` values.
* `--tail 3s` keeps processing after the input ends so reverbs and delays ring out.
* `--mix 30%` blends the processed signal with the dry input.
* The plugin's latency is compensated, so the output lines up with the input sample for
  sample. The output has the plugin's channel count: this stereo test plugin turned a mono
  file into a stereo one (add `--channels 1` to get mono back).

**Listen for:** the effect you asked for and nothing else: no clicks at the start (latency
compensation) and no cut-off tails (raise `--tail`).

The sidecar (`voice.gain.wav.ferraus.json`) stores the plugin, its parameters, and its state,
so `ferraus replay voice.gain.wav.ferraus.json --overwrite` repeats the run. To save a configured state
yourself:

```bash
ferraus plugin state save "Ferraus Test Gain" --param "gain=-3dB" -o gain.state.json
ferraus plugin run voice.wav --plugin "Ferraus Test Gain" --state gain.state.json -o voice.state.wav
```

## 4. Play MIDI through an instrument plugin

```bash
ferraus render melody.mid --instrument 'plugin:"Ferraus Test Sine"' -o melody.plugin.wav --json
```

```json
{"ok": true, "command": "render",
 "params": {"instrument": {"given": "plugin:\"Ferraus Test Sine\"", "source": {"type": "plugin", "name": "Ferraus Test Sine"}, "via": "explicit"}},
 "metrics": {"duration_s": 11.57, "notes": 16, "peak_dbfs": -8.08}}
```

Use the `plugin:"Name"` form for plugin instruments (the quotes go inside the argument, so
wrap the whole thing in single quotes in your shell). `hum --to` and `score perform
--instrument` take the same form.

## 5. Plugins and sounds outside the standard folders: `--sound-path`

Point ferraus at a folder of plugins, SoundFonts (`.sf2`), SFZ libraries, and impulse
responses for one run with `--sound-path` (repeatable), for every run with the
`FERRAUS_SOUND_PATH` environment variable, or permanently with `[sounds] paths` in the config:

```bash
ferraus sounds list --path ~/my-sounds       # what ferraus finds in the folder
ferraus sounds scan ~/my-sounds              # index it (fast lookups later)
ferraus plugin run in.wav --sound-path ~/my-sounds --plugin "My Reverb" -o out.wav
ferraus render song.mid --sound-path ~/my-sounds --instrument strings -o song.wav
```

## 6. Install free instruments: `sounds install`

The sounds registry lists free instruments with their licenses and pinned checksums:

```bash
ferraus sounds search piano
ferraus sounds info gm-basic
ferraus sounds install gm-basic -y
```

```json
{"ok": true, "command": "sounds install",
 "result": {"id": "gm-basic", "license": {"spdx": "LicenseRef-GeneralUser-GS-2.0", ...},
            "installed": {"name": "GM Basic (GeneralUser GS)", "type": "sf2", "paths": [".../data/sounds/gm-basic"]}}}
```

The license is printed before anything is installed; downloads need `-y`. After installing
`gm-basic`, every General MIDI name plays from it instead of the built-in synth:

```bash
ferraus render melody.mid --instrument Flute -o melody.flute.wav
```

```json
{"params": {"instrument": {"given": "Flute", "source": {"type": "sf2", "path": ".../GeneralUser-GS.sf2", "preset": {"bank": 0, "program": 73}},
                           "via": "gm", "program": 73, "program_name": "Flute"}}}
```

Vendors that require their own installer (Vital, for example) cannot be downloaded by
ferraus; download them yourself and register the file:
`ferraus sounds install vital --from ~/Downloads/VitalInstaller.zip`. ferraus never runs
installers or asks for administrator rights.

Short names for instruments you use often: `ferraus sounds alias piano=salamander-grand-piano`.

## When something goes wrong

```json
{"ok": false, "error": {"code": "plugin_not_found",
  "message": "no plugin named 'Nonexistent Synth' was found (searched the scan cache and --sound-path folders)",
  "hint": "ferraus plugin scan --paths <folder-with-plugins>", "exit_code": 7}}
```

Exit code 7 covers a plugin that was not found, crashed, or stopped responding (the host is
stopped after `--watchdog`, 60 s by default). See [Troubleshooting](../troubleshooting.md).

## Reference

[plugin scan](../commands/plugin_scan.md) · [plugin list](../commands/plugin_list.md) ·
[plugin info](../commands/plugin_info.md) · [plugin run](../commands/plugin_run.md) ·
[plugin state save](../commands/plugin_state_save.md) · [render](../commands/render.md) ·
[sounds install](../commands/sounds_install.md) · [Sound folders](../concepts/sound-folders.md)
