use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Top-level runtime errors surfaced to callers.
#[derive(Debug, Error)]
pub enum Error {
    #[error("policy manifest missing `capabilities` table")]
    InvalidPolicyFormat,
    #[error("policy parse failed: {0}")]
    PolicyParse(String),
    #[error("invalid filesystem capability entry: {0}")]
    InvalidFsEntry(String),
    #[error("invalid network host entry: {0}")]
    InvalidNetworkHost(String),
    #[error("invalid capability entry: {0}")]
    InvalidCapabilityEntry(String),
    #[error("capability denied: {0}")]
    CapDenied(String),
    #[error("unknown agent")]
    UnknownAgent,
    #[error("agent already exists: {0}")]
    AgentAlreadyExists(String),
    #[error("agent spawn failed: {0}")]
    SpawnFailed(String),
    #[error("agent task join failed: {0}")]
    AgentJoinFailed(String),
    #[error("statistics unavailable")]
    StatsUnavailable,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("wasmtime error: {0}")]
    Wasmtime(#[from] wasmtime::Error),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("capability override error: {0}")]
    Override(String),
}

impl Error {
    pub fn spawn_failed(path: PathBuf, reason: impl Into<String>) -> Self {
        Self::SpawnFailed(format!("{} ({})", path.display(), reason.into()))
    }
}

impl From<Error> for runloop_core::Error {
    fn from(err: Error) -> Self {
        use runloop_core::Error as CoreError;
        match err {
            Error::CapDenied(msg) => CoreError::CapDenied(msg),
            Error::Config(msg) => CoreError::Config(msg),
            Error::Io(err) => CoreError::Io(err),
            Error::Wasmtime(err) => CoreError::Runtime(err.to_string()),
            other => CoreError::Runtime(other.to_string()),
        }
    }
}
