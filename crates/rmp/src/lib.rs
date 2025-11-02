use std::fmt;

use bytes::{BufMut, Bytes, BytesMut};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

pub mod registry;

pub const MAGIC: &[u8; 4] = b"RMP0";
pub const HEADER_LEN: usize = 60;
const HEADER_VERSION_V0: u16 = 0;

bitflags::bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct HeaderFlags: u16 {
        const SIGNED = 0b0000_0001;
        const COMPRESSED = 0b0000_0010;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub header_version: u16,
    pub header_len: u16,
    pub flags: HeaderFlags,
    pub schema_id: u16,
    pub body_len: u32,
    pub created_at_ms: u64,
    pub ttl_ms: u32,
    pub trace_id: [u8; 16],
    pub msg_id: [u8; 16],
}

impl Header {
    pub fn new(schema_id: u16, body_len: u32, ttl_ms: u32) -> Self {
        let now_ms = Utc::now().timestamp_millis() as u64;
        Self {
            header_version: HEADER_VERSION_V0,
            header_len: HEADER_LEN as u16,
            flags: HeaderFlags::empty(),
            schema_id,
            body_len,
            created_at_ms: now_ms,
            ttl_ms,
            trace_id: Uuid::now_v7().into_bytes(),
            msg_id: Uuid::now_v7().into_bytes(),
        }
    }

    pub fn with_ids(
        schema_id: u16,
        body_len: u32,
        ttl_ms: u32,
        trace_id: [u8; 16],
        msg_id: [u8; 16],
    ) -> Self {
        let now_ms = Utc::now().timestamp_millis() as u64;
        Self {
            header_version: HEADER_VERSION_V0,
            header_len: HEADER_LEN as u16,
            flags: HeaderFlags::empty(),
            schema_id,
            body_len,
            created_at_ms: now_ms,
            ttl_ms,
            trace_id,
            msg_id,
        }
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut buf = [0u8; HEADER_LEN];
        buf[..4].copy_from_slice(MAGIC);
        buf[4..6].copy_from_slice(&self.header_version.to_be_bytes());
        buf[6..8].copy_from_slice(&self.header_len.to_be_bytes());
        buf[8..10].copy_from_slice(&self.flags.bits().to_be_bytes());
        buf[10..12].copy_from_slice(&self.schema_id.to_be_bytes());
        buf[12..16].copy_from_slice(&self.body_len.to_be_bytes());
        buf[16..24].copy_from_slice(&self.created_at_ms.to_be_bytes());
        buf[24..28].copy_from_slice(&self.ttl_ms.to_be_bytes());
        buf[28..44].copy_from_slice(&self.trace_id);
        buf[44..60].copy_from_slice(&self.msg_id);
        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(DecodeError::TruncatedHeader(bytes.len()));
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[..4]);
        if &magic != MAGIC {
            return Err(DecodeError::BadMagic(magic));
        }
        let header_version = u16::from_be_bytes(bytes[4..6].try_into().unwrap());
        if header_version != HEADER_VERSION_V0 {
            return Err(DecodeError::UnsupportedVersion(header_version));
        }
        let header_len = u16::from_be_bytes(bytes[6..8].try_into().unwrap());
        if header_len as usize != HEADER_LEN {
            return Err(DecodeError::InvalidHeaderLen(header_len));
        }
        let flags = HeaderFlags::from_bits(u16::from_be_bytes(bytes[8..10].try_into().unwrap()))
            .ok_or(DecodeError::InvalidFlags(u16::from_be_bytes(
                bytes[8..10].try_into().unwrap(),
            )))?;
        let schema_id = u16::from_be_bytes(bytes[10..12].try_into().unwrap());
        let body_len = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        let created_at_ms = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
        let ttl_ms = u32::from_be_bytes(bytes[24..28].try_into().unwrap());
        let mut trace_id = [0u8; 16];
        trace_id.copy_from_slice(&bytes[28..44]);
        let mut msg_id = [0u8; 16];
        msg_id.copy_from_slice(&bytes[44..60]);

