# Glossary

| Term | Meaning |
| --- | --- |
| ADSR | Attack, decay, sustain, release: the four stages of a note's volume envelope. |
| Agent | A program, often driven by an AI model, that calls ferraus commands or MCP tools on its own. See [MCP](concepts/mcp.md). |
| Attack / release | How fast a compressor, gate, or limiter reacts when the level rises (attack) and recovers when it falls (release). |
| Bit depth | Bits per sample in integer audio (16, 24); ferraus processes in 32-bit float internally. |
| Bleed | Sound from one instrument that leaks into another's stem after separation (cymbals in the vocal stem). See [Stems](concepts/stems.md). |
| Chroma | Energy per pitch class (C, C#, ... B), used for key and chord detection. |
| CLAP, VST3, AU, LV2 | Audio plugin formats; CLAP and VST3 are hosted in v1. |
| Crest factor | Peak level minus RMS level in dB; high means punchy, low means dense or heavily limited. |
| dBFS | Decibels relative to digital full scale; 0 dBFS is the loudest sample value. |
| dBTP | Decibels true peak, measured after oversampling so peaks between samples are caught. |
| De-esser | A compressor that reacts only to sibilance ("s", "sh") around 5–9 kHz. |
| Dither | Low-level noise added when reducing bit depth so quantization error becomes benign hiss. |
| Envelope | A value (gain, pan, pitch) that changes over time, set by keyframes. |
| Execution provider | An ONNX Runtime backend (CPU, CUDA, CoreML, DirectML). |
| Formant | A resonance of the vocal tract; keeping formants fixed keeps a voice sounding like itself when its pitch changes. |
| fx stage | One effect inside an `fx` chain or pipeline, written `name:key=value`. |
| Gate | Silences a signal while it is below a threshold. |
| Groove | A template of timing and velocity offsets per grid position, extracted from a performance. |
| Hum | A low buzz at the mains frequency (50 or 60 Hz) and its harmonics, picked up from power lines. Removed by `dehum`. |
| Integrated loudness | The loudness of a whole program, gated to ignore silence, in LUFS. |
| Key | The tonal center and mode of a piece (G major, E minor). |
| Lead sheet | Melody on a treble staff with chord symbols above and lyrics below. |
| Leveler | A slow automatic gain that raises quiet passages and lowers loud ones, like riding a fader. See [Clean a podcast](tutorials/clean-a-podcast.md). |
| Limiter | A compressor with a very high ratio that keeps peaks under a ceiling. |
| LUFS, LU | Loudness units per EBU R128; LUFS is absolute, LU is a difference. |
| Mastering | Final processing of a mix for delivery: tonal balance, dynamics, stereo image, loudness, and format. |
| MCP | Model Context Protocol: the interface through which agents call ferraus as tools. |
| Mid/side | Stereo expressed as center (mid) and difference (side); the basis of widening and center extraction. |
| MIDI | A file of note and control events rather than audio. |
| Musical noise | Chirping, watery artefacts left by aggressive noise reduction. `denoise --artifact-control` reduces it. |
| MusicXML | The open interchange format for notation; ferraus's hub format between audio, MIDI, and engraved scores. |
| Noise profile | Per-frequency statistics of a noise-only region, used by spectral gating. |
| Null test | Subtracting two signals to check they are identical; a processed file at 0% mix must null against its input. |
| OMR | Optical music recognition: reading printed or scanned sheet music into a machine-readable score. |
| Onset | The moment a note starts. Transcription splits notes at onsets; `--onset-sensitivity` sets how readily. |
| Pan law | How gains are split between speakers as a source moves, so loudness stays even. See [pan laws](concepts/pan-laws.md). |
| Pipeline | A YAML or JSON file describing a graph of stages, run with `ferraus pipeline run`. |
| PPQ | Pulses (ticks) per quarter note: the time resolution of a MIDI file. |
| Quantize | Moving MIDI note timings onto a rhythmic grid. |
| Relative minor | The minor key that shares a major key's notes (A minor for C major); key detection often confuses the two. |
| Retune speed | How fast pitch correction pulls toward the target note; 0 ms is the robotic effect. |
| RMS | Root mean square: an average level that tracks perceived loudness better than peaks. |
| SDR | Signal-to-distortion ratio in dB; the standard separation quality metric (higher is better). |
| SF2, SFZ | Sample-based instrument formats (SoundFont 2 and the SFZ text format). |
| SI-SDR | Scale-invariant SDR, used to evaluate denoising and in `ferraus compare`. |
| Sidecar | The `<output>.ferraus.json` provenance file written beside each output. |
| Solfa | Tonic sol-fa notation (do re mi, written d r m f s l t) relative to the key; readable without a staff. |
| Sound folder | A folder of SoundFonts, SFZ libraries, impulse responses, or plugins passed with `--sound-path`. See [Sound folders](concepts/sound-folders.md). |
| Spectral gating | Attenuating time-frequency bins that fall below a noise threshold. |
| Stem | One instrument group separated from a mix (vocals, drums, bass, other, ...). |
| STFT | Short-time Fourier transform: the spectrum of short overlapping windows of audio. |
| TD-PSOLA | Time-domain pitch-synchronous overlap-add: pitch shifting that preserves formants. |
| Tempo map | The tempo (and changes) of a piece, used to convert beats to seconds. |
| Tier | A named set of separation models trading speed for quality (`fast`, `balanced`, `quality`, `max`). See [Stems](concepts/stems.md). |
| Transient | The sharp start of a sound (a drum hit, a pluck). |
| True peak | See dBTP. |
| TTA | Test-time augmentation: running a model on shifted copies and averaging, for better separation. |
| Varispeed | Changing speed and pitch together, like a tape played faster (`speed`); `stretch` and `pitch` change one without the other. |
