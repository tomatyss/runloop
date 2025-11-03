use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

/// Current protocol header version.
pub const HEADER_VERSION: u8 = 1;
/// Expected header length in bytes (excluding frame length prefix).
pub const HEADER_LEN: u16 = 60;

/// Maximum frame payload supported by the decoder (1 MiB by default).
pub const DEFAULT_MAX_FRAME_LEN: u32 = 1_048_576;

/// RMP header fields (v0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub header_version: u8,
    pub header_flags: u8,
    pub header_len: u16,
    pub trace_id: u128,
    pub opening_id: u64,
    pub msg_id: u64,
    pub created_at_ms: u64,
    pub ttl_ms: u32,
    pub caps_bitmap: u32,
    pub tokens_budget: u32,
    pub schema_id: u16,
    pub priority: u8,
    pub reserved: u8,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            header_version: HEADER_VERSION,
            header_flags: 0,
            header_len: HEADER_LEN,
            trace_id: 0,
            opening_id: 0,
            msg_id: 0,
            created_at_ms: 0,
            ttl_ms: 30_000,
            caps_bitmap: 0,
            tokens_budget: 0,
            schema_id: 0,
            priority: 0,
            reserved: 0,
        }
    }
}

/// Errors produced during frame decoding.
#[derive(Debug, Error)]
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
    /// Header length/version mismatch.
    #[error("unsupported header version {0} or length {1}")]
    Unsupported(u8, u16),
}

impl Header {
    /// Encode header into provided buffer (excluding the frame length prefix).
    pub fn encode_into(&self, buf: &mut BytesMut) {
        buf.put_u8(self.header_version);
        buf.put_u8(self.header_flags);
        buf.put_u16_le(self.header_len);
        buf.put_u128_le(self.trace_id);
        buf.put_u64_le(self.opening_id);
        buf.put_u64_le(self.msg_id);
        buf.put_u64_le(self.created_at_ms);
        buf.put_u32_le(self.ttl_ms);
        buf.put_u32_le(self.caps_bitmap);
        buf.put_u32_le(self.tokens_budget);
        buf.put_u16_le(self.schema_id);
        buf.put_u8(self.priority);
        buf.put_u8(self.reserved);
    }

    /// Decode header from buffer.
    pub fn decode_from<B: Buf>(mut buf: B) -> Result<Self, FrameDecodeError> {
        if buf.remaining() < HEADER_LEN as usize {
            return Err(FrameDecodeError::Truncated);
        }
        let header_version = buf.get_u8();
        let header_flags = buf.get_u8();
        let header_len = buf.get_u16_le();
        if header_version != HEADER_VERSION || header_len != HEADER_LEN {
            return Err(FrameDecodeError::Unsupported(header_version, header_len));
        }
        Ok(Self {
            header_version,
            header_flags,
            header_len,
            trace_id: buf.get_u128_le(),
            opening_id: buf.get_u64_le(),
            msg_id: buf.get_u64_le(),
            created_at_ms: buf.get_u64_le(),
            ttl_ms: buf.get_u32_le(),
            caps_bitmap: buf.get_u32_le(),
            tokens_budget: buf.get_u32_le(),
            schema_id: buf.get_u16_le(),
            priority: buf.get_u8(),
            reserved: buf.get_u8(),
        })
    }
}

/// Encode a header + body into a single frame buffer (with u32 length prefix).
pub fn encode_frame(header: &Header, body: &[u8]) -> BytesMut {
    let frame_len = HEADER_LEN as usize + body.len();
    let mut buf = BytesMut::with_capacity(4 + frame_len);
    buf.put_u32_le(frame_len as u32);
    header.encode_into(&mut buf);
    buf.extend_from_slice(body);
    buf
}

/// Decode a frame from bytes.
pub fn decode_frame(bytes: &[u8], max_len: u32) -> Result<(Header, &[u8]), FrameDecodeError> {
    if bytes.len() < 4 {
        return Err(FrameDecodeError::Incomplete);
    }
    let mut cursor = &bytes[..];
    let frame_len = cursor.get_u32_le();
    if frame_len > max_len {
        return Err(FrameDecodeError::Oversized(frame_len));
    }
    if cursor.len() < frame_len as usize {
        return Err(FrameDecodeError::Incomplete);
    }
    if frame_len < HEADER_LEN as u32 {
        return Err(FrameDecodeError::Truncated);
    }
    let header_bytes = &cursor[..HEADER_LEN as usize];
    let header = Header::decode_from(header_bytes)?;
    let body = &cursor[HEADER_LEN as usize..frame_len as usize];
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
        header.priority = 3;
        let body = b"{\"type\":221,\"payload\":{}}";
        let frame = encode_frame(&header, body);
        let (decoded, decoded_body) =
            decode_frame(&frame[..], DEFAULT_MAX_FRAME_LEN).expect("decode");
        assert_eq!(decoded.trace_id, 42);
        assert_eq!(decoded.msg_id, 7);
        assert_eq!(decoded.schema_id, 221);
        assert_eq!(decoded.priority, 3);
        assert_eq!(decoded_body, body);
    }

    #[test]
    fn decode_rejects_oversized() {
        let mut header = Header::default();
        let frame = encode_frame(&header, &[0; 4]);
        assert!(matches!(
            decode_frame(&frame[..], 2),
            Err(FrameDecodeError::Oversized(_))
        ));
    }

    #[test]
    fn decode_rejects_truncated_header() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(2); // smaller than HEADER_LEN
        buf.extend_from_slice(&[0u8; 2]);
        assert!(matches!(
            decode_frame(&buf[..], DEFAULT_MAX_FRAME_LEN),
            Err(FrameDecodeError::Truncated)
        ));
    }
}
