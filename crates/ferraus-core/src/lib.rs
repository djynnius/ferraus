//! # ferraus-core
//!
//! Shared foundation for every ferraus crate:
//!
//! * [`AudioBuffer`] — planar `f32` audio, the internal format of every stage.
//! * [`units`] — the shared parser for `1.5s`, `-6dB`, `2kHz`, `C4`, `4:1`, `80%`, `1/16`.
//! * [`op`] — [`OpSpec`]: the single definition of a command from which the CLI, `fx`
//!   stages, pipelines, JSON Schemas, MCP tools, and docs are generated.
//! * [`exec`] — the executor shared by the CLI, pipelines, and the MCP server.
//! * [`envelope`] — the `--json` result envelope and provenance sidecars.
//! * [`pipeline`] — pipeline files with content-hash stage caching.

pub mod buffer;
pub mod builtin;
pub mod ctx;
pub mod envelope;
pub mod error;
pub mod exec;
pub mod op;
pub mod paths;
pub mod pipeline;
pub mod registry;
pub mod resample;
pub mod schema;
pub mod testing;
pub mod units;
pub mod wav;

pub use buffer::{db_to_lin, lin_to_db, AudioBuffer, MediaBuffer};
pub use ctx::{AudioIo, EncodeOptions, Globals, OpCtx, Progress, SourceInfo};
pub use envelope::{Envelope, Sidecar, FERRAUS_VERSION, SCHEMA_VERSION};
pub use error::{Error, ErrorKind, Result};
pub use op::{param, Args, Category, InputKind, Inputs, OpSpec, ParamKind, ParamSpec, Runner};
pub use registry::Registry;

/// Everything an op-defining crate usually needs.
pub mod prelude {
    pub use crate::buffer::{db_to_lin, lin_to_db, AudioBuffer};
    pub use crate::ctx::OpCtx;
    pub use crate::error::{Error, ErrorKind, Result};
    pub use crate::op::{param, Args, Category, InputKind, Inputs, OpSpec, ParamKind};
    pub use crate::units;
    pub use serde_json::{json, Value};
}
