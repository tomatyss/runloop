use rmp_serde::{decode::Deserializer, encode::Serializer};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::Cursor;

use crate::{Error, registry};

/// `{ "type": <schema_id>, "payload": ... }` MsgPack wrapper carried in RMP bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    #[serde(rename = "type")]
    pub schema_id: u16,
    pub payload: T,
}

impl<T> Envelope<T> {
    /// Construct a new envelope with the provided schema identifier.
    pub fn new(schema_id: u16, payload: T) -> Self {
        Self { schema_id, payload }
    }
}

/// Encode a payload into a MsgPack envelope derived from the schema id.
pub fn encode_payload<T: Serialize>(schema_id: u16, payload: &T) -> Result<Vec<u8>, Error> {
    registry::type_name_for(schema_id).ok_or(Error::UnknownSchema(schema_id))?;
    let env = Envelope::new(schema_id, payload);
    let mut buf = Vec::new();
    let mut serializer = Serializer::new(&mut buf).with_struct_map();
    env.serialize(&mut serializer)?;
    Ok(buf)
}

/// Decode a payload and assert the embedded type matches the schema mapping.
pub fn decode_payload<T: DeserializeOwned>(schema_id: u16, bytes: &[u8]) -> Result<T, Error> {
    registry::type_name_for(schema_id).ok_or(Error::UnknownSchema(schema_id))?;
    let mut deserializer = Deserializer::new(Cursor::new(bytes));
    let env = Envelope::<T>::deserialize(&mut deserializer)?;
    if env.schema_id != schema_id {
        return Err(Error::BodyTypeMismatch {
            expected: schema_id,
            actual: env.schema_id,
        });
    }
    Ok(env.payload)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct TestPayload {
        value: u32,
    }

    #[test]
    fn envelope_roundtrip() {
        let payload = TestPayload { value: 42 };
        let buf = encode_payload(runloop_core::content::CT_TRACE_LINE, &payload).unwrap();
        let decoded: TestPayload =
            decode_payload(runloop_core::content::CT_TRACE_LINE, &buf).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn mismatched_type_returns_error() {
        let payload = TestPayload { value: 7 };
        let env = Envelope::new(0xDEAD, payload);
        let mut buf = Vec::new();
        let mut serializer = Serializer::new(&mut buf).with_struct_map();
        env.serialize(&mut serializer).unwrap();
        let err = decode_payload::<TestPayload>(runloop_core::content::CT_TRACE_LINE, &buf)
            .expect_err("mismatch should error");
        assert!(
            matches!(err, Error::BodyTypeMismatch { expected, actual } if expected == runloop_core::content::CT_TRACE_LINE && actual == 0xDEAD)
        );
    }
}
