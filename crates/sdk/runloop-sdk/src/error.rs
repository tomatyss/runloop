use std::env::VarError;
use thiserror::Error;

/// Errors surfaced by the agent SDK/shim utilities.
#[derive(Debug, Error)]
pub enum Error {
    /// Required environment variable missing.
    #[error("missing environment variable {0}")]
    MissingEnv(&'static str),
    /// Environment variable present but malformed.
    #[error("invalid environment variable {var}: {reason}")]
    InvalidEnv {
        /// Environment variable name.
        var: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// Capability description could not be parsed.
    #[error("invalid capability manifest: {0}")]
    InvalidCaps(String),
    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Bus-level failure.
    #[error(transparent)]
    Bus(#[from] runloop_bus::BusError),
    /// RMP encode/decode failure.
    #[error(transparent)]
    Rmp(#[from] runloop_rmp::Error),
}

impl Error {
    pub(crate) fn invalid(var: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidEnv {
            var,
            reason: reason.into(),
        }
    }

    pub(crate) fn from_var(var: &'static str, err: VarError) -> Self {
        match err {
            VarError::NotPresent => Self::MissingEnv(var),
            VarError::NotUnicode(value) => Self::InvalidEnv {
                var,
                reason: format!("value is not valid UTF-8: {value:?}"),
            },
        }
    }
}

/// Convenient result alias for SDK operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<crate::caps::CapsParseError> for Error {
    fn from(err: crate::caps::CapsParseError) -> Self {
        match err {
            crate::caps::CapsParseError::Json(inner) => Self::Json(inner),
            crate::caps::CapsParseError::Invalid(reason) => Self::InvalidCaps(reason),
        }
    }
}
