//! # ferraus-mcp
//!
//! A Model Context Protocol server (protocol revision 2025-06-18, backwards compatible with
//! 2025-03-26 and 2024-11-05) exposing every ferraus command as a tool. Tools, their input
//! schemas, and descriptions are generated from the same op specs as the CLI, so they
//! cannot drift. Transports: stdio (newline-delimited JSON-RPC) and streamable HTTP
//! (`POST /mcp`).
//!
//! The protocol layer is implemented directly on `serde_json` rather than through an SDK:
//! it is small, stable, and this keeps the server dependency-light (see
//! `docs/decisions/0003-mcp-protocol-implementation.md`).

pub mod server;

use ferraus_core::prelude::*;

pub use server::{Server, ServerConfig};

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::custom("mcp serve", Category::Agents, "Run an MCP server exposing every command as a tool", serve)
        .about("Starts a Model Context Protocol server. On stdio (the default) it is what Claude Code and other agents launch as a subprocess; with --http it serves the streamable-HTTP transport for several clients. Every read and write is confined to the --allow directories (default: the current directory) and outputs default to a ferraus-out/ folder under the first allowed directory.")
        .inputs(Inputs::NONE)
        .param(param("http", ParamKind::Str).help("Serve streamable HTTP on this address (e.g. 127.0.0.1:8765) instead of stdio"))
        .param(param("allow", ParamKind::Path).repeatable().help("Directory the server may read and write (repeatable); default: current directory"))
        .example("claude mcp add ferraus -- ferraus mcp serve --allow ~/music", "Register ferraus with Claude Code")
        .example("ferraus mcp serve --http 127.0.0.1:8765 --allow ~/music", "Serve several clients over HTTP")]
}

fn serve(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let mut allow: Vec<std::path::PathBuf> = a.all("allow").iter().map(std::path::PathBuf::from).collect();
    if allow.is_empty() {
        allow = ctx.globals.allow.clone();
    }
    if allow.is_empty() {
        allow.push(std::env::current_dir()?);
    }
    let allow: Vec<_> = allow.iter().map(|p| ferraus_core::ctx::absolutize(p)).collect();
    let mut base = ctx.globals.clone();
    base.allow = allow.clone();
    let cfg = ServerConfig { allow, base_globals: base };
    let server = Server::new(registry, ctx.io, cfg);
    match a.str("http") {
        Some(addr) => server.serve_http(&addr)?,
        None => server.serve_stdio()?,
    }
    ctx.globals.sidecar = false;
    Ok(Value::Null)
}
