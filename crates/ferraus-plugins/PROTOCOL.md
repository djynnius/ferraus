# ferraus plugin-host protocol (version 1)

ferraus never loads a plugin into its own process. Every plugin instance runs in its own
host process: `ferraus-plughost` for CLAP (Apache-2.0 OR MIT) or `ferraus-plughost-vst3`
for VST3 (GPL-3.0-only). Both speak this protocol. The types live in
`crates/ferraus-plugins/src/protocol.rs`. The host side is in `src/host_rt.rs` and the
ferraus side in `src/client.rs`.

## Processes

* ferraus starts the host with piped stdin, stdout, and stderr, and no arguments. The host
  is found through `FERRAUS_PLUGHOST` or `FERRAUS_PLUGHOST_VST3`, then next to the ferraus
  executable (and that folder's parent), then on `PATH`.
* One host process loads exactly one bundle and instantiates at most one plugin.
* Before running any plugin code, the host takes a private duplicate of its stdout for the
  protocol and points file descriptor 1 at stderr. Anything a plugin prints goes to stderr
  and cannot corrupt the frames.
* stderr is free text (plugin logs, host diagnostics). ferraus keeps the last few lines and
  adds them to crash and timeout errors. `FERRAUS_PLUGIN_LOG=1` echoes them live.
* The host exits after a `quit` request or when its stdin closes.
* No GUI is ever created.

## Control channel: framed JSON

Each message is one frame: a `u32` byte length (little-endian) followed by that many bytes
of UTF-8 JSON. The largest allowed frame is 64 MiB. ferraus writes requests to the host's
stdin. The host writes exactly one reply per request to stdout, in request order. ferraus
may pipeline `process` requests (see below); every other request waits for its reply.

### Requests (`"cmd"` field)

| cmd | fields | reply | meaning |
| --- | --- | --- | --- |
| `hello` | | `hello` | handshake: `host`, `version`, `protocol` (= 1), `formats` |
| `probe` | `path` | `probe` (`plugins: [Descriptor]`) | list the plugins in a bundle without instantiating any (used by `plugin scan`) |
| `load` | `path`, `plugin_id`? | `loaded` (`info: PluginInfo`) | load the bundle and instantiate and initialize one plugin. `plugin_id` may be omitted when the bundle holds exactly one plugin |
| `set_params` | `params: [{name, value}]` | `params` | set parameters. `name` is a stable id (`12`, `id:12`) or a display name (case- and punctuation-insensitive; `module/name` accepted). `value` is display text (`2.5kHz`, `-6dB`, `30%`), a plain in-range number, a step label, or `norm:0.3` |
| `get_params` | | `params` (`[{id, name, value, text}]`) | current values in plain units, plus the plugin's display text |
| `list_presets` | | `presets` (`[PresetInfo]`) | CLAP: the preset-discovery factory. VST3: the program list behind the program-change parameter |
| `load_preset` | `preset` | `params` | load a preset by name, load key, or file path (CLAP preset file, VST3 `.vstpreset`) |
| `get_state` | | `state` (`state_b64`) | the plugin's native state, base64 |
| `set_state` | `state_b64` | `ok` | restore a state saved earlier |
| `activate` | `sample_rate`, `block_size`, `shm_path` | `activated` (`latency`, `tail`, `channels_in`, `channels_out`) | map the shared-memory file, activate, and start processing. `tail` is in samples (`null` = not reported, `4294967295` = infinite) |
| `process` | `slot`, `frames`, `events`? | `processed` (`slot`) | process `frames` (at most `block_size`) from ring slot `slot` |
| `deactivate` | | `ok` | stop processing and deactivate; the plugin stays loaded |
| `quit` | | `ok` | tear down; the host exits |

On failure, a request gets `{"reply": "error", "code": ..., "message": ...}` instead.
Codes:

* `load_failed`, `activate_failed`, `process_failed`, `no_state`, `unsupported`,
  `preset_failed`: ferraus reports these as `ErrorKind::Plugin` (exit 7).
* `unknown_param`, `bad_value`, `unknown_preset`, `bad_state`, `bad_request`: ferraus
  reports these as `ErrorKind::Usage` (exit 2), with a hint such as
  `ferraus plugin info "<name>"`.

### Events

`events` is a list of objects tagged by `type`, each carrying `time` (sample offset within
the block) and sorted by time:

* `note_on` / `note_off`: `channel` (0–15), `key` (0–127), `velocity` (0–1).
* `midi`: `data` (3 raw MIDI bytes).

How each host delivers them:

* **CLAP**: CLAP note events when the plugin's first note port supports the CLAP dialect.
  Otherwise the notes go out as `CLAP_EVENT_MIDI`. Raw `midi` events always go out as
  `CLAP_EVENT_MIDI`.
* **VST3**: notes go out as `NoteOnEvent` / `NoteOffEvent`. Other raw MIDI messages are
  not delivered yet: they would need `IMidiMapping`.

The client slices one absolute-frame event list into blocks, so events stay
sample-accurate.

### Descriptor and PluginInfo

`Descriptor` fields:

* `id`, `name`, `vendor`, `version`, `description`, `url`.
* `features`: CLAP feature strings or VST3 sub-categories, lower-cased.
* `kind`: one of `instrument`, `audio-effect`, `note-effect`, `analyzer`, `other`.

`PluginInfo` fields:

* `descriptor` and `format` (`clap` or `vst3`).
* `params`: each with `id`, `name`, `module`, `min`, `max`, `default`, `value` (plain
  units), `default_text`, `value_text`, `unit` (inferred from the display text or the VST3
  units string), and the flags `stepped`, `hidden`, `readonly`, `bypass`, `automatable`.
* `audio_inputs`, `audio_outputs`: `{name, channels, main}`.
* `note_inputs`, `note_outputs`: `{name, dialects}`.
* `main_in_channels`, `main_out_channels`.
* `has_state`, `has_presets`.

## Audio channel: shared-memory ring

Before `activate`, ferraus creates a temp file
(`<tmp>/ferraus-shm-<pid>-<n>-<nanos>.bin`), sizes it, writes the header, and maps it. The
host maps the same file. ferraus deletes the file when the host is shut down. A file-backed
mapping is used because it behaves the same on Linux, macOS, and Windows.

Header (64 bytes, little-endian):

| offset | size | field |
| --- | --- | --- |
| 0 | 8 | magic `FRAUSHM1` |
| 8 | 4 | protocol version (1) |
| 12 | 4 | `channels_in` (the plugin's main input port; 0 for most instruments) |
| 16 | 4 | `channels_out` (the main output port) |
| 20 | 4 | `block_size` (frames) |
| 24 | 4 | `slots` |
| 28 | 36 | reserved (zero) |

Slot `s` starts at byte `64 + s * slot_bytes`, where
`slot_bytes = (channels_in + channels_out) * block_size * 4`. A slot holds the planar
`f32` input channels (each `block_size` frames), then the planar `f32` output channels.
Every offset is a multiple of 4, so the data is aligned.

The host copies the main-port channels between the ring and the plugin's own port buffers.
Sidechain inputs get silence, and extra outputs are discarded. ferraus adapts the channel
count of the input audio to `channels_in` (mono is duplicated, extra channels are
downmixed) before writing it.

### Flow and pipelining

1. ferraus writes block `k` into slot `k mod slots` and sends `process{slot, frames, events}`.
2. It may send up to `slots` requests before reading a reply (default 4 in flight).
3. Replies come back in order. On `processed{slot}`, ferraus copies that slot's output
   region, and the slot becomes free.

The pipe write that follows a slot write, and the pipe read that precedes reading it, order
the memory accesses. Both sides also issue a sequentially consistent fence.

### Latency and tail

After `activate`, ferraus reads `latency` and runs `input + tail + latency` frames. It then
drops the first `latency` output frames, so every sample stays at its original position.
`tail` comes from `--tail`. With `auto`, it is the plugin's reported tail (capped at 30 s).
When an effect reports no tail or an infinite one, it gets 2 s; an instrument that reports
no tail gets none.

## Watchdog and failures

Every wait for a reply is bounded by the watchdog: 60 s by default, set with `--watchdog`
or `FERRAUS_PLUGIN_WATCHDOG_S`. Each reply counts as progress.

* **Timeout**: ferraus kills the host and returns `ErrorKind::Plugin`, code
  `plugin_timeout`, exit 7.
* **Crash** (EOF on the pipe, or a broken pipe): ferraus reaps the host and returns
  `ErrorKind::Plugin`, code `plugin_crashed`, exit 7.

Both errors name the plugin and its bundle path, describe how it died (for example
"killed by signal 6 (SIGABRT)"), and quote the last lines the host wrote to stderr. The
ferraus process itself carries on. Scans probe every bundle in its own host, so a bundle
that crashes is recorded in the scan cache with its error and skipped.

## State formats

* **CLAP**: the bytes from `clap_plugin_state.save`.
* **VST3**: a container holding the component and controller states together: `FV3S`, then
  `u32` version 1, then `u32` length and the component state, then `u32` length and the
  controller state. A blob without the magic is treated as raw component state.

`plugin state save` wraps either kind as JSON:

```json
{"format": "ferraus-plugin-state", "version": 1,
 "plugin": {"name": "...", "id": "...", "format": "clap", "path": "..."},
 "params": [...], "state_b64": "..."}
```

`--state` accepts that JSON, a ferraus sidecar (`plugins[0].state_b64`), or a raw blob.

## Versioning

`hello` returns `protocol`. ferraus refuses a host with a different version and hints to
reinstall. Any incompatible change to messages or to the shared-memory layout bumps
`PROTOCOL_VERSION`, and the shared-memory magic carries the version too.
