use once_cell::sync::Lazy;
use std::collections::HashMap;

use crate::content::*;

static ID_TO_NAME: Lazy<HashMap<u16, &'static str>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(CT_INVALID, "invalid");
    map.insert(CT_OBSERVATION, "observation");
    map.insert(CT_INTENT, "intent");
    map.insert(CT_TOOL_CALL, "tool.call");
    map.insert(CT_TOOL_RESULT, "tool.result");
    map.insert(CT_ARTIFACT, "artifact");
    map.insert(CT_CRITIQUE, "critique");
    map.insert(CT_STATE_DELTA, "state.delta");
    map.insert(CT_RUN_EVENT, "run.event");
    map.insert(CT_CONTROL, "control");
    map.insert(CT_ERROR_REPORT, "error.report");
    map.insert(CT_AGENT_HELLO, "agent.hello");
    map.insert(CT_RUNTIME_HELLO, "runtime.hello");
    map.insert(CT_INIT, "init");
    map.insert(CT_HOSTCALL_REQ, "hostcall.request");
    map.insert(CT_HOSTCALL_RES, "hostcall.response");
    map.insert(CT_INTENT_CONTACTS_RESOLVE, "intent.contacts.resolve");
    map.insert(CT_CONTACTS_RESOLVED, "contacts.resolved");
    map.insert(CT_INTENT_CONTEXT_GATHER, "intent.context.gather");
    map.insert(CT_CONTEXT_BUNDLE, "context.bundle");
    map.insert(CT_INTENT_DRAFT_WRITE, "intent.draft.write");
    map.insert(CT_DRAFT_EMAIL, "draft.email");
    map.insert(CT_INTENT_REVIEW_DRAFT, "intent.review.draft");
    map.insert(CT_REVIEW_CRITIQUE, "review.critique");
    map.insert(CT_INTENT_SEND_MAIL, "intent.send.mail");
    map.insert(CT_MAIL_RESULT, "mail.result");
    map.insert(CT_CTRL_REQ, "ctrl.request");
    map.insert(CT_CTRL_RESP, "ctrl.response");
    map.insert(CT_METRICS_SNAPSHOT, "metrics.snapshot");
    map.insert(CT_OPENING_EVENT, "opening.event");
    map.insert(CT_TRACE_LINE, "trace.line");
    map.insert(CT_AGENT_LOG_LINE, "agent.log.line");
    map.insert(CT_ACTION_REQUEST, "action.request");
    map.insert(CT_ACTION_DECISION, "action.decision");
    map.insert(CT_BUS_DROP_NOTICE, "bus.drop.notice");
    map
});

static NAME_TO_ID: Lazy<HashMap<&'static str, u16>> =
    Lazy::new(|| ID_TO_NAME.iter().map(|(id, name)| (*name, *id)).collect());

/// Resolve the canonical type string for a schema identifier.
pub fn type_name_for(schema_id: u16) -> Option<&'static str> {
    ID_TO_NAME.get(&schema_id).copied()
}

/// Resolve the schema identifier for a canonical type string.
pub fn schema_for(type_name: &str) -> Option<u16> {
    NAME_TO_ID.get(type_name).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidirectional_lookup() {
        for (id, name) in ID_TO_NAME.iter() {
            assert_eq!(type_name_for(*id), Some(*name));
            assert_eq!(schema_for(name), Some(*id));
        }
        assert!(schema_for("missing").is_none());
    }
}
