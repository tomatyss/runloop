//! Runloop Message Protocol helpers (header + body encoding helpers).

pub mod envelope;
pub mod header;
pub mod registry;

pub use envelope::{Envelope, decode_payload, encode_payload};
pub use header::{FrameDecodeError, HEADER_LEN, HEADER_VERSION, Header};

use thiserror::Error;

/// Errors surfaced by RMP helpers beyond frame decoding.
#[derive(Debug, Error)]
pub enum Error {
    /// Schema identifier missing from the registry.
    #[error("unknown schema id {0}")]
    UnknownSchema(u16),
    /// Type name missing from the registry.
    #[error("unknown content type '{0}'")]
    UnknownContentType(String),
    /// Body `type` field disagrees with header schema mapping.
    #[error("body type mismatch: expected schema id {expected:#06x}, got {actual:#06x}")]
    BodyTypeMismatch { expected: u16, actual: u16 },
    /// TTL field invalid or unsupported.
    #[error("invalid ttl {0} ms")]
    InvalidTtl(u32),
    /// MsgPack encoding failure.
    #[error("msgpack encoding failed: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    /// MsgPack decoding failure.
    #[error("msgpack decoding failed: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
}

impl From<Error> for runloop_core::Error {
    fn from(err: Error) -> Self {
        Self::Rmp(err.to_string())
    }
}
