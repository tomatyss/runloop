//! Shim/runtime handshake payload definitions.
use runloop_core::content::{CT_AGENT_HELLO, CT_RUNTIME_HELLO};
use runloop_rmp::{decode_payload, encode_payload};
use serde::{Deserialize, Serialize};

use crate::caps::EffectiveCaps;
use crate::error::Result;

/// Summary of capabilities advertised or granted during handshake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilitySummary {
    /// Bitmask describing coarse capability families (future expansion).
    pub bitmap: u64,
    /// Human-readable capability labels (e.g., `kb.read.contacts`).
    #[serde(default)]
    pub labels: Vec<String>,
}

impl CapabilitySummary {
    /// Construct a summary from the provided capability labels.
    #[must_use]
    pub fn new(bitmap: u64, labels: Vec<String>) -> Self {
        Self { bitmap, labels }
    }
}

/// Optional trace metadata that agents can attach to handshakes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceContext {
    /// Hex-encoded trace identifier.
    pub trace_id: String,
    /// Optional opening identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opening_id: Option<String>,
    /// Optional node identifier within the opening DAG.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

/// Payload sent by the shim/agent when attaching to the bus/runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentHello {
    /// Logical agent identifier.
    pub agent_id: String,
    /// Semantic version of the shim sending the hello.
    pub shim_version: String,
    /// Capability summary declared by the agent bundle.
    pub capabilities: CapabilitySummary,
    /// Optional trace context for diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
}

impl AgentHello {
    /// Encode the hello payload into an RMP envelope.
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(encode_payload(CT_AGENT_HELLO, self)?)
    }

    /// Decode a hello payload from the provided bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(decode_payload(CT_AGENT_HELLO, bytes)?)
    }
}

/// Payload returned by the runtime when accepting an agent shim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentHelloAck {
    /// Opaque session identifier.
    pub session_id: String,
    /// Effective capabilities granted after policy overrides.
    pub effective_caps: EffectiveCaps,
    /// Recommended heartbeat interval in milliseconds.
    pub heartbeat_ms: u32,
}

impl AgentHelloAck {
    /// Encode the ack payload into an RMP envelope.
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(encode_payload(CT_RUNTIME_HELLO, self)?)
    }

    /// Decode the ack payload from bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(decode_payload(CT_RUNTIME_HELLO, bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let hello = AgentHello {
            agent_id: "agent:test".into(),
            shim_version: "1.2.3".into(),
            capabilities: CapabilitySummary::new(0xAA, vec!["kb.read.contacts".into()]),
            trace: Some(TraceContext {
                trace_id: "trace-1".into(),
                opening_id: Some("opening-1".into()),
                node_id: None,
            }),
        };
        let bytes = hello.encode().expect("encode hello");
        let decoded = AgentHello::decode(&bytes).expect("decode hello");
        assert_eq!(decoded.agent_id, hello.agent_id);
        assert_eq!(decoded.capabilities.labels, hello.capabilities.labels);
    }
}
