use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

/// Magic bytes guarding the header.
pub const MAGIC: [u8; 4] = *b"RMP0";
/// Current protocol header version.
pub const HEADER_VERSION: u16 = 1;
/// Expected header length in bytes (excluding frame length prefix).
pub const HEADER_LEN: u16 = 60;

/// Maximum frame payload supported by the decoder (1 MiB by default).
pub const DEFAULT_MAX_FRAME_LEN: u32 = 1_048_576;

/// RMP header fields (v0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub header_version: u16,
    pub header_len: u16,
    pub flags: u16,
    pub schema_id: u16,
    pub body_len: u32,
    pub created_at_ms: u64,
    pub ttl_ms: u32,
    pub trace_id: u128,
    pub opening_id: u64,
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
            ttl_ms: 30_000,
            trace_id: 0,
            opening_id: 0,
            msg_id: 0,
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
    /// Header magic mismatch.
    #[error("invalid magic {0:?}")]
    InvalidMagic([u8; 4]),
    /// Header length/version mismatch.
    #[error("unsupported header version {0} or length {1}")]
    Unsupported(u16, u16),
}

impl Header {
    /// Encode header into provided buffer (excluding the frame length prefix).
    pub fn encode_into(&self, buf: &mut BytesMut, body_len: u32) {
        buf.put_slice(&MAGIC);
        buf.put_u16(self.header_version);
        buf.put_u16(self.header_len);
        buf.put_u16(self.flags);
        buf.put_u16(self.schema_id);
        buf.put_u32(body_len);
        buf.put_u64(self.created_at_ms);
        buf.put_u32(self.ttl_ms);
        buf.put_u128(self.trace_id);
        buf.put_u64(self.opening_id);
        buf.put_u64(self.msg_id);
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
        let flags = buf.get_u16();
        Ok(Self {
            header_version,
            header_len,
            flags,
            schema_id: buf.get_u16(),
            body_len: buf.get_u32(),
            created_at_ms: buf.get_u64(),
            ttl_ms: buf.get_u32(),
            trace_id: buf.get_u128(),
            opening_id: buf.get_u64(),
            msg_id: buf.get_u64(),
        })
    }
}

/// Encode a header + body into a single frame buffer (with u32 length prefix).
pub fn encode_frame(header: &Header, body: &[u8]) -> BytesMut {
    let frame_len = HEADER_LEN as usize + body.len();
    let mut buf = BytesMut::with_capacity(4 + frame_len);
    buf.put_u32(frame_len as u32);
    let mut header = header.clone();
    header.body_len = body.len() as u32;
    header.encode_into(&mut buf, header.body_len);
    buf.extend_from_slice(body);
    buf
}

/// Decode a frame from bytes.
pub fn decode_frame(bytes: &[u8], max_len: u32) -> Result<(Header, &[u8]), FrameDecodeError> {
    if bytes.len() < 4 {
        return Err(FrameDecodeError::Incomplete);
    }
    let mut cursor = &bytes[..];
    let frame_len = cursor.get_u32();
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
        header.flags = 0;
        header.trace_id = 42;
        header.opening_id = 1024;
        header.msg_id = 7;
        header.schema_id = 221;
        let body = b"{\"type\":221,\"payload\":{}}";
        let frame = encode_frame(&header, body);
        let (decoded, decoded_body) =
            decode_frame(&frame[..], DEFAULT_MAX_FRAME_LEN).expect("decode");
        assert_eq!(decoded.trace_id, 42);
        assert_eq!(decoded.opening_id, 1024);
        assert_eq!(decoded.msg_id, 7);
        assert_eq!(decoded.schema_id, 221);
        assert_eq!(decoded.body_len as usize, body.len());
        assert_eq!(decoded_body, body);
    }

    #[test]
    fn decode_rejects_oversized() {
        let header = Header::default();
        let frame = encode_frame(&header, &[0; 4]);
        assert!(matches!(
            decode_frame(&frame[..], 2),
            Err(FrameDecodeError::Oversized(_))
        ));
    }

    #[test]
    fn decode_rejects_truncated_header() {
        let mut buf = BytesMut::new();
        buf.put_u32(2); // smaller than HEADER_LEN
        buf.extend_from_slice(&[0u8; 2]);
        assert!(matches!(
            decode_frame(&buf[..], DEFAULT_MAX_FRAME_LEN),
            Err(FrameDecodeError::Truncated)
        ));
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut buf = BytesMut::new();
        buf.put_u32(HEADER_LEN as u32);
        buf.extend_from_slice(b"RMQ0");
        buf.put_u16(HEADER_VERSION);
        buf.put_u16(HEADER_LEN);
        buf.put_u16(0);
        buf.put_u16(0);
        buf.put_u32(0);
        buf.put_u64(0);
        buf.put_u32(0);
        buf.put_u128(0);
        buf.put_u64(0);
        buf.put_u64(0);
        assert!(matches!(
            decode_frame(&buf[..], DEFAULT_MAX_FRAME_LEN),
            Err(FrameDecodeError::InvalidMagic(_))
        ));
    }
}
