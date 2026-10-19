//! The ferraus command-line front end: builds the clap command tree from the op registry,
//! parses global flags and config, runs the invocation, and prints the result.

pub mod cli;
pub mod config;
pub mod docgen;
pub mod progress;
pub mod system;
pub mod tools;

use ferraus_core::Registry;

/// Every op from every crate. One place, so the CLI, MCP server, schemas, and docs agree.
pub fn registry() -> Registry {
    let mut r = Registry::new();
    r.extend(ferraus_core::builtin::ops());
    r.extend(ferraus_codec::ops());
    r.extend(ferraus_media::ops());
    r.extend(ferraus_edit::ops());
    r.extend(ferraus_dsp::ops());
    r.extend(ferraus_restore::ops());
    r.extend(ferraus_analysis::ops());
    r.extend(ferraus_master::ops());
    r.extend(ferraus_pitch::ops());
    r.extend(ferraus_ml::ops());
    r.extend(ferraus_models::ops());
    r.extend(ferraus_separate::ops());
    r.extend(ferraus_denoise::ops());
    r.extend(ferraus_midi::ops());
    r.extend(ferraus_synth::ops());
    r.extend(ferraus_plugins::ops());
    r.extend(ferraus_score::ops());
    r.extend(ferraus_mcp::ops());
    r.extend(system::ops());
    r.extend(tools::ops());
    r
}

/// Run the CLI with the given arguments (including argv[0]); returns the exit code.
pub fn main_with_args(argv: Vec<String>) -> i32 {
    cli::run(argv)
}
