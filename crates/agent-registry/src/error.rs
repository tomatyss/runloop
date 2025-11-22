use runloop_core::AgentRef;
use std::path::PathBuf;
use thiserror::Error;

/// Errors encountered while resolving agent manifests.
#[derive(Debug, Error)]
pub enum AgentRegistryError {
    #[error("agent manifest not found for {reference}")]
    NotFound { reference: AgentRef },
    #[error("{path}: {source}")]
    IoPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read manifest {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse manifest {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("agent mismatch for {reference}: {detail}")]
    Mismatch { reference: AgentRef, detail: String },
    #[error("schema error for {reference}: {detail}")]
    Schema { reference: AgentRef, detail: String },
    #[error("artifact error for {reference}: {detail}")]
    Artifact { reference: AgentRef, detail: String },
    #[error("tools.json error: {detail}")]
    Tools {
        reference: Option<AgentRef>,
        detail: String,
    },
}
