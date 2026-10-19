# ADR 0003: MCP protocol implemented directly

Status: accepted (2026-10-19)

## Context
The PRD names `rmcp` (the official Rust SDK). Its API has changed across releases, and ferraus
needs only the server side: initialize, tools, resources, prompts, progress notifications,
cancellation, stdio, and streamable HTTP.

## Decision
`ferraus-mcp` implements JSON-RPC 2.0 and MCP revision 2025-06-18 (negotiating down to
2025-03-26 and 2024-11-05) on `serde_json`, with `tiny_http` for the HTTP transport. Tool
definitions, input schemas, and output schemas are generated from op specs.

## Consequences
* No SDK churn; the protocol surface is covered by unit tests (initialize, tools/list,
  one-to-one CLI↔tool mapping, sandboxed tools/call, jobs, resources, prompts, errors).
* The HTTP transport answers with `application/json` (allowed by the spec) and does not open a
  server-initiated SSE stream; progress notifications are delivered on stdio. Revisit if a
  client needs SSE progress over HTTP.
* Switching to `rmcp` later only touches `server.rs`.
