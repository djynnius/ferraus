# ferraus agent guide

ferraus: audio editing CLI. Grammar: ferraus <command> [inputs] [flags] [-o out]. Commands take one audio file unless marked (<midi>, <file>, x2, ...). Add --json for a structured result, --dry-run to plan. Units: 1.5s 4b -6dB 2kHz C4 4:1 80% 1/16. Effects chain: ferraus fx in.wav -o out.wav --fx "name:key=val".

## Inspect

- `analyze chords: Chord sequence with times and confidence (best-effort)`
- `analyze clipping: Count and locate clipped runs [--threshold --min-run]`
- `analyze key: Key and scale with confidence [--method]`
- `analyze loudness: EBU R128 loudness: integrated, short-term, momentary, range, true peak [--timeline]`
- `analyze pitch: Pitch curve as JSON or CSV [--tracker --hop --range]`
- `analyze problems: Diagnose clipping, DC, clicks, hum, hiss, phase, silence, and more [--min-severity]`
- `analyze silence: List silent regions with start, end, and floor level [--threshold --min]`
- `analyze similarity <audio>x2: Spectral, cepstral, and loudness similarity between two files`
- `analyze spectrum: Long-term average spectrum in fractional-octave bands or FFT bins [--resolution]`
- `analyze stats: Per-channel peak, true peak, RMS, DC, crest factor, dynamic range [--histogram]`
- `analyze stereo: Stereo image: correlation, width per band, mono compatibility`
- `analyze tempo: Tempo, beat times, downbeats, and time signature [--range]`
- `compare <audio>x2: Null test: align b to a and report RMS and peak difference and SI-SDR [--max-offset --no-align]`
- `info <file>: Probe a file: container, codec, duration, rate, channels, bit depth, bitrate, tags, art [--loudness --spectrum]`
- `spectrogram: Export a spectrogram as a PNG image or JSON matrix [--fft --hop --mel]`
- `tags <file>: Read and write metadata tags and cover art [--set --remove --art]`
- `waveform: Export a waveform image (PNG, SVG) or JSON peaks [--width --height --peaks-per-second]`

## Edit

- `channels: Mix down, up-mix, extract, swap, or mid/side-convert channels [--to --extract --swap]`
- `concat <audio...>: Join files end to end, optionally with crossfades or gaps [--crossfade --crossfade-curve --gap]`
- `convert: Decode and re-encode between formats [--vbr --dither]`
- `cut: Remove one or more time ranges and join the rest [--range --crossfade]`
- `envelope: Keyframed automation of gain, pan, or pitch [--points --curve --target]`
- `fade: Fade in and/or out [--in --out --curve]`
- `gain: Apply a fixed gain, overall or per channel [--by]`
- `generate noise: Generate white, pink, or brown noise [--color --dur --amplitude]`
- `generate silence: Generate digital silence [--dur]`
- `generate tone: Generate a test tone or sweep [--freq --dur --shape]`
- `invert-phase: Invert polarity [--channel]`
- `mix <audio...>: Sum several files with per-input gain, pan, and offset [--gains --pans --offsets]`
- `normalize: Set loudness (LUFS), peak, or RMS level to a target [--lufs --peak --rms]`
- `resample: Change the sample rate with a high-quality sinc resampler [--to --quality]`
- `reverse: Play the audio backwards`
- `silence detect: Find silent regions and report their start, end, and level [--threshold --min --markers]`
- `silence pad: Add silence to the start and/or end [--start --end]`
- `silence trim: Remove leading and trailing silence, optionally long pauses too [--edges --all --threshold]`
- `speed: Change speed and pitch together (varispeed) [--factor]`
- `split: Split a file into parts at times, at silences, or into equal chunks [--at --every --on-silence]`
- `trim: Keep one time range of the input [--range --pad --pad-start]`

## Dynamics

