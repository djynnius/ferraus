# Global options

Every command accepts these. Command-specific options are listed on each command's page.

| Flag | Meaning | Default |
| --- | --- | --- |
| `-o, --output <path>` | Output path; `-` for stdout; templates `{name}` (input file stem), `{stem}` (output role, e.g. `vocals`), `{index}` (batch item), `{ext}`, `{command}` | `<input>.<command>.<ext>` beside the input |
| `--outdir <dir>` | Output directory, required for batch runs without a template | — |
| `--format <fmt>` | Output container when the extension is ambiguous or output is stdout | from the extension |
| `--input-format <fmt>` | Format of stdin | — |
| `--json` | Structured result on stdout, nothing else on stdout | off |
| `--progress auto\|none\|bar\|json` | Progress on stderr; `json` is one NDJSON event per line | `auto` |
| `-q`, `-v`, `-vv` | Log verbosity on stderr | warnings |
| `--threads <n>` | CPU threads | all cores |
| `--device auto\|cpu\|cuda\|coreml\|directml` | Inference device | `auto` |
| `--deterministic` | Bit-exact CPU results; fixes the thread count | off |
| `--seed <n>` | Seed for stochastic steps (dither, humanize, test-time augmentation) | 0 |
| `--offline` | Fail instead of using the network | off |
| `-y, --yes` | Allow model downloads | off |
| `--overwrite` | Replace an existing output | off |
| `--in-place` | Write back to the input, keeping `<input>.bak` | off |
| `--no-sidecar` | Skip the `<output>.ferraus.json` provenance file | sidecar on |
| `--dry-run` | Resolve parameters and print the plan without processing | off |
| `--explain` | One line per resolved parameter: what it does and why it was chosen | off |
| `--force` | Recompute even if an identical output already exists | off |
| `--sample-rate`, `--bit-depth`, `--channels`, `--bitrate` | Output encoding overrides | match input; lossy 192k |
| `--config <path>` | Alternate config file | platform default |
| `--sound-path <dir>` | Extra instrument/sound folder (repeatable; also `FERRAUS_SOUND_PATH`) | registry + standard plugin dirs |
| `--bpm <n>` | Tempo for beat-based values | detected when needed |
| `--async` | Run in the background and print a job id (see `ferraus jobs list` and `ferraus jobs wait`) | off |
| `--timeout <time>` | Cancel after this long (exit code 8) | none |
| `--max-memory <size>` | Memory budget before offline stages spill to disk | 2GB |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Processing failed |
| 2 | Usage or parameter error |
| 3 | Input missing or unreadable |
| 4 | Unsupported format or codec |
| 5 | Model not cached and downloads disabled |
| 6 | Model download or verification failed |
| 7 | Plugin crashed, timed out, or was not found |
| 8 | Cancelled (including `--timeout`) |
| 9 | Output verification failed |
| 10 | Network error |
| 70 | Internal error (a bug) |
