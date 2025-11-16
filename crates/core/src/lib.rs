//! Core types shared across Runloop crates.

pub mod agents;
pub mod config;
pub mod content;
pub mod content_types;
pub mod control;
pub mod error;
pub mod ids;
pub mod trace;

pub use agents::{AgentDigest, AgentPorts, AgentRef, AgentSchemaBundle, DescribedAgent};
pub use config::{Config, ConfigLayer, ConfigOverride, ConfigSource};
pub use content::{
    CT_ACTION_DECISION, CT_ACTION_REQUEST, CT_AGENT_HELLO, CT_AGENT_LOG_LINE, CT_ARTIFACT,
    CT_BUS_DROP_NOTICE, CT_CONTACTS_RESOLVED, CT_CONTEXT_BUNDLE, CT_CONTROL, CT_CRITIQUE,
    CT_CTRL_REQ, CT_CTRL_RESP, CT_DRAFT_EMAIL, CT_ERROR_REPORT, CT_HOSTCALL_REQ, CT_HOSTCALL_RES,
    CT_INIT, CT_INTENT, CT_INTENT_CONTACTS_RESOLVE, CT_INTENT_CONTEXT_GATHER,
    CT_INTENT_DRAFT_WRITE, CT_INTENT_REVIEW_DRAFT, CT_INTENT_SEND_MAIL, CT_INVALID, CT_MAIL_RESULT,
    CT_METRICS_SNAPSHOT, CT_OBSERVATION, CT_OPENING_EVENT, CT_REVIEW_CRITIQUE, CT_RUN_EVENT,
    CT_RUNTIME_HELLO, CT_STATE_DELTA, CT_TOOL_CALL, CT_TOOL_RESULT, CT_TRACE_LINE,
};
pub use control::{
    ControlRequest, ControlResponse, DescribeAgentsRequest, RunAccepted, RunCancelRequest,
    RunSubmitRequest,
};
pub use error::Error;
pub use ids::{AgentId, EventId, OpeningId, TraceId};
pub use trace::TraceContext;
