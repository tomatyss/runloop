use std::io;
use std::path::PathBuf;

use runloop_core::ids::EventId;
use thiserror::Error;

/// Capability bucket identifier for denial reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapKind {
    Fs,
    Net,
    Time,
    KbRead,
    KbWrite,
    Model,
    Secrets,
    Exec,
    Other,
}

impl CapKind {
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        if let Some((prefix, _)) = label.split_once('.') {
            return Self::from_prefix(prefix);
        }
        Self::from_prefix(label)
    }

    fn from_prefix(prefix: &str) -> Self {
        match prefix {
            "fs" | "fs_ro" | "fs_rw" => CapKind::Fs,
            "net" | "net.http" => CapKind::Net,
            "time" => CapKind::Time,
            "kb" | "kb.read" => CapKind::KbRead,
            "kb.write" => CapKind::KbWrite,
            "model" => CapKind::Model,
            "secrets" => CapKind::Secrets,
            "exec" => CapKind::Exec,
            _ => CapKind::Other,
        }
    }
}

impl std::fmt::Display for CapKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            CapKind::Fs => "fs",
            CapKind::Net => "net",
            CapKind::Time => "time",
            CapKind::KbRead => "kb_read",
            CapKind::KbWrite => "kb_write",
            CapKind::Model => "model",
            CapKind::Secrets => "secrets",
            CapKind::Exec => "exec",
            CapKind::Other => "other",
        };
        write!(f, "{name}")
    }
}

/// Structured capability denial context propagated to callers.
#[derive(Debug, Clone)]
pub struct CapDeniedInfo {
    pub cap: CapKind,
    pub op: String,
    pub detail: String,
    pub reason: String,
    pub audit_event: Option<EventId>,
}

impl CapDeniedInfo {
    #[must_use]
    pub fn new(
        cap: impl AsRef<str>,
        op: impl Into<String>,
        detail: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            cap: CapKind::from_label(cap.as_ref()),
            op: op.into(),
            detail: detail.into(),
            reason: reason.into(),
            audit_event: None,
        }
    }
}

impl std::fmt::Display for CapDeniedInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "capability denied: cap={}, op={}, detail={}, reason={}",
            self.cap, self.op, self.detail, self.reason
        )
    }
}

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
    #[error("{0}")]
    CapDenied(CapDeniedInfo),
    #[error("unknown agent")]
    UnknownAgent,
    #[error("agent already exists: {0}")]
    AgentAlreadyExists(String),
    #[error("agent spawn failed: {0}")]
    SpawnFailed(String),
    #[error("agent failed to enter ready state within {ms} ms")]
    ReadyTimeout { ms: u64, agent: String },
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
    #[error("agent {agent} requires secrets that cannot be resolved: {secrets:?}")]
    SecretsMissing { agent: String, secrets: Vec<String> },
}

impl Error {
    pub fn spawn_failed(path: PathBuf, reason: impl Into<String>) -> Self {
        Self::SpawnFailed(format!("{} ({})", path.display(), reason.into()))
    }
}

impl From<CapDeniedInfo> for Error {
    fn from(info: CapDeniedInfo) -> Self {
        Self::CapDenied(info)
    }
}

impl From<Error> for runloop_core::Error {
    fn from(err: Error) -> Self {
        use runloop_core::Error as CoreError;
        match err {
            Error::CapDenied(info) => CoreError::CapDenied(info.to_string()),
            Error::ReadyTimeout { ms, agent } => {
                CoreError::Timeout(format!("agent {agent} not ready after {ms} ms"))
            }
            Error::Config(msg) => CoreError::Config(msg),
            Error::Io(err) => CoreError::Io(err),
            Error::Wasmtime(err) => CoreError::Runtime(err.to_string()),
            other => CoreError::Runtime(other.to_string()),
        }
    }
}
