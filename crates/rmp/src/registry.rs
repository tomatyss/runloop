//! Registry of Runloop Message Protocol schema identifiers.

pub use runloop_core::content::{
    CT_ACTION_DECISION, CT_ACTION_REQUEST, CT_AGENT_HELLO, CT_AGENT_LOG_LINE, CT_ARTIFACT,
    CT_BUS_DROP_NOTICE, CT_CONTACTS_RESOLVED, CT_CONTEXT_BUNDLE, CT_CONTROL, CT_CRITIQUE,
    CT_CTRL_REQ, CT_CTRL_RESP, CT_DRAFT_EMAIL, CT_ERROR_REPORT, CT_HOSTCALL_REQ, CT_HOSTCALL_RES,
    CT_INIT, CT_INTENT, CT_INTENT_CONTACTS_RESOLVE, CT_INTENT_CONTEXT_GATHER,
    CT_INTENT_DRAFT_WRITE, CT_INTENT_REVIEW_DRAFT, CT_INTENT_SEND_MAIL, CT_INVALID, CT_MAIL_RESULT,
    CT_METRICS_SNAPSHOT, CT_OBSERVATION, CT_OPENING_EVENT, CT_REVIEW_CRITIQUE, CT_RUN_EVENT,
    CT_RUNTIME_HELLO, CT_STATE_DELTA, CT_TOOL_CALL, CT_TOOL_RESULT, CT_TRACE_LINE,
};

pub use runloop_core::content_types::{TypeDescriptor, TypeFamily};

/// Lookup the canonical type string for a schema identifier.
pub fn type_name_for(schema_id: u16) -> Option<&'static str> {
    runloop_core::content_types::type_name_for(schema_id)
}

/// Resolve a schema identifier given its canonical type string.
pub fn schema_for(type_name: &str) -> Option<u16> {
    runloop_core::content_types::schema_for(type_name)
}

/// Lookup the descriptor for a schema identifier.
pub fn descriptor_for_schema(schema_id: u16) -> Option<&'static TypeDescriptor> {
    runloop_core::content_types::descriptor_for_schema(schema_id)
}
