use crate::{
    Executor, NodeExecution, NodeExecutionRequest, NodeState, Opening, RunTrace, RunnerError,
};
use blake3::Hasher;
use runloop_core::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayMismatch {
    pub node_id: String,
    pub expected_hash: Option<String>,
    pub actual_hash: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayReport {
    pub matches: bool,
    pub mismatches: Vec<ReplayMismatch>,
    pub replay_hash: String,
}

pub async fn replay<E>(
    executor: &E,
    opening: &Opening,
    trace: &RunTrace,
) -> Result<ReplayReport, RunnerError>
where
    E: Executor + ?Sized,
{
    let mut mismatches = Vec::new();
    let mut hasher = Hasher::new();

    let nodes_by_id: HashMap<_, _> = opening
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    for node_trace in &trace.nodes {
        let Some(node) = nodes_by_id.get(node_trace.node_id.as_str()) else {
            mismatches.push(ReplayMismatch {
                node_id: node_trace.node_id.clone(),
                expected_hash: node_trace
                    .final_attempt
                    .as_ref()
                    .and_then(|attempt| attempt.output_hash.clone()),
                actual_hash: None,
                reason: "node not present in opening definition".into(),
            });
            continue;
        };

        match (&node_trace.final_attempt, &node_trace.state) {
            (None, NodeState::Skipped | NodeState::Cancelled) => continue,
            (None, NodeState::Failed { reason }) => {
                mismatches.push(ReplayMismatch {
                    node_id: node_trace.node_id.clone(),
                    expected_hash: None,
                    actual_hash: None,
                    reason: format!("node recorded as failed: {reason}"),
                });
            }
            (None, NodeState::Succeeded) => {
                mismatches.push(ReplayMismatch {
                    node_id: node_trace.node_id.clone(),
                    expected_hash: None,
                    actual_hash: None,
                    reason: "trace missing final attempt for succeeded node".into(),
                });
            }
            (None, _) => continue,
            (Some(final_attempt), state) => {
                let request = NodeExecutionRequest {
                    node,
                    inputs: &final_attempt.inputs,
                    attempt: final_attempt.attempt,
                    trace_id: trace.trace_id,
                    opening_id: trace.opening_id,
                    agent_id: AgentId::new(),
                };

                let execution = executor.execute(request).await?;
                match (execution, state) {
                    (NodeExecution::Completed(outputs), NodeState::Succeeded) => {
                        let actual_hash = outputs.hash();
                        hasher.update(actual_hash.as_bytes());
                        let expected_hash = final_attempt.output_hash.clone();
                        let actual_hex = actual_hash.to_hex().to_string();
                        if Some(actual_hex.clone()) != expected_hash {
                            mismatches.push(ReplayMismatch {
                                node_id: node_trace.node_id.clone(),
                                expected_hash,
                                actual_hash: Some(actual_hex.clone()),
                                reason: "output hash mismatch".into(),
                            });
                        }
                    }
                    (
                        NodeExecution::Failed { reason, .. },
                        NodeState::Failed { reason: recorded },
                    ) => {
                        let expected_reason =
                            final_attempt.error.as_deref().unwrap_or(recorded.as_str());
                        hasher.update(expected_reason.as_bytes());
                        if expected_reason != reason {
                            mismatches.push(ReplayMismatch {
                                node_id: node_trace.node_id.clone(),
                                expected_hash: None,
                                actual_hash: None,
                                reason: format!(
                                    "failure reason mismatch (expected '{expected_reason}', got '{reason}')"
                                ),
                            });
                        }
                    }
                    (NodeExecution::Completed(outputs), NodeState::Failed { .. }) => {
                        let actual_hash = outputs.hash();
                        mismatches.push(ReplayMismatch {
                            node_id: node_trace.node_id.clone(),
                            expected_hash: final_attempt.output_hash.clone(),
                            actual_hash: Some(actual_hash.to_hex().to_string()),
                            reason: "expected failure but replay succeeded".into(),
                        });
                    }
                    (NodeExecution::Failed { reason, .. }, NodeState::Succeeded) => {
                        mismatches.push(ReplayMismatch {
                            node_id: node_trace.node_id.clone(),
                            expected_hash: final_attempt.output_hash.clone(),
                            actual_hash: None,
                            reason: format!("executor reported failure: {reason}"),
                        });
                    }
                    (NodeExecution::Completed(_), other_state) => {
                        mismatches.push(ReplayMismatch {
                            node_id: node_trace.node_id.clone(),
                            expected_hash: final_attempt.output_hash.clone(),
                            actual_hash: None,
                            reason: format!(
                                "unexpected state {:?} for completed node",
                                other_state
                            ),
                        });
                    }
                    (NodeExecution::Failed { reason, .. }, other_state) => {
                        mismatches.push(ReplayMismatch {
                            node_id: node_trace.node_id.clone(),
                            expected_hash: final_attempt.output_hash.clone(),
                            actual_hash: None,
                            reason: format!(
                                "executor reported failure while node recorded as {:?}: {reason}",
                                other_state
                            ),
                        });
                    }
                }
            }
        };
    }

    let replay_hash = hasher.finalize().to_hex().to_string();
    Ok(ReplayReport {
        matches: mismatches.is_empty(),
        mismatches,
        replay_hash,
    })
}