- `clip: Soft or hard clipper [--ceiling --mode --knee]`
- `compress: Compress dynamic range (feed-forward, soft knee) [--threshold --ratio --attack]`
- `deess: Reduce sibilance [--center --bandwidth --threshold]`
- `dynamics: Transfer-curve dynamics: compressor, expander, and gate from one curve [--curve --spline --attack]`
- `expand: Downward expander [--threshold --ratio --attack]`
- `gate: Noise gate with hysteresis and hold [--threshold --attack --hold]`
- `limit: True-peak brickwall limiter [--ceiling --release --lookahead]`
- `multiband: Multiband compressor [--crossovers --bands --threshold]`
- `transient: Shape attack and sustain [--attack --sustain]`

## Tone

- `band-split: Split audio into frequency bands that sum back to the input [--crossovers]`
- `bitcrush: Bit-depth and sample-rate reduction [--bits --rate]`
- `chorus: Chorus (or chorus/flanger blend) [--rate --depth --voices]`
- `distort: Distortion from drawn transfer curves [--curve-pos --curve-neg --smooth]`
- `eq: Parametric EQ (cascaded biquads or linear phase) [--band --linear-phase]`
- `fft-filter: Filter by a drawn magnitude curve [--curve --fft]`
- `filter: Scientific filters: Butterworth, Chebyshev, Bessel, elliptic [--design --type --freq]`
- `flanger: Flanger [--rate --depth --feedback]`
- `graphic-eq: Linear-phase graphic equalizer (10, 20, or 30 bands) [--bands --gains --accuracy]`
- `guitar: Guitar amp chain: compressor, tone filter, distortion, cabinet [--amp --cab --filter]`
- `highpass: High-pass filter [--freq --order --q]`
- `lowpass: Low-pass filter [--freq --order --q]`
- `match-eq: Match the long-term spectrum of a reference [--reference --amount --smoothing]`
- `notch: Narrow notch filter, optionally with harmonics [--freq --q --depth]`
- `phaser: Phaser [--stages --rate --depth]`
- `saturate: Saturation (tape, tube, soft, hard) [--drive --type --tone]`
- `tilt: Tilt EQ: one-knob brighter or darker [--pivot --gain]`
- `tremolo: Tremolo (amplitude modulation) [--rate --depth --shape]`
- `vibrato: Vibrato (pitch modulation) [--rate --depth]`
- `vocoder: Vocoder: make a carrier speak or sing with the input's voice [--carrier --note --follow]`

## Space and time

- `delay: Feedback delay (echo), tempo-synced [--time --feedback --mix]`
- `doppler: Doppler pitch and level trajectory of a moving source [--path --velocity --distance]`
- `echo: Multi-tap echo with per-repeat filtering [--taps --time --feedback]`
- `reverb: Add reverb (plate, room, hall, chamber, or an impulse response) [--type --size --decay]`

## Restoration

- `center: Extract or remove centre-panned material (vocals, dialogue) [--mode --pan --phase-tolerance]`
- `dc-remove: Remove DC offset [--freq]`
- `declick: Remove clicks, pops, and crackle [--sensitivity --complexity --max-width]`
- `declip: Repair clipped peaks by autoregressive interpolation [--threshold --max-gap --headroom]`
- `dehiss: Reduce broadband hiss [--floor --reduction --range]`
- `dehum: Remove mains hum (50/60 Hz) and its harmonics [--freq --harmonics --q]`
- `dereverb: Reduce room reverberation [--rt60 --amount --late]`
- `enhance: Essential-Sound-style one-knob chain for dialogue, music, SFX, or ambience [--loudness --repair --clarity]`
- `enhance-voice: Vocal enhancer: rumble filter, de-plosive, de-ess, compression, presence [--mode]`
- `leveler: Slow speech volume leveler [--target --amount --range]`
- `phase-align: Align the timing of channels (azimuth and mic-placement correction) [--mode --shift --window]`
- `remove-sound: Learn an unwanted sound and subtract it everywhere [--learn --model --save-model]`
- `repair: Spectral repair of a time-frequency region from its neighbours [--range --freq --method]`

## Panning

