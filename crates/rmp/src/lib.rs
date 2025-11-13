//! Runloop Message Protocol helpers (header + body encoding helpers).

pub mod envelope;
pub mod header;
pub mod registry;

pub use envelope::{DecodedEnvelope, EnvelopeMeta, decode_payload, encode_payload};
pub use header::{FrameDecodeError, HEADER_LEN, HEADER_VERSION, Header};

use thiserror::Error;

/// Errors surfaced by RMP helpers beyond frame decoding.
#[derive(Debug, Error)]
pub enum Error {
    /// Schema identifier missing from the registry.
    #[error("unknown schema id {0}")]
    UnknownSchema(u16),
    /// Body `type` field disagrees with header schema mapping.
    #[error("body type mismatch: expected schema id {expected:#06x}, got type '{actual}'")]
    BodyTypeMismatch { expected: u16, actual: String },
    /// TTL field invalid or unsupported.
    #[error("invalid ttl {0} ms")]
    InvalidTtl(u64),
    /// Expiry calculation overflowed 64-bit range.
    #[error("invalid expiry (created_at_ms={created_at_ms}, ttl_ms={ttl_ms})")]
    InvalidExpiry { created_at_ms: u64, ttl_ms: u64 },
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
