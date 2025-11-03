//! Registry of Runloop Message Protocol schema identifiers.

/// Reserved/invalid schema id.
pub const CT_INVALID: u16 = 0;

// Core primitives (1-10)
pub const CT_OBSERVATION: u16 = 1;
pub const CT_INTENT: u16 = 2;
pub const CT_TOOL_CALL: u16 = 3;
pub const CT_TOOL_RESULT: u16 = 4;
pub const CT_ARTIFACT: u16 = 5;
pub const CT_CRITIQUE: u16 = 6;
pub const CT_STATE_DELTA: u16 = 7;
pub const CT_RUN_EVENT: u16 = 8;
pub const CT_CONTROL: u16 = 9;
pub const CT_ERROR_REPORT: u16 = 10;

// Host shim/control (100-119)
pub const CT_AGENT_HELLO: u16 = 100;
pub const CT_RUNTIME_HELLO: u16 = 101;
pub const CT_INIT: u16 = 102;
pub const CT_HOSTCALL_REQ: u16 = 110;
pub const CT_HOSTCALL_RES: u16 = 111;

// Canonical agent payloads (200-249)
pub const CT_INTENT_CONTACTS_RESOLVE: u16 = 200;
pub const CT_CONTACTS_RESOLVED: u16 = 201;
pub const CT_INTENT_CONTEXT_GATHER: u16 = 210;
pub const CT_CONTEXT_BUNDLE: u16 = 211;
pub const CT_INTENT_DRAFT_WRITE: u16 = 220;
pub const CT_DRAFT_EMAIL: u16 = 221;
pub const CT_INTENT_REVIEW_DRAFT: u16 = 230;
pub const CT_REVIEW_CRITIQUE: u16 = 231;
pub const CT_INTENT_SEND_MAIL: u16 = 240;
pub const CT_MAIL_RESULT: u16 = 241;

// CLI control plane (2001-2002)
pub const CT_CTRL_REQ: u16 = 2001;
pub const CT_CTRL_RESP: u16 = 2002;

// Observability / UI topics (3001-3006)
pub const CT_METRICS_SNAPSHOT: u16 = 3001;
pub const CT_OPENING_EVENT: u16 = 3002;
pub const CT_TRACE_LINE: u16 = 3003;
pub const CT_AGENT_LOG_LINE: u16 = 3004;
pub const CT_ACTION_REQUEST: u16 = 3005;
pub const CT_ACTION_DECISION: u16 = 3006;