- `autopan: Cyclic automatic panning [--rate --depth --shape]`
- `channels mix: Channel mixer: per-output weights and phase invert [--out --invert]`
- `pan: Place a source in the stereo field (static or keyframed) [--position --law --mode]`
- `widen: Stereo widener (mid/side) with mono bass [--amount --mono-below --center-pan]`

## Clean

- `denoise: Remove steady background noise (hiss, hum, fan, room tone) or run a speech denoiser [--profile --noise-range --auto]`
- `denoise profile: Learn a noise profile (.noiseprof) from a noise-only region [--range --auto --from]`
- `denoise profile merge <file...>: Combine noise profiles from several takes into one [--method]`

## Separate

- `separate: Split a mix into stems (vocals, drums, bass, other, ...) [--stems --tier --only]`

## Pitch

- `pitch: Shift pitch, keep duration [--semitones --cents --formant]`
- `pitch-bend: Pitch envelope: tape-style varispeed or duration-preserving bend [--points --curve --preserve-duration]`
- `stretch: Change duration, keep pitch [--factor --to --bpm]`
- `tune: Pitch correction (auto-tune) to a key, scale or written melody [--key --scale --strength]`

## MIDI

- `hum: Turn a hummed idea into an instrument part [--to --grid --strength]`
- `midi from-json <file>: Write a MIDI file from JSON [--ppq]`
- `midi groove extract <midi>: Capture a groove from a reference part [--grid]`
- `midi info <midi>: Summarize a MIDI file`
- `midi legato <midi>: Extend notes to the next onset [--gap]`
- `midi merge <midi...>: Combine MIDI files [--mode]`
- `midi quantize <midi>: Snap notes to a grid [--grid --strength --swing]`
- `midi split-tracks <midi>: One file per track or channel [--by]`
- `midi tempo <midi>: Set or scale the tempo [--set --scale]`
- `midi to-json <midi>: Convert MIDI to editable JSON [--compact]`
- `midi transpose <midi>: Shift note pitches [--semitones --include-drums]`
- `midi trim <midi>: Keep a time range [--range --shift]`
- `midi velocity <midi>: Scale, compress, or set velocities [--scale --compress --set]`
- `render <midi>: Render MIDI to audio with an instrument [--instrument --map --swap]`
- `transcribe: Turn singing, humming, or an instrument into MIDI [--mode --tracker --min-note]`

## Scores

- `score <file>: Notation from a recording or a MIDI file [--layout --chords --solfa]`
- `score check <file>: Validate a score: measure durations, voices, unsupported constructs [--strict]`
- `score convert <file...>: Convert between notation formats [--pretty --solfa]`
- `score from-json <file>: Score JSON back to notation [--pretty]`
- `score perform <file>: One shot: read, play, render [--tempo --repeats --dynamics]`
- `score play <file>: Notation to a MIDI performance [--tempo --repeats --dynamics]`
- `score read <file>: Optical music recognition: sheet music to MusicXML [--pages --dpi --engine]`
- `score to-json <file>: Score as the agent-facing JSON model [--pretty]`

## Chains

- `fx: Apply a chain of effects in order in one pass [--fx]`
- `pipeline explain <file>: Print the resolved plan with every default filled in [--set]`
- `pipeline run <file>: Run a pipeline file (YAML or JSON) with stage caching [--set --no-cache]`
- `pipeline validate <file>: Check a pipeline file without running it [--set]`
- `presets list [effect]: List built-in and user presets`
- `presets save [name]: Save parameter values as a user preset [--set]`
- `presets show [name]: Show a preset's values, e.g. compress/tighten`

## Plugins and sounds

