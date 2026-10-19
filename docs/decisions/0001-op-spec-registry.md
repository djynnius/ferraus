# ADR 0001: One op-spec registry instead of clap derive

Status: accepted (2026-10-19)

## Context
The PRD requires that CLI help, JSON Schemas, MCP tools, fx stages, pipeline stages, and docs
are generated from one definition so they cannot drift. It suggests clap derive + schemars.

## Decision
Every command is an `OpSpec` value (`ferraus-core/src/op.rs`) with typed parameter specs whose
values are parsed by the shared unit parser. The clap command tree is generated at run time
from the registry (`ferraus-cli/src/cli.rs`) with clap's builder API; JSON Schemas
(`ferraus-core/src/schema.rs`), MCP tools, `describe --agent`, man pages, completions, and the
docs site are generated from the same specs. Feature crates contribute ops through `ops()`.

## Consequences
* Adding a command never touches the CLI crate; a crate's `ops()` is the only registration.
* Parameter values are strings until an op reads them with a typed accessor, so `-6dB`,
  `4b@120`, `C4`, and `1/8T` behave the same on the CLI, in fx strings, YAML, and MCP JSON.
* The "doc comment on each clap command is the only source of help text" rule becomes "the
  `help`/`about` strings on each OpSpec are the only source".
