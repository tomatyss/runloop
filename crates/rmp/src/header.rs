use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error as ThisError;

use crate::Error;

/// Magic bytes guarding the Runloop Message Protocol frame.
pub const MAGIC: [u8; 4] = *b"RMP0";
/// Current protocol header version.
pub const HEADER_VERSION: u16 = 0;
/// Expected header length in bytes.
pub const HEADER_LEN: u16 = 64;

/// Default TTL used when callers do not override it (milliseconds).
pub const DEFAULT_TTL_MS: u64 = 30_000;
/// Maximum MsgPack body length (8 MiB by default) enforced by the decoder.
pub const DEFAULT_MAX_FRAME_LEN: u32 = 8 * 1024 * 1024;

/// Errors produced during frame decoding.
#[derive(Debug, ThisError)]
pub enum FrameDecodeError {
    /// Frame exceeded configured maximum length.
    #[error("frame length {0} exceeds maximum allowed")]
    Oversized(u32),
    /// Frame shorter than expected header.
    #[error("frame truncated before header could be read")]
    Truncated,
    /// Frame incomplete (more bytes required).
    #[error("frame incomplete")]
    Incomplete,
    /// Header magic mismatch.
    #[error("invalid magic {0:?}")]
    InvalidMagic([u8; 4]),
    /// Header length/version mismatch.
    #[error("unsupported header version {0} or length {1}")]
    Unsupported(u16, u16),
    /// Header flags/reserved bits violated v0 invariants.
    #[error("invalid header flags or reserved bits set ({0:#010x})")]
    InvalidHeaderFlags(u32),
    /// Header body length disagrees with actual payload size.
    #[error("body length mismatch (declared {declared}, actual {actual})")]
    LengthMismatch { declared: u32, actual: u32 },
}

/// RMP header fields (v1).
use serde::{Deserialize, Serialize};

/// RMP header fields (v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub header_version: u16,
    pub header_len: u16,
    pub flags: u32,
    pub schema_id: u16,
    pub body_len: u32,
    pub created_at_ms: u64,
    pub ttl_ms: u64,
    pub trace_id: u128,
    pub msg_id: u64,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            header_version: HEADER_VERSION,
            header_len: HEADER_LEN,
            flags: 0,
            schema_id: 0,
            body_len: 0,
            created_at_ms: 0,
            ttl_ms: DEFAULT_TTL_MS,
            trace_id: 0,
            msg_id: 0,
        }
    }
}

impl Header {
    /// Encode header into provided buffer.
    pub fn encode_into(&self, buf: &mut BytesMut) {
        buf.put_slice(&MAGIC);
        buf.put_u16(self.header_version);
        buf.put_u16(self.header_len);
        buf.put_u32(self.flags);
        buf.put_u16(self.schema_id);
        buf.put_u16(0); // reserved2
        buf.put_u32(self.body_len);
        buf.put_u64(self.created_at_ms);
        buf.put_u64(self.ttl_ms);
        buf.put_u128(self.trace_id);
        buf.put_u64(self.msg_id);
        buf.put_u32(0); // reserved4
    }

