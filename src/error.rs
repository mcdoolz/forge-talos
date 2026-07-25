//! Error types for the Talos crate.
//!
//! [`TalosError`] covers every failure mode that can occur when invoking
//! `agy`, parsing its output, or managing concurrency limits.

/// Enumerates all errors that can occur within the Talos crate.
#[derive(Debug, thiserror::Error)]
pub enum TalosError {
    /// The `agy` binary was not found on `$PATH` or at the configured location.
    #[error("agy binary not found — ensure it is installed and on $PATH")]
    AgyNotFound,

    /// The `agy` process exceeded the configured timeout.
    #[error("agy invocation timed out after the configured duration")]
    Timeout,

    /// The `agy` process exited with a non-zero status code.
    #[error("agy process failed (exit code {exit_code}): {stderr}")]
    ProcessFailed {
        /// The non-zero exit code returned by the process.
        exit_code: i32,
        /// Captured stderr output from the process.
        stderr: String,
    },

    /// The expected transcript file was not found on disk.
    #[error("transcript not found at: {path}")]
    TranscriptNotFound {
        /// The filesystem path where the transcript was expected.
        path: String,
    },

    /// Failed to parse transcript JSONL or other structured output.
    #[error("parse error: {0}")]
    ParseError(String),

    /// The concurrency semaphore has no available permits.
    #[error("concurrency limit reached — too many concurrent invocations")]
    ConcurrencyLimit,

    /// Configuration file is missing, malformed, or contains invalid values.
    #[error("configuration error: {0}")]
    ConfigError(String),

    /// An underlying I/O error occurred.
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

impl From<serde_json::Error> for TalosError {
    fn from(err: serde_json::Error) -> Self {
        TalosError::ParseError(err.to_string())
    }
}

impl From<toml::de::Error> for TalosError {
    fn from(err: toml::de::Error) -> Self {
        TalosError::ConfigError(err.to_string())
    }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, TalosError>;
