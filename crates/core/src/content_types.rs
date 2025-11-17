use once_cell::sync::Lazy;
use std::collections::HashMap;

use crate::content::*;

/// Primitive families referenced by the fixed header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeFamily {
    Observation,
    Intent,
    Artifact,
    ToolResult,
    Critique,
    StateDelta,
    Error,
}

impl TypeFamily {
    pub const fn prefix(self) -> &'static str {
        match self {
            TypeFamily::Observation => "observation",
            TypeFamily::Intent => "intent",
            TypeFamily::Artifact => "artifact",
            TypeFamily::ToolResult => "toolresult",
            TypeFamily::Critique => "critique",
            TypeFamily::StateDelta => "statedelta",
            TypeFamily::Error => "error",
        }
    }
}

/// Descriptor tying schema identifiers to canonical type strings.
#[derive(Debug, Clone, Copy)]
pub struct TypeDescriptor {
    pub schema_id: u16,
    pub family: TypeFamily,
    pub kind: &'static str,
    pub version: u16,
    pub type_str: &'static str,
    pub json_schema_id: &'static str,
}

macro_rules! family_prefix {
    (Observation) => {
        "observation"
    };
    (Intent) => {
        "intent"
    };
    (Artifact) => {
        "artifact"
    };
    (ToolResult) => {
        "toolresult"
    };
    (Critique) => {
        "critique"
    };
    (StateDelta) => {
        "statedelta"
    };
    (Error) => {
        "error"
    };
}

macro_rules! type_descriptor {
    ($schema:expr, $family:ident, $kind:expr, $version:expr) => {
        TypeDescriptor {
            schema_id: $schema,
            family: TypeFamily::$family,
            kind: $kind,
            version: $version,
            type_str: concat!(
                family_prefix!($family),
                ".",
                $kind,
                ".v",
                stringify!($version)
            ),
            json_schema_id: concat!(
                "schema://runloop/",
                family_prefix!($family),
                ".",
                $kind,
                ".v",
                stringify!($version),
                "#"
            ),
        }
    };
}

static DESCRIPTORS: &[TypeDescriptor] = &[
    type_descriptor!(CT_OBSERVATION, Observation, "generic", 1),
    type_descriptor!(CT_INTENT, Intent, "generic", 1),
    type_descriptor!(CT_TOOL_CALL, Intent, "tool_call", 1),
    type_descriptor!(CT_TOOL_RESULT, ToolResult, "generic", 1),
    type_descriptor!(CT_ARTIFACT, Artifact, "generic", 1),
    type_descriptor!(CT_CRITIQUE, Critique, "generic", 1),
    type_descriptor!(CT_STATE_DELTA, StateDelta, "generic", 1),
    type_descriptor!(CT_RUN_EVENT, Observation, "run.event", 1),
    type_descriptor!(CT_CONTROL, Intent, "control", 1),
    type_descriptor!(CT_ERROR_REPORT, Error, "report", 1),
    type_descriptor!(CT_AGENT_HELLO, Observation, "agent.hello", 1),
    type_descriptor!(CT_RUNTIME_HELLO, Observation, "runtime.hello", 1),
    type_descriptor!(CT_INIT, Intent, "init", 1),
    type_descriptor!(CT_HOSTCALL_REQ, Intent, "hostcall.request", 1),
    type_descriptor!(CT_HOSTCALL_RES, ToolResult, "hostcall.response", 1),
    type_descriptor!(CT_INTENT_CONTACTS_RESOLVE, Intent, "contacts.resolve", 1),
    type_descriptor!(CT_CONTACTS_RESOLVED, ToolResult, "contacts.resolved", 1),
    type_descriptor!(CT_INTENT_CONTEXT_GATHER, Intent, "context.gather", 1),
    type_descriptor!(CT_CONTEXT_BUNDLE, Observation, "context.bundle", 1),
    type_descriptor!(CT_INTENT_DRAFT_WRITE, Intent, "draft.write", 1),
    type_descriptor!(CT_DRAFT_EMAIL, Artifact, "draft.email", 1),
    type_descriptor!(CT_INTENT_REVIEW_DRAFT, Intent, "review.draft", 1),
    type_descriptor!(CT_REVIEW_CRITIQUE, Critique, "review", 1),
    type_descriptor!(CT_INTENT_SEND_MAIL, Intent, "send.mail", 1),
    type_descriptor!(CT_MAIL_RESULT, ToolResult, "mail.result", 1),
    type_descriptor!(
        CT_EXECUTOR_AGENT_REQUEST,
        Intent,
        "executor.agent.request",
        1
    ),
    type_descriptor!(
        CT_EXECUTOR_AGENT_RESPONSE,
        ToolResult,
        "executor.agent.response",
        1
    ),
    type_descriptor!(CT_CTRL_REQ, Intent, "ctrl.request", 1),
    type_descriptor!(CT_CTRL_RESP, ToolResult, "ctrl.response", 1),
    type_descriptor!(CT_METRICS_SNAPSHOT, Observation, "metrics.snapshot", 1),
    type_descriptor!(CT_OPENING_EVENT, Observation, "opening.event", 1),
    type_descriptor!(CT_TRACE_LINE, Observation, "trace.line", 1),
    type_descriptor!(CT_AGENT_LOG_LINE, Observation, "agent.log", 1),
    type_descriptor!(CT_ACTION_REQUEST, Intent, "action.request", 1),
    type_descriptor!(CT_ACTION_DECISION, ToolResult, "action.decision", 1),
    type_descriptor!(CT_BUS_DROP_NOTICE, Error, "bus.drop.notice", 1),
];

static ID_TO_DESC: Lazy<HashMap<u16, &'static TypeDescriptor>> = Lazy::new(|| {
    let mut map = HashMap::new();
    for desc in DESCRIPTORS {
        map.insert(desc.schema_id, desc);
    }
    map
});

static TYPE_TO_DESC: Lazy<HashMap<&'static str, &'static TypeDescriptor>> = Lazy::new(|| {
    ID_TO_DESC
        .values()
        .map(|desc| (desc.type_str, *desc))
        .collect()
});

/// Resolve the canonical descriptor for `schema_id`.
pub fn descriptor_for_schema(schema_id: u16) -> Option<&'static TypeDescriptor> {
    ID_TO_DESC.get(&schema_id).copied()
}

/// Resolve the canonical descriptor for a type string.
pub fn descriptor_for_type(type_str: &str) -> Option<&'static TypeDescriptor> {
    TYPE_TO_DESC.get(type_str).copied()
}

/// Resolve the canonical type string for a schema identifier.
pub fn type_name_for(schema_id: u16) -> Option<&'static str> {
    descriptor_for_schema(schema_id).map(|d| d.type_str)
}

/// Resolve the schema identifier for a canonical type string.
pub fn schema_for(type_name: &str) -> Option<u16> {
    descriptor_for_type(type_name).map(|d| d.schema_id)
}

/// Resolve the primitive family for a schema identifier.
pub fn family_for_schema(schema_id: u16) -> Option<TypeFamily> {
    descriptor_for_schema(schema_id).map(|d| d.family)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidirectional_lookup() {
        for desc in DESCRIPTORS {
            assert_eq!(type_name_for(desc.schema_id), Some(desc.type_str));
            assert_eq!(schema_for(desc.type_str), Some(desc.schema_id));
            assert_eq!(family_for_schema(desc.schema_id), Some(desc.family));
        }
        assert!(schema_for("missing").is_none());
        assert!(descriptor_for_schema(0xFFFF).is_none());
    }
}
