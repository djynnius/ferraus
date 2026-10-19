# Using ferraus from AI agents (MCP)

ferraus runs as a Model Context Protocol server. Every command is a tool named
`ferraus_<command>` (spaces and hyphens become underscores: `ferraus_denoise_profile`,
`ferraus_enhance_voice`), with the same argument schema as `ferraus schema <command>` and the
JSON envelope as its result (in `structuredContent`, and as text in `content`). ferraus 0.1.0
lists 175 tools. A complete session is in the
[MCP setup and tool-call recipe](../cookbook/mcp-tool-calls.md).

## Claude Code

```bash
claude mcp add ferraus -- ferraus mcp serve --allow ~/music
```

## Other clients (stdio)

```json
{"mcpServers": {"ferraus": {"command": "ferraus", "args": ["mcp", "serve", "--allow", "/Users/me/music"]}}}
```

## HTTP

```bash
ferraus mcp serve --http 127.0.0.1:8765 --allow ~/music
```

Clients POST JSON-RPC to `http://127.0.0.1:8765/mcp`. Requests with a browser `Origin` other
than localhost are refused.

## Sandbox

Every read and write must be inside an `--allow` directory (default: the current directory).
Relative paths are resolved against the first allowed directory, and when a tool call names no
output, results go to `ferraus-out/` there.

## Tool arguments

```json
{"inputs": ["takes/vocal.wav"], "key": "auto", "strength": "85%", "output": "vocal.tuned.wav"}
```

Global options allowed inside arguments: `overwrite`, `dry_run`, `force`, `bpm`, `sample_rate`,
`bit_depth`, `channels`, `bitrate`, `format`, `outdir`, `seed`, `deterministic`.

## Long jobs

`ferraus_job_start {"tool": "ferraus_separate", "arguments": {...}}` returns a `job_id`;
poll `ferraus_job_status`, fetch `ferraus_job_result`, or `ferraus_job_cancel`. Calls that
carry a progress token also receive `notifications/progress`.

## Resources and prompts

Resources: `ferraus://catalog`, `ferraus://presets`, `ferraus://models`, `ferraus://plugins`,
`ferraus://sounds`, and templates `ferraus://presets/{effect}`, `ferraus://jobs/{id}`,
`ferraus://docs/{command}`, `ferraus://schema/{command}`.

Prompts: `clean-and-tune-vocal`, `extract-stems-for-remix`, `hum-to-instrument`, `master-song`.
