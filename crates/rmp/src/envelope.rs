use rmp_serde::{decode::Deserializer, encode::Serializer};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Cursor;

use crate::{Error, registry};

/// Optional metadata surfaced alongside every payload.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_id: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Result of decoding a MsgPack envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedEnvelope<T> {
    pub type_str: String,
    pub payload: T,
    pub meta: Option<EnvelopeMeta>,
}

#[derive(Serialize)]
struct EncEnvelope<'a, T: ?Sized> {
    #[serde(rename = "type")]
    type_str: &'a str,
    #[serde(rename = "payload")]
    payload: &'a T,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<&'a EnvelopeMeta>,
}

#[derive(Deserialize)]
struct RawEnvelope<T> {
    #[serde(rename = "type")]
    type_str: String,
    payload: T,
    #[serde(default)]
    meta: Option<EnvelopeMeta>,
}

/// Encode a payload + optional metadata into a MsgPack envelope derived from the schema id.
pub fn encode_payload<T: Serialize>(
    schema_id: u16,
    payload: &T,
    meta: Option<&EnvelopeMeta>,
) -> Result<Vec<u8>, Error> {
    let descriptor =
        registry::descriptor_for_schema(schema_id).ok_or(Error::UnknownSchema(schema_id))?;
    let env = EncEnvelope {
        type_str: descriptor.type_str,
        payload,
        meta,
    };
    let mut buf = Vec::new();
    let mut serializer = Serializer::new(&mut buf).with_struct_map();
    env.serialize(&mut serializer)?;
    Ok(buf)
}

/// Decode a payload and assert the embedded type matches the schema mapping.
pub fn decode_payload<T: DeserializeOwned>(
    schema_id: u16,
    bytes: &[u8],
) -> Result<DecodedEnvelope<T>, Error> {
    let descriptor =
        registry::descriptor_for_schema(schema_id).ok_or(Error::UnknownSchema(schema_id))?;
    let mut deserializer = Deserializer::new(Cursor::new(bytes));
    let RawEnvelope {
        type_str,
        payload,
        meta,
    } = RawEnvelope::<T>::deserialize(&mut deserializer)?;
    if registry::schema_for(&type_str) != Some(descriptor.schema_id) {
        return Err(Error::BodyTypeMismatch {
            expected: descriptor.schema_id,
            actual: type_str,
        });
    }
    Ok(DecodedEnvelope {
        type_str,
        payload,
        meta,
    })
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
        let meta = EnvelopeMeta {
            opening_id: Some(7),
            priority: Some(1),
            ..EnvelopeMeta::default()
        };
        let buf =
            encode_payload(runloop_core::content::CT_TRACE_LINE, &payload, Some(&meta)).unwrap();
        let decoded =
            decode_payload::<TestPayload>(runloop_core::content::CT_TRACE_LINE, &buf).unwrap();
        assert_eq!(decoded.payload, payload);
        assert_eq!(decoded.meta.as_ref().unwrap().opening_id, Some(7));
    }

    #[test]
    fn mismatched_type_returns_error() {
        let payload = TestPayload { value: 7 };
        let bogus = EncEnvelope {
            type_str: "artifact.created.v1",
            payload: &payload,
            meta: None,
        };
        let mut buf = Vec::new();
        let mut serializer = Serializer::new(&mut buf).with_struct_map();
        bogus.serialize(&mut serializer).unwrap();
        let err = decode_payload::<TestPayload>(runloop_core::content::CT_TRACE_LINE, &buf)
            .expect_err("mismatch should error");
        assert!(matches!(err, Error::BodyTypeMismatch { .. }));
    }
}
