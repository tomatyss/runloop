use runloop_rmp::{
    decode_payload, encode_payload,
    header::{
        DEFAULT_MAX_FRAME_LEN, FrameDecodeError, HEADER_LEN, Header, MAGIC, decode_frame,
        encode_frame,
    },
};
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Payload {
    value: u32,
}

#[tokio::test(flavor = "multi_thread")]
async fn duplex_roundtrip() -> io::Result<()> {
    let (mut writer, mut reader) = tokio::io::duplex(256);

    let mut header = Header::default();
    header.schema_id = runloop_core::content::CT_TRACE_LINE;
    header.trace_id = 0xAA;
    header.msg_id = 1;
    header.created_at_ms = 10;
    header.ttl_ms = 1_000;

    let payload = Payload { value: 123 };
    let body = encode_payload(header.schema_id, &payload, None).expect("encode envelope");
    let frame = encode_frame(&header, &body);

    writer.write_all(&frame).await?;

    let mut buf = vec![0u8; frame.len()];
    reader.read_exact(&mut buf).await?;

    let (decoded_header, decoded_body) =
        decode_frame(&buf, DEFAULT_MAX_FRAME_LEN).expect("decode frame");
    assert_eq!(decoded_header.schema_id, header.schema_id);
    assert_eq!(decoded_header.trace_id, header.trace_id);
    assert_eq!(decoded_header.msg_id, header.msg_id);
    let decoded =
        decode_payload::<Payload>(decoded_header.schema_id, decoded_body).expect("decode payload");
    assert_eq!(decoded.payload, payload);
    Ok(())
}

#[test]
fn decode_handles_corrupt_frames() {
    let err = decode_frame(&[], DEFAULT_MAX_FRAME_LEN).unwrap_err();
    assert!(matches!(err, FrameDecodeError::Incomplete));

    let err = decode_frame(&vec![0u8; 3], DEFAULT_MAX_FRAME_LEN).unwrap_err();
    assert!(matches!(err, FrameDecodeError::Incomplete));

    let mut wrong_magic = vec![0u8; 4 + HEADER_LEN as usize];
    wrong_magic[..4].copy_from_slice(&(HEADER_LEN as u32).to_be_bytes());
    wrong_magic[4..8].copy_from_slice(&MAGIC);
    // Corrupt magic value
    wrong_magic[4] = b'X';
    let err = decode_frame(&wrong_magic, DEFAULT_MAX_FRAME_LEN).unwrap_err();
    assert!(matches!(err, FrameDecodeError::InvalidMagic(_)));

    let mut truncated = vec![0u8; 4 + (HEADER_LEN as usize - 1)];
    truncated[..4].copy_from_slice(&((HEADER_LEN - 1) as u32).to_be_bytes());
    truncated[4..8].copy_from_slice(&MAGIC);
    let err = decode_frame(&truncated, DEFAULT_MAX_FRAME_LEN).unwrap_err();
    assert!(matches!(err, FrameDecodeError::Truncated));

    // Body length mismatch
    let mut header = Header::default();
    header.schema_id = runloop_core::content::CT_TRACE_LINE;
    let body = encode_payload(header.schema_id, &Payload { value: 7 }, None).unwrap();
    let mut frame = encode_frame(&header, &body);
    let invalid_len = (body.len() as u32) + 1;
    let offset = 4  /* frame prefix */
        + 4        /* magic */
        + 2        /* header_version */
        + 2        /* header_len */
        + 4        /* flags */
        + 2        /* schema_id */
        + 2; /* reserved2 */
    frame[offset..offset + 4].copy_from_slice(&invalid_len.to_be_bytes());
    let err = decode_frame(&frame, DEFAULT_MAX_FRAME_LEN).unwrap_err();
    assert!(
        matches!(
            err,
            FrameDecodeError::LengthMismatch { declared, actual }
            if declared == invalid_len && actual == body.len() as u32
        ),
        "unexpected error: {err:?}"
    );
}
