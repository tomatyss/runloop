use crate::{AgentDigest, AgentRef, DescribedAgent, OpeningId, TraceId};
use serde::{Deserialize, Serialize};

/// Control-plane request envelope carried over `CT_CTRL_REQ`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ControlRequest {
    /// Submit an opening (YAML) for execution via the daemon.
    RunSubmit(RunSubmitRequest),
    /// Cancel a previously-submitted run by trace id.
    RunCancel(RunCancelRequest),
    /// Describe manifest metadata for a list of agent references.
    DescribeAgents(DescribeAgentsRequest),
}

/// Submit an opening YAML blob for execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunSubmitRequest {
    pub request_id: TraceId,
    pub opening_yaml: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_digests: Vec<AgentDigest>,
}

/// Cancel request payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunCancelRequest {
    pub trace_id: TraceId,
}

/// Describe agents referenced by the CLI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DescribeAgentsRequest {
    pub request_id: TraceId,
    pub agents: Vec<AgentRef>,
}

/// Control-plane response carried over `CT_CTRL_RESP`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ControlResponse {
    RunAccepted(RunAccepted),
    RunRejected {
        request_id: TraceId,
        reason: String,
    },
    RunCancelled {
        request_id: TraceId,
        trace_id: TraceId,
    },
    AgentsDescribed {
        request_id: TraceId,
        agents: Vec<DescribedAgent>,
    },
    AgentsDescribeFailed {
        request_id: TraceId,
        reason: String,
    },
}

/// Run submission acceptance payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunAccepted {
    pub request_id: TraceId,
    pub trace_id: TraceId,
    pub opening_id: OpeningId,
    pub opening_name: String,
}