    /// Decode header from buffer.
    pub fn decode_from<B: Buf>(mut buf: B) -> Result<Self, FrameDecodeError> {
        if buf.remaining() < HEADER_LEN as usize {
            return Err(FrameDecodeError::Truncated);
        }
        let mut magic = [0u8; 4];
        buf.copy_to_slice(&mut magic);
        if magic != MAGIC {
            return Err(FrameDecodeError::InvalidMagic(magic));
        }
        let header_version = buf.get_u16();
        let header_len = buf.get_u16();
        if header_version != HEADER_VERSION || header_len != HEADER_LEN {
            return Err(FrameDecodeError::Unsupported(header_version, header_len));
        }
        let flags = buf.get_u32();
        if flags != 0 {
            return Err(FrameDecodeError::InvalidHeaderFlags(flags));
        }
        let schema_id = buf.get_u16();
        let reserved2 = buf.get_u16();
        if reserved2 != 0 {
            return Err(FrameDecodeError::InvalidHeaderFlags(reserved2 as u32));
        }
        let body_len = buf.get_u32();
        let created_at_ms = buf.get_u64();
        let ttl_ms = buf.get_u64();
        let trace_id = buf.get_u128();
        let msg_id = buf.get_u64();
        let reserved4 = buf.get_u32();
        if reserved4 != 0 {
            return Err(FrameDecodeError::InvalidHeaderFlags(reserved4));
        }
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

    /// Returns `(trace_id, msg_id)` tuple suitable for dedupe caches.
    pub fn dedupe_key(&self) -> (u128, u64) {
        (self.trace_id, self.msg_id)
    }

    /// Returns expiration timestamp, validating TTL invariants.
    pub fn expires_at_ms(&self) -> Result<u64, Error> {
        if self.ttl_ms == 0 {
            return Err(Error::InvalidTtl(self.ttl_ms));
        }
        let expires = (self.created_at_ms as u128) + (self.ttl_ms as u128);
        if expires > u128::from(u64::MAX) {
            return Err(Error::InvalidExpiry {
                created_at_ms: self.created_at_ms,
                ttl_ms: self.ttl_ms,
            });
        }
        Ok(expires as u64)
    }

    /// Returns `true` if the message has expired relative to `now_ms`.
    pub fn is_expired(&self, now_ms: u64) -> Result<bool, Error> {
        Ok(now_ms >= self.expires_at_ms()?)
    }
}

/// Encode a header + body into a single frame buffer (with 32-bit length prefix).
pub fn encode_frame(header: &Header, body: &[u8]) -> BytesMut {
    let mut header = header.clone();
    header.body_len = body.len() as u32;
    let frame_len = HEADER_LEN as usize + body.len();
    let mut buf = BytesMut::with_capacity(4 + frame_len);
    buf.put_u32(frame_len as u32);
    header.encode_into(&mut buf);
    buf.extend_from_slice(body);
    buf
}

/// Decode a frame from bytes, enforcing a maximum body length.
pub fn decode_frame(bytes: &[u8], max_len: u32) -> Result<(Header, &[u8]), FrameDecodeError> {
    if bytes.len() < 4 {
        return Err(FrameDecodeError::Incomplete);
    }
    let mut cursor = bytes;
    let frame_len = cursor.get_u32();
    if cursor.len() < frame_len as usize {
        return Err(FrameDecodeError::Incomplete);
    }
    if frame_len < HEADER_LEN as u32 {
        return Err(FrameDecodeError::Truncated);
    }
    let header_bytes = &cursor[..HEADER_LEN as usize];
    let header = Header::decode_from(header_bytes)?;
    let body_len = frame_len - HEADER_LEN as u32;
    if header.body_len != body_len {
        return Err(FrameDecodeError::LengthMismatch {
            declared: header.body_len,
            actual: body_len,
        });
    }
    if header.body_len > max_len {
        return Err(FrameDecodeError::Oversized(header.body_len));
    }
    let body = &cursor[HEADER_LEN as usize..(HEADER_LEN as usize + body_len as usize)];
    Ok((header, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let mut header = Header::default();
        header.trace_id = 42;
        header.msg_id = 7;
        header.schema_id = 221;
        header.created_at_ms = 1_000;
        header.ttl_ms = 5_000;
        let body = b"{\"type\":\"example\",\"payload\":{}}";
        let frame = encode_frame(&header, body);
        let (decoded, decoded_body) =
            decode_frame(&frame[..], DEFAULT_MAX_FRAME_LEN).expect("decode");
        assert_eq!(decoded.trace_id, 42);
        assert_eq!(decoded.msg_id, 7);
        assert_eq!(decoded.schema_id, 221);
        assert_eq!(decoded.body_len as usize, body.len());
        assert_eq!(decoded_body, body);
    }

    #[test]
    fn decode_rejects_oversized_body() {
        let header = Header::default();
        let body = vec![0u8; 8];
        let frame = encode_frame(&header, &body);
        assert!(matches!(
            decode_frame(&frame[..], 4),
            Err(FrameDecodeError::Oversized(len)) if len == body.len() as u32
        ));
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut buf = BytesMut::with_capacity(HEADER_LEN as usize);
        buf.put_slice(b"RMQ0");
        buf.put_u16(HEADER_VERSION);
        buf.put_u16(HEADER_LEN);
        buf.put_u32(0); // flags
        buf.put_u16(0); // schema_id
        buf.put_u16(0); // reserved2
        buf.put_u32(0); // body_len
        buf.put_u64(0); // created_at
        buf.put_u64(1); // ttl_ms
        buf.put_u128(0); // trace id
        buf.put_u64(0); // msg id
        buf.put_u32(0); // reserved4
        assert!(matches!(
            Header::decode_from(&buf[..]),
            Err(FrameDecodeError::InvalidMagic(_))
        ));
    }

    #[test]
    fn expires_at_validates_ttl() {
        let mut header = Header::default();
        header.created_at_ms = 1_000;
        header.ttl_ms = 2_000;
        assert_eq!(header.expires_at_ms().unwrap(), 3_000);
        assert!(!header.is_expired(2_500).unwrap());
        assert!(header.is_expired(3_500).unwrap());
    }

    #[test]
    fn expires_at_zero_ttl_is_error() {
        let mut header = Header::default();
        header.ttl_ms = 0;
        assert!(matches!(header.expires_at_ms(), Err(Error::InvalidTtl(0))));
        assert!(matches!(
            header.is_expired(u64::MAX),
            Err(Error::InvalidTtl(0))
        ));
    }
}