- `plugin info [name]: Show a plugin's parameters, presets, ports, latency, and MIDI support`
- `plugin list: List cached plugins [--instruments --effects --format]`
- `plugin presets list [name]: List a plugin's presets`
- `plugin run: Run an effect plugin over audio [--plugin --param --preset]`
- `plugin scan: Find CLAP and VST3 plugins and cache their metadata [--paths --format --rescan]`
- `plugin state load [name]: Load a saved state into a plugin and show the resulting parameters [--state]`
- `plugin state save [name]: Save a plugin's native state as base64 JSON [--param --preset --state]`
- `sounds alias [assignment]: Set what an instrument name resolves to [--remove]`
- `sounds info [id]: Show a registry entry's license, size, and downloads`
- `sounds install [id]: Download, verify, and install a sound [--from --name]`
- `sounds list: List installed sounds, or what a folder contains [--path --category]`
- `sounds remove [id]: Remove an installed sound`
- `sounds scan [dir]: Index a folder of sounds [--watch --interval]`
- `sounds search [query]: Search the sounds registry [--type --license]`
- `sounds update: Fetch the latest signed sounds registry`

## Mastering

- `master: Automatic master of a finished mix to a delivery target [--target --lufs --true-peak]`
- `master analyze: The measurement report an agent decides from [--target --lufs --true-peak]`
- `master deliver: Write the deliverable set [--formats --tags --isrc]`
- `master plan: The exact chain master would apply, every parameter resolved, editable [--target --lufs --true-peak]`
- `master verify: Pass or fail against a delivery spec [--target --lufs --true-peak]`

## Media bridge

- `ffmpeg [args]: Run the managed FFmpeg with any arguments (pass-through)`
- `ffprobe [args]: Run the managed ffprobe with any arguments (pass-through)`
- `media extract-audio <file>: Pull an audio track out of a video or multi-track file [--track --copy]`
- `media info <file>: Container and stream details as JSON (ffprobe -show_format -show_streams)`
- `media remux <file>: Change the container without re-encoding (ffmpeg -c copy)`
- `media replace-audio <file>x2: Write a copy of a video with a new audio track [--codec --keep-original --shortest]`
- `media streams <file>: List every stream in a container: video, audio, subtitle, data`

## Models and system

- `accelerate install: Install GPU execution-provider libraries for ONNX Runtime [--provider --accept-eula]`
- `accelerate status: Show available execution providers and installed GPU libraries`
- `models info [name]: Spec, license, source, benchmarks, and cache state of a model`
- `models list: List registry models: version, task, size, license, cached, and which tiers use them [--task --cached]`
- `models path [name]: Print the cached file path of a model`
- `models prune: Remove cached models no tier uses, stale partial downloads, and enforce the cache budget`
- `models pull [name]: Download models with resume and SHA-256 verification [--all-for --tier --accept-license]`
- `models registry update: Refresh the signed registry manifest, or pin a snapshot [--pin --unpin --url]`
- `models rm [name]: Remove a cached model`
- `models verify [name]: Re-hash the model cache`

## Agents

- `describe: Compact catalog of every command for agents [--agent --format]`
- `jobs cancel [id]: Cancel a running background job`
- `jobs list: List background jobs started with --async`
- `jobs logs [id]: Progress and log output of a background job`
- `jobs result [id]: The JSON result envelope of a finished job`
- `jobs status [id]: Status of a background job`
- `jobs wait [id]: Block until a background job finishes, then return its result [--poll]`
- `mcp serve: Run an MCP server exposing every command as a tool [--http --allow]`
- `replay <file>: Re-run the command recorded in a sidecar`
- `schema [command]: List commands, or print the JSON Schema of one command's arguments and result`
- `verify <file>: Check that a sidecar's inputs and outputs still match their hashes`

## System

- `bench: Measure real-time factors of every effect on this machine [--only --duration --save]`
- `completions [shell]: Print a shell completion script`
- `doctor: Report what is installed: version, paths, FFmpeg, models, devices, plugins [--install-ffmpeg --install-audiveris]`
- `licenses: Print every third-party license and attribution`
- `self-uninstall: Remove everything the URL installer placed [--purge]`
- `self-update: Update ferraus to the latest release (opt-in, never automatic) [--version]`

## Docs

- `docs [command]: Full reference page for a command, offline`
- `docs build-site: Generate the mdBook docs site source from the command definitions [--out --guides --check]`
- `man: Print a man page (roff) for ferraus`
