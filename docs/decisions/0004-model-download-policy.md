# ADR 0004: Model downloads never prompt

Status: accepted (2026-10-19)

## Context
The PRD is inconsistent: design principle 3 says ferraus never prompts and never downloads
without `--yes` or a config opt-in, while the model policy talks about "skipping the prompt"
and the example config sets `auto_download = true`.

## Decision
ferraus never prompts. A missing model is downloaded only when `-y/--yes` is passed or the
config sets `models.auto_download = true`; otherwise the command fails fast with exit code 5
and a hint naming the exact `ferraus models pull <name>` command. `--offline` always wins.
The download prints its size and license on stderr (and in `--json` warnings) before
fetching. The shipped default config does not set `auto_download`, so the default is "no
download"; the PRD's example config is still valid and opts in.
