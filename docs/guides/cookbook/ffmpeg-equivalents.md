# FFmpeg equivalents

_Every ferraus command on this page was run against ferraus 0.1.0._

ferraus does common audio jobs natively, so an agent or person who already knows FFmpeg can
translate directly. Anything not listed here still works through the pass-through:
`ferraus ffmpeg -- <the same ffmpeg arguments>` (the exact command line is recorded in the
JSON envelope and sidecar). `ferraus ffmpeg -- -h` prints FFmpeg's own help.

Global flags map the same way everywhere: `-ar` → `--sample-rate`, `-ac` → `--channels`,
`-b:a` → `--bitrate`, `-sample_fmt s16` → `--bit-depth 16`, `-y` → `--overwrite`,
`-f wav -` (stdout) → `-o -`.

## Conversion and encoding

| ffmpeg | ferraus |
| --- | --- |
| `ffmpeg -i in.wav out.flac` | `ferraus convert in.wav -o out.flac` |
| `ffmpeg -i in.flac -c:a libmp3lame -b:a 256k out.mp3` | `ferraus convert in.flac -o out.mp3 --bitrate 256k` |
| `ffmpeg -i in.flac -c:a libmp3lame -q:a 2 out.mp3` | `ferraus convert in.flac -o out.mp3 --bitrate 190k` (VBR is the default; `--vbr off` for CBR) |
| `ffmpeg -i in.wav -c:a libopus -b:a 64k out.opus` | `ferraus convert in.wav -o out.opus --bitrate 64k` |
| `ffmpeg -i in.wav -c:a libvorbis -q:a 5 out.ogg` | `ferraus convert in.wav -o out.ogg --bitrate 160k` |
| `ffmpeg -i in.wav -c:a aac -b:a 192k out.m4a` | `ferraus convert in.wav -o out.m4a` (uses the FFmpeg bridge for AAC) |
| `ffmpeg -i in.wav -c:a alac out.m4a` | `ferraus convert in.wav -o out.m4a --format alac` (bridge) |
| `ffmpeg -i in.wav -sample_fmt s16 out.wav` | `ferraus convert in.wav -o out.wav --bit-depth 16` (TPDF dither; `--dither none` to truncate) |
| `ffmpeg -i in.wav -c:a pcm_f32le out.wav` | `ferraus convert in.wav -o out.wav --bit-depth 32` |
| `ffmpeg -i in.wav -ar 44100 out.wav` | `ferraus resample in.wav --to 44100 -o out.wav` or `ferraus convert in.wav -o out.wav --sample-rate 44100` |
| `ffmpeg -i in.wav -af aresample=resampler=soxr:precision=28 -ar 48000 out.wav` | `ferraus resample in.wav --to 48000 --quality high -o out.wav` |
| `ffmpeg -i in.wav -f wav - \| other` | `ferraus convert in.wav -o - \| other` |
| `cat in.wav \| ffmpeg -f wav -i - out.flac` | `cat in.wav \| ferraus convert - --input-format wav -o out.flac` |

## Inspecting

| ffmpeg | ferraus |
| --- | --- |
| `ffprobe in.mp3` | `ferraus info in.mp3` |
| `ffprobe -v error -show_format -show_streams -of json in.mkv` | `ferraus media info in.mkv --json` |
| `ffprobe -v error -show_entries stream=index,codec_type,codec_name:stream_tags=language in.mkv` | `ferraus media streams in.mkv` |
| `ffmpeg -i in.wav -af ebur128=peak=true -f null -` | `ferraus analyze loudness in.wav` (or `ferraus info in.wav --loudness`) |
| `ffmpeg -i in.wav -af volumedetect -f null -` | `ferraus analyze stats in.wav` (peak, true peak, RMS per channel) |
| `ffmpeg -i in.wav -af astats -f null -` | `ferraus analyze stats in.wav` (add `--histogram` for the level histogram) |
| `ffmpeg -i in.wav -lavfi showspectrumpic out.png` | `ferraus spectrogram in.wav -o out.png` |
| `ffmpeg -i in.wav -lavfi showwavespic out.png` | `ferraus waveform in.wav -o out.png` |
| `ffmpeg -i in.wav -af silencedetect=n=-40dB:d=0.3 -f null -` | `ferraus silence detect in.wav --threshold -40dB --min 300ms` |

## Editing

| ffmpeg | ferraus |
| --- | --- |
| `ffmpeg -ss 10 -to 40 -i in.wav out.wav` | `ferraus trim in.wav --range 10s..40s -o out.wav` |
| `ffmpeg -ss 1:00 -i in.wav out.wav` | `ferraus trim in.wav --range 1:00..end -o out.wav` |
| `ffmpeg -f concat -safe 0 -i list.txt -c copy out.wav` | `ferraus concat a.wav b.wav c.wav -o out.wav` |
| `ffmpeg -i a.wav -i b.wav -filter_complex acrossfade=d=2 out.wav` | `ferraus concat a.wav b.wav --crossfade 2s -o out.wav` |
| `ffmpeg -i in.wav -f segment -segment_time 30 out%03d.wav` | `ferraus split in.wav --every 30s -o 'out{stem}.wav'` (writes `out001.wav`, `out002.wav`, ...; `{stem}` is the part number) |
| `ffmpeg -i in.wav -af "afade=t=in:d=1,afade=t=out:st=58:d=2" out.wav` | `ferraus fade in.wav --in 1s --out 2s -o out.wav` |
| `ffmpeg -i in.wav -af volume=-6dB out.wav` | `ferraus gain in.wav --by -6dB -o out.wav` |
| `ffmpeg -i in.wav -af loudnorm=I=-14:TP=-1 out.wav` | `ferraus normalize in.wav --lufs -14 --ceiling -1dBTP -o out.wav` (`--peak` is a separate peak-normalize mode and cannot be combined with `--lufs`) |
| `ffmpeg -i in.wav -af dynaudnorm out.wav` | `ferraus leveler in.wav -o out.wav` (slow level riding; `normalize --rms -20dB` for one static gain) |
| `ffmpeg -i in.wav -af silenceremove=start_periods=1:stop_periods=1 out.wav` | `ferraus silence trim in.wav -o out.wav` (leading and trailing; `--all` also shortens inner pauses) |
| `ffmpeg -i in.wav -af areverse out.wav` | `ferraus reverse in.wav -o out.wav` |
| `ffmpeg -i in.wav -af atempo=1.25 out.wav` | `ferraus stretch in.wav --factor 0.8 -o out.wav` (`--factor` multiplies the duration: 1.25x faster is 0.8; or `--to-bpm` with `--bpm`) |
| `ffmpeg -i in.wav -af asetrate=48000*1.059463,aresample=48000 out.wav` | `ferraus speed in.wav --factor 1.059463 -o out.wav` (varispeed: pitch and tempo together, like the ffmpeg line) or `ferraus pitch in.wav --semitones 1 -o out.wav` (pitch only, duration kept) |

