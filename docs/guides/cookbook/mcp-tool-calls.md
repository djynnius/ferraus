# MCP setup and a tool call

`ferraus mcp serve` exposes every command as a Model Context Protocol tool, so an AI client
(Claude Code, Claude Desktop, Cursor, or any MCP client) can edit audio without a shell. This
page sets it up and shows a complete session as it ran against ferraus 0.1.0. Background:
[MCP concepts](../concepts/mcp.md).

## Set it up

Claude Code:

```bash
claude mcp add ferraus -- ferraus mcp serve --allow ~/music
```

Any client that reads an `mcpServers` config (Claude Desktop's
`claude_desktop_config.json`, Cursor's `mcp.json`, ...):

```json
{"mcpServers": {"ferraus": {"command": "ferraus", "args": ["mcp", "serve", "--allow", "/Users/me/music"]}}}
```

`--allow` is the sandbox: every input and output must be inside an allowed directory (repeat
the flag for more; the default is the current directory). Use an absolute path in JSON
configs, since `~` is not expanded there.

## A session

The server speaks JSON-RPC 2.0 over stdio, one message per line. This is the exchange a
client performs, trimmed:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"docs-test","version":"0"}}}
```

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18",
 "capabilities":{"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false},"prompts":{"listChanged":false},"logging":{}},
 "serverInfo":{"name":"ferraus","title":"ferraus audio toolkit","version":"0.1.0"},
 "instructions":"ferraus edits, repairs, separates, tunes, transcribes, and masters audio. Every tool returns the ferraus JSON envelope ..."}}
```

`tools/list` returns 175 tools: one per command (`ferraus_<command>`, spaces become
underscores) plus `ferraus_job_start`, `ferraus_job_status`, `ferraus_job_result`, and
`ferraus_job_cancel`. Each tool's `inputSchema` is the command's argument schema
(`ferraus schema <command>`):

```json
{"name": "ferraus_fx", "title": "ferraus fx",
 "description": "Apply a chain of effects in order in one pass\n\n...\n\nCLI equivalent: ferraus fx take.wav -o clean.wav --fx \"denoise:auto\" ...",
 "inputSchema": {"type": "object", "required": ["inputs"], "additionalProperties": false,
                 "properties": {"inputs": {"type": "array", "items": {"type": "string"}, "minItems": 1, ...},
                                "output": {"type": "string", ...}, "fx": {...}, ...}}}
```

A tool call: arguments use the flag names with `_` for `-`, inputs go in `inputs`, the output
path in `output`.

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"ferraus_normalize",
 "arguments":{"inputs":["episodes/ep02.wav"],"lufs":"-16","output":"ep02.mcp.wav"}}}
```

```json
{"jsonrpc":"2.0","id":4,"result":{
 "content":[{"type":"text","text":"{\"ok\":true,\"command\":\"normalize\", ...}"}],
 "structuredContent":{"ok":true,"command":"normalize",
   "inputs":[{"path":"/Users/me/music/episodes/ep02.wav","duration_s":13.5,"sample_rate":48000,"channels":1}],
   "outputs":[{"path":"/Users/me/music/ep02.mcp.wav","duration_s":13.5}],
   "params":{"lufs":"-16","ceiling":"-1dBTP","true_peak_limit":"on","mode":"lufs","target_lufs":-16.0, ...},
   "metrics":{"before":{"integrated_lufs":-22.43,"true_peak_dbtp":-7.88},"gain_db":6.43,
              "after":{"integrated_lufs":-16.0,"true_peak_dbtp":-1.45}}}}}
```

`structuredContent` is the same envelope `--json` prints on the command line; `content[0].text`
carries it as text for clients that do not read structured content. Relative paths resolve
against the first `--allow` directory. When a call names no `output`, results go to
`ferraus-out/` inside it.

List values can be JSON arrays: `{"inputs": ["vox.wav", "beat.wav"], "gains": ["0dB", "-6dB"]}`
is the same as `--gains 0dB,-6dB`.

A path outside the sandbox is refused, and the error is a normal envelope with
`"isError": true`:

```json
{"jsonrpc":"2.0","id":5,"result":{"isError":true,"structuredContent":{"ok":false,"command":"gain",
 "error":{"code":"path_not_allowed","message":"/etc/hosts is outside the allowed directories",
          "hint":"allowed: /Users/me/music","exit_code":2}}, ...}}
```

## Long jobs

Separation, mastering, and batches can take minutes. Start them as jobs:

```json
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"ferraus_job_start",
 "arguments":{"tool":"ferraus_separate","arguments":{"inputs":["song.wav"],"tier":"fast"}}}}
```

The result's `structuredContent` is `{"job_id": "job-1", "state": "running"}`; poll
`ferraus_job_status` with `{"job_id": "job-1"}`, then fetch `ferraus_job_result`, or stop it
with `ferraus_job_cancel`. Jobs live as long as the server process. Calls that carry an MCP progress token also receive
`notifications/progress`.

## Resources and prompts

`resources/list` returns `ferraus://catalog`, `ferraus://presets`, `ferraus://models`,
`ferraus://plugins`, and `ferraus://sounds`; the templates `ferraus://docs/{command}`,
`ferraus://schema/{command}`, `ferraus://presets/{effect}`, and `ferraus://jobs/{id}` read one
item. `prompts/list` returns `clean-and-tune-vocal`, `extract-stems-for-remix`,
`hum-to-instrument`, and `master-song`, which walk a model through the recipes in this
cookbook.

## HTTP instead of stdio

```bash
ferraus mcp serve --http 127.0.0.1:8765 --allow ~/music
```

Clients POST JSON-RPC to `http://127.0.0.1:8765/mcp`. Requests with a browser `Origin` other
than localhost are refused.