        Ok(Self {
            header_version,
            header_len,
            flags,
            schema_id,
            body_len,
            created_at_ms,
            ttl_ms,
            trace_id,
            msg_id,
        })
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(self.created_at_ms as i64)
            .single()
            .unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap())
    }

    pub fn expires_at_ms(&self) -> Option<u64> {
        if self.ttl_ms == 0 {
            None
        } else {
            Some(self.created_at_ms.saturating_add(self.ttl_ms as u64))
        }
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms()
            .map(|deadline| now_ms > deadline)
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: Header,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(DecodeError::TruncatedHeader(bytes.len()));
        }
        let header = Header::decode(bytes)?;
        let total_len = HEADER_LEN + header.body_len as usize;
        if bytes.len() < total_len {
            return Err(DecodeError::TruncatedBody {
                expected: header.body_len as usize,
                actual: bytes.len() - HEADER_LEN,
            });
        }
        let body = bytes[HEADER_LEN..HEADER_LEN + header.body_len as usize].to_vec();
        Ok(Self { header, body })
    }

    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(HEADER_LEN + self.body.len());
        buf.put_slice(&self.header.encode());
        buf.put_slice(&self.body);
        buf.freeze()
    }

    pub fn with_payload<T: Serialize>(
        schema_id: u16,
        ttl_ms: u32,
        trace_id: [u8; 16],
        msg_id: [u8; 16],
        payload: &T,
    ) -> Result<Self, EncodeError> {
        let envelope = BodyEnvelope::new(schema_id, payload);
        let body = rmp_serde::to_vec_named(&envelope).map_err(EncodeError::Msgpack)?;
        let header = Header::with_ids(schema_id, body.len() as u32, ttl_ms, trace_id, msg_id);
        Ok(Self { header, body })
    }

    pub fn to_envelope<T: DeserializeOwned>(&self) -> Result<BodyEnvelope<T>, DecodeError> {
        let envelope: BodyEnvelope<T> =
            rmp_serde::from_slice(&self.body).map_err(DecodeError::Msgpack)?;
        if envelope.schema_type != self.header.schema_id {
            return Err(DecodeError::SchemaMismatch {
                header: self.header.schema_id,
                body: envelope.schema_type,
            });
        }
        Ok(envelope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyEnvelope<T> {
    #[serde(rename = "type")]
    pub schema_type: u16,
    pub payload: T,
}

impl<T> BodyEnvelope<T> {
    pub fn new(schema_id: u16, payload: T) -> Self {
        Self {
            schema_type: schema_id,
            payload,
        }
    }
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("header truncated: {0} bytes available")]
    TruncatedHeader(usize),
    #[error("body truncated: expected {expected} bytes, got {actual}")]
    TruncatedBody { expected: usize, actual: usize },
    #[error("invalid magic: {0:?}")]
    BadMagic([u8; 4]),
    #[error("unsupported header version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid header length {0}")]
    InvalidHeaderLen(u16),
    #[error("invalid flags {0:#06x}")]
    InvalidFlags(u16),
    #[error("schema mismatch header={header}, body={body}")]
    SchemaMismatch { header: u16, body: u16 },
    #[error("msgpack decode error: {0}")]
    Msgpack(rmp_serde::decode::Error),
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("msgpack encode error: {0}")]
    Msgpack(rmp_serde::encode::Error),
}

impl fmt::Display for HeaderFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngCore, SeedableRng};
    use rand_chacha::ChaCha20Rng;

    #[test]
    fn header_roundtrip() {
        let mut rng = ChaCha20Rng::seed_from_u64(42);
        let mut trace_id = [0u8; 16];
        rng.fill_bytes(&mut trace_id);
        let mut msg_id = [0u8; 16];
        rng.fill_bytes(&mut msg_id);

        let header = Header::with_ids(0x0001, 128, 5_000, trace_id, msg_id);
        let encoded = header.encode();
        let decoded = Header::decode(&encoded).unwrap();
        assert_eq!(header, decoded);
    }

    #[test]
    fn frame_with_payload_roundtrip() {
        let trace_id = Uuid::now_v7().into_bytes();
        let msg_id = Uuid::now_v7().into_bytes();
        let payload = serde_json::json!({ "hello": "world" });
        let frame = Frame::with_payload(0x0002, 10_000, trace_id, msg_id, &payload).unwrap();
        let encoded = frame.encode();
        let decoded = Frame::from_encoded(&encoded).unwrap();
        let envelope: BodyEnvelope<serde_json::Value> = decoded.to_envelope().unwrap();
        assert_eq!(envelope.schema_type, 0x0002);
        assert_eq!(envelope.payload, payload);
    }

    #[test]
    fn ttl_eval() {
        let mut header = Header::new(0x0001, 0, 1000);
        header.created_at_ms = 1000;
        assert!(header.is_expired(3001));
        assert!(!header.is_expired(1999));
        header.ttl_ms = 0;
        assert!(!header.is_expired(u64::MAX));
    }
}