## Channels

| ffmpeg | ferraus |
| --- | --- |
| `ffmpeg -i in.wav -ac 1 out.wav` | `ferraus channels in.wav --to mono -o out.wav` (or `--channels 1` on any command) |
| `ffmpeg -i in.wav -ac 2 out.wav` | `ferraus channels in.wav --to stereo -o out.wav` |
| `ffmpeg -i in.wav -af "pan=mono\|c0=FL" left.wav` | `ferraus channels in.wav --extract L -o left.wav` |
| `ffmpeg -i in.wav -af channelmap=1\|0 out.wav` | `ferraus channels in.wav --swap -o out.wav` |
| `ffmpeg -i in.wav -filter_complex channelsplit out_L.wav out_R.wav` | `ferraus channels in.wav --extract L -o L.wav` and `--extract R -o R.wav` |
| `ffmpeg -i a.wav -i b.wav -filter_complex amix=inputs=2 out.wav` | `ferraus mix a.wav b.wav -o out.wav` |

## Filters

| ffmpeg | ferraus |
| --- | --- |
| `ffmpeg -i in.wav -af highpass=f=80 out.wav` | `ferraus highpass in.wav --freq 80Hz -o out.wav` (or `ferraus eq in.wav --band highpass:80Hz -o out.wav`) |
| `ffmpeg -i in.wav -af "equalizer=f=1000:t=q:w=1:g=3" out.wav` | `ferraus eq in.wav --band peak:1kHz:+3dB -o out.wav` |
| `ffmpeg -i in.wav -af acompressor=threshold=-18dB:ratio=4 out.wav` | `ferraus compress in.wav --threshold -18dB --ratio 4:1 -o out.wav` |
| `ffmpeg -i in.wav -af alimiter=limit=0.89 out.wav` | `ferraus limit in.wav --ceiling -1dBTP -o out.wav` |
| `ffmpeg -i in.wav -af afftdn out.wav` | `ferraus denoise in.wav -o out.wav` |
| `ffmpeg -i in.wav -af "aecho=0.8:0.9:60:0.4" out.wav` | `ferraus echo in.wav --taps 1 --time 60ms --feedback 40% -o out.wav` |
| `ffmpeg -i in.wav -af "highpass=f=80,loudnorm=I=-16" out.wav` | `ferraus fx in.wav --fx "eq:band=highpass:80Hz" --fx "normalize:lufs=-16" -o out.wav` |

## Video containers

| ffmpeg | ferraus |
| --- | --- |
| `ffmpeg -i video.mp4 -vn audio.wav` | `ferraus media extract-audio video.mp4 -o audio.wav` (decoded natively) |
| `ffmpeg -i video.mp4 -vn -c:a copy audio.m4a` | `ferraus media extract-audio video.mp4 --copy` |
| `ffmpeg -i movie.mkv -map 0:a:1 commentary.flac` | `ferraus media extract-audio movie.mkv --track 1 -o commentary.flac` |
| `ffmpeg -i video.mp4 -i new.wav -map 0:v -map 1:a -c:v copy -c:a aac out.mp4` | `ferraus media replace-audio video.mp4 new.wav -o out.mp4` |
| `ffmpeg -i video.mp4 -i new.wav -map 0:v -map 1:a -map 0:a -c:v copy out.mkv` | `ferraus media replace-audio video.mp4 new.wav --keep-original -o out.mkv` |
| `ffmpeg -i in.mkv -map 0 -c copy out.mp4` | `ferraus media remux in.mkv -o out.mp4` |
| `ffmpeg -i video.mp4 -af loudnorm=I=-16 -c:v copy out.mp4` | `ferraus media extract-audio video.mp4 -o a.wav && ferraus normalize a.wav --lufs -16 -o b.wav && ferraus media replace-audio video.mp4 b.wav -o out.mp4` |
| anything else (scaling, subtitles, thumbnails, overlays) | `ferraus ffmpeg -- <ffmpeg arguments>` (a native `ferraus video` namespace is reserved for v2) |

## Getting FFmpeg

The bridge looks for `FERRAUS_FFMPEG` (or `ffmpeg.path` in the config), then the managed
build in the ferraus cache, then `ffmpeg` on `PATH`. `ferraus doctor --install-ffmpeg`
downloads a pinned, SHA-256-verified static build (LGPL builds on Linux and Windows; the
macOS build is GPL) and shows its license; `ferraus doctor` reports which binary is in use.
