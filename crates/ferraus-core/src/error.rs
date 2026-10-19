//! Error type shared by every ferraus crate, with stable exit codes.

use serde::Serialize;
use std::fmt;

/// Error categories. The discriminant is the process exit code (see the PRD exit-code table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Processing failed.
    Processing,
    /// Usage or parameter error.
    Usage,
    /// Input missing or unreadable.
    InputMissing,
    /// Unsupported format or codec.
    Unsupported,
    /// Model not cached and downloads disabled.
    ModelNotCached,
    /// Model download or verification failed.
    ModelDownload,
    /// Plugin crashed, timed out, or was not found.
    Plugin,
    /// Cancelled (including timeouts and memory limits).
    Cancelled,
    /// Output verification failed.
    Verification,
    /// Network error.
    Network,
    /// Internal error (bug).
    Internal,
}

impl ErrorKind {
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::Processing => 1,
            ErrorKind::Usage => 2,
            ErrorKind::InputMissing => 3,
            ErrorKind::Unsupported => 4,
            ErrorKind::ModelNotCached => 5,
            ErrorKind::ModelDownload => 6,
            ErrorKind::Plugin => 7,
            ErrorKind::Cancelled => 8,
            ErrorKind::Verification => 9,
            ErrorKind::Network => 10,
            ErrorKind::Internal => 70,
        }
    }

    pub fn all() -> &'static [ErrorKind] {
        &[
            ErrorKind::Processing,
            ErrorKind::Usage,
            ErrorKind::InputMissing,
            ErrorKind::Unsupported,
            ErrorKind::ModelNotCached,
            ErrorKind::ModelDownload,
            ErrorKind::Plugin,
            ErrorKind::Cancelled,
            ErrorKind::Verification,
            ErrorKind::Network,
            ErrorKind::Internal,
        ]
    }

    pub fn describe(self) -> &'static str {
        match self {
            ErrorKind::Processing => "Processing failed",
            ErrorKind::Usage => "Usage or parameter error",
            ErrorKind::InputMissing => "Input missing or unreadable",
            ErrorKind::Unsupported => "Unsupported format or codec",
            ErrorKind::ModelNotCached => "Model not cached and downloads disabled",
            ErrorKind::ModelDownload => "Model download or verification failed",
            ErrorKind::Plugin => "Plugin crashed, timed out, or was not found",
            ErrorKind::Cancelled => "Cancelled",
            ErrorKind::Verification => {
                "Output verification failed (checksum, clipping guard, duration mismatch)"
            }
            ErrorKind::Network => "Network error",
            ErrorKind::Internal => "Internal error (bug)",
        }
    }
}

/// The error carried through every command. `code` is a stable snake_case identifier,
/// `hint` is a runnable command when one exists.
#[derive(Debug, Clone, Serialize)]
pub struct Error {
    #[serde(skip)]
    pub kind: ErrorKind,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub exit_code: i32,
}

impl Error {
    pub fn new(kind: ErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Error {
            kind,
            code: code.into(),
            message: message.into(),
            hint: None,
            exit_code: kind.exit_code(),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Usage, "invalid_argument", message)
    }

    pub fn processing(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Processing, "processing_failed", message)
    }

    pub fn input(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::InputMissing, "input_unreadable", message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Unsupported, "unsupported_format", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Internal, "internal_error", message).with_hint(
            "Please report this at https://github.com/djynnius/ferraus/issues with the command and its sidecar",
        )
    }

    pub fn verification(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Verification, "verification_failed", message)
    }

    pub fn plugin(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Plugin, "plugin_failed", message)
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Cancelled, "cancelled", message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " (hint: {h})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => {
                Error::new(ErrorKind::InputMissing, "not_found", e.to_string())
            }
            std::io::ErrorKind::PermissionDenied => {
                Error::new(ErrorKind::InputMissing, "permission_denied", e.to_string())
            }
            _ => Error::new(ErrorKind::Processing, "io_error", e.to_string()),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::usage(format!("invalid JSON: {e}"))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
