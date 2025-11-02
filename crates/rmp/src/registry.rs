//! Canonical registry of RMP schema identifiers. Keep in sync with `docs/rmp-registry.md`.

pub const CT_OBSERVATION: u16 = 0x0001;
pub const CT_INTENT: u16 = 0x0002;
pub const CT_TOOL_CALL: u16 = 0x0003;
pub const CT_TOOL_RESULT: u16 = 0x0004;
pub const CT_ARTIFACT: u16 = 0x0005;
pub const CT_CRITIQUE: u16 = 0x0006;
pub const CT_STATE_DELTA: u16 = 0x0007;
pub const CT_CONTROL_HEARTBEAT: u16 = 0x0008;
pub const CT_CONTROL_ACK: u16 = 0x0009;
pub const CT_CONTROL_ERROR: u16 = 0x000A;
pub const CT_PLAN_OPENING_SPEC: u16 = 0x000B;
pub const CT_PLAN_NODE_STATUS: u16 = 0x000C;
pub const CT_METRICS_SPAN: u16 = 0x000D;

/// Sentinel value reserved to show that no schema should ever use `0x0000`.
pub const CT_RESERVED_ZERO: u16 = 0x0000;
