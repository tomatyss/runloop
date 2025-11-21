use crate::{Edge, Literal, Node, Opening, Predicate, Retry, SuccessCondition};
use async_trait::async_trait;
use blake3::Hasher;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use runloop_core::{AgentId, OpeningId, TraceId};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use thiserror::Error;
use tokio::{sync::mpsc::UnboundedSender, time};

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("executor error: {0}")]
    Executor(String),
    #[error("node '{node_id}' failed: {reason}")]
    NodeFailure { node_id: String, reason: String },
    #[error("node '{node_id}' timed out after {timeout_ms} ms")]
    Timeout { node_id: String, timeout_ms: u64 },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeInputs {
    pub ports: HashMap<String, Vec<JsonValue>>,
}

impl NodeInputs {
    pub fn push(&mut self, port: &str, value: JsonValue) {
        self.ports.entry(port.to_string()).or_default().push(value);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeOutputs {
    pub ports: HashMap<String, Vec<JsonValue>>,
}

impl NodeOutputs {
    pub fn push(&mut self, port: &str, value: JsonValue) {
        self.ports.entry(port.to_string()).or_default().push(value);
    }

    pub fn hash(&self) -> blake3::Hash {
        let mut hasher = Hasher::new();
        // Stable ordering by port name.
        let mut ports: Vec<_> = self.ports.iter().collect();
        ports.sort_by(|a, b| a.0.cmp(b.0));
        for (port, values) in ports {
            hasher.update(port.as_bytes());
            for value in values {
                let serialized =
                    serde_json::to_vec(value).unwrap_or_else(|_| value.to_string().into_bytes());
                hasher.update(&serialized);
            }
        }
        hasher.finalize()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NodeExecution {
    Completed(NodeOutputs),
    Failed { retryable: bool, reason: String },
}

#[derive(Clone, Debug)]
pub struct NodeExecutionRequest<'a> {
    pub node: &'a Node,
    pub inputs: &'a NodeInputs,
    pub attempt: u32,
    pub trace_id: TraceId,
    pub opening_id: OpeningId,
    pub agent_id: AgentId,
}

#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NodeState {
    Pending,
    Running,
    Succeeded,
    Failed { reason: String },
    Skipped,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeAttemptRecord {
    pub attempt: u32,
    pub started_at: SystemTime,
    pub finished_at: SystemTime,
    pub inputs: NodeInputs,
    pub output: Option<NodeOutputs>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeRecord {
    pub node_id: String,
    pub state: NodeState,
    pub attempts: Vec<NodeAttemptRecord>,
}

fn default_node_state() -> NodeState {
    NodeState::Pending
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeAttemptTrace {
    pub attempt: u32,
    pub inputs: NodeInputs,
    pub outputs: Option<NodeOutputs>,
    pub output_hash: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeTrace {
    pub node_id: String,
    #[serde(default = "default_node_state")]
    pub state: NodeState,
    pub final_attempt: Option<NodeAttemptTrace>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunTrace {
    pub trace_id: TraceId,
    #[serde(default)]
    pub opening_id: OpeningId,
    pub nodes: Vec<NodeTrace>,
    #[serde(default)]
    pub ladder: Vec<LadderHop>,
    pub final_hash: String,
    pub success: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunReport {
    pub trace: RunTrace,
    pub node_records: Vec<NodeRecord>,
}

/// Streaming event emitted during a run for observability consumers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RunEvent {
    NodeState {
        node_id: String,
        state: NodeState,
        attempt: u32,
    },
    LogLine {
        node_id: String,
        level: String,
        message: String,
    },
    TraceLine {
        line: String,
    },
    Completed {
        trace: RunTrace,
    },
}

/// Single hop in the ladder view used by `rlp trace`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LadderHop {
    pub ts_ms: u64,
    pub topic: String,
    pub schema_id: u16,
    pub frame_len: u32,
    pub body_len: u32,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub msg_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

pub struct Runner<E>
where
    E: Executor + 'static,
{
    opening: Opening,
    executor: Arc<E>,
    trace_id: TraceId,
    opening_id: OpeningId,
    event_tx: Option<UnboundedSender<RunEvent>>,
}

impl<E> Runner<E>
where
    E: Executor + 'static,
{
    pub fn new(opening: Opening, executor: Arc<E>) -> Self {
        Self {
            opening,
            executor,
            trace_id: TraceId::new(),
            opening_id: OpeningId::new(),
            event_tx: None,
        }
    }

    /// Attach an event sender for run progress notifications.
    pub fn with_event_tx(mut self, tx: UnboundedSender<RunEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    pub fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    pub fn opening_id(&self) -> OpeningId {
        self.opening_id
    }

    fn emit_event(&self, event: RunEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event);
        }
    }

    pub async fn run(&self) -> Result<RunReport, RunnerError> {
        let mut records: HashMap<String, NodeRecord> = self
            .opening
            .nodes
            .iter()
            .map(|node| {
                (
                    node.id.clone(),
                    NodeRecord {
                        node_id: node.id.clone(),
                        state: NodeState::Pending,
                        attempts: Vec::new(),
                    },
                )
            })
            .collect();

        let inbound_total: HashMap<String, usize> = self
            .opening
            .nodes
            .iter()
            .map(|node| (node.id.clone(), 0usize))
            .collect();
        let mut remaining: HashMap<String, usize> = inbound_total.clone();
        for edge in &self.opening.edges {
            if let Some(counter) = remaining.get_mut(&edge.to.node) {
                *counter += 1;
            }
        }
        let inbound_total = remaining.clone();

        let mut outgoing: HashMap<String, Vec<&Edge>> = HashMap::new();
        for edge in &self.opening.edges {
            outgoing
                .entry(edge.from.node.clone())
                .or_default()
                .push(edge);
        }

        let mut queue = VecDeque::new();
        for node in &self.opening.nodes {
            if inbound_total.get(&node.id).copied().unwrap_or_default() == 0 {
                queue.push_back(node.id.clone());
            }
        }

        let mut input_buffers: HashMap<String, NodeInputs> = HashMap::new();
        for node in &self.opening.nodes {
            if inbound_total.get(&node.id).copied().unwrap_or_default() == 0 {
                input_buffers.entry(node.id.clone()).or_default();
            }
        }

        let mut outputs_cache: HashMap<String, NodeOutputs> = HashMap::new();
        let mut rng = StdRng::from_entropy();
        let mut run_failed = false;
        let mut processed = HashMap::new();

        while let Some(node_id) = queue.pop_front() {
            if run_failed {
                break;
            }

            let node = self
                .opening
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .expect("node present");

            let record = records
                .get_mut(&node.id)
                .expect("record must exist for node");
            record.state = NodeState::Running;

            let mut attempts_used = 0u32;
            let max_attempts = if node.retry.max_attempts == 0 {
                1
            } else {
                node.retry.max_attempts
            };

            let mut node_succeeded = false;

            let attempt_inputs = input_buffers.remove(&node.id).unwrap_or_default();

            loop {
                attempts_used += 1;
                let start_time = SystemTime::now();
                let request = NodeExecutionRequest {
                    node,
                    inputs: &attempt_inputs,
                    attempt: attempts_used,
                    trace_id: self.trace_id,
                    opening_id: self.opening_id,
                    agent_id: AgentId::new(),
                };
                self.emit_event(RunEvent::NodeState {
                    node_id: node.id.clone(),
                    state: NodeState::Running,
                    attempt: attempts_used,
                });
                self.emit_event(RunEvent::LogLine {
                    node_id: node.id.clone(),
                    level: "info".into(),
                    message: format!("node '{}' attempt {} started", node.id, attempts_used),
                });

                let timeout_ms = node
                    .timeout_ms
                    .or(self.opening.policy.timeout_ms)
                    .unwrap_or(30_000);

                let exec_future = self.executor.execute(request);
                let execution = time::timeout(Duration::from_millis(timeout_ms), exec_future).await;

                match execution {
                    Ok(Ok(NodeExecution::Completed(outputs))) => {
                        let finished_at = SystemTime::now();
                        record.attempts.push(NodeAttemptRecord {
                            attempt: attempts_used,
                            started_at: start_time,
                            finished_at,
                            inputs: attempt_inputs.clone(),
                            output: Some(outputs.clone()),
                            error: None,
                        });
                        outputs_cache.insert(node.id.clone(), outputs.clone());
                        record.state = NodeState::Succeeded;
                        node_succeeded = true;
                        self.emit_event(RunEvent::NodeState {
                            node_id: node.id.clone(),
                            state: record.state.clone(),
                            attempt: attempts_used,
                        });
                        self.emit_event(RunEvent::LogLine {
                            node_id: node.id.clone(),
                            level: "info".into(),
                            message: format!(
                                "node '{}' succeeded on attempt {}",
                                node.id, attempts_used
                            ),
                        });
                        self.emit_event(RunEvent::TraceLine {
                            line: format!(
                                "{} -> succeeded (attempt {}, outputs={})",
                                node.id,
                                attempts_used,
                                outputs.ports.keys().cloned().collect::<Vec<_>>().join(", ")
                            ),
                        });

                        if let Some(edges) = outgoing.get(&node.id) {
                            for edge in edges {
                                let predicate_passed = edge
                                    .predicate
                                    .as_ref()
                                    .map(|predicate| {
                                        evaluate_predicate(predicate, &edge.from.port, &outputs)
                                    })
                                    .unwrap_or(true);

                                if predicate_passed
                                    && let Some(values) = outputs.ports.get(&edge.from.port)
                                {
                                    let entry =
                                        input_buffers.entry(edge.to.node.clone()).or_default();
                                    for value in values {
                                        entry.push(&edge.to.port, value.clone());
                                    }
                                }

                                if let Some(counter) = remaining.get_mut(&edge.to.node)
                                    && *counter > 0
                                {
                                    *counter -= 1;
                                    if *counter == 0 {
                                        let delivered = input_buffers
                                            .get(&edge.to.node)
                                            .map(|inputs| !inputs.ports.is_empty())
                                            .unwrap_or(false);
                                        let had_inbound =
                                            inbound_total.get(&edge.to.node).copied().unwrap_or(0)
                                                > 0;
                                        if delivered || !had_inbound {
                                            queue.push_back(edge.to.node.clone());
                                        } else if let Some(record) = records.get_mut(&edge.to.node)
                                        {
                                            record.state = NodeState::Skipped;
                                            processed.insert(edge.to.node.clone(), false);
                                        }
                                    }
                                }
                            }
                        }
                        processed.insert(node.id.clone(), true);
                        break;
                    }
                    Ok(Ok(NodeExecution::Failed { retryable, reason })) => {
                        let finished_at = SystemTime::now();
                        record.attempts.push(NodeAttemptRecord {
                            attempt: attempts_used,
                            started_at: start_time,
                            finished_at,
                            inputs: attempt_inputs.clone(),
                            output: None,
                            error: Some(reason.clone()),
                        });
                        self.emit_event(RunEvent::LogLine {
                            node_id: node.id.clone(),
                            level: "warn".into(),
                            message: format!(
                                "node '{}' failed on attempt {} ({})",
                                node.id, attempts_used, reason
                            ),
                        });
                        if retryable && attempts_used < max_attempts {
                            let delay_ms = compute_backoff_ms(&node.retry, attempts_used, &mut rng);
                            time::sleep(Duration::from_millis(delay_ms)).await;
                            continue;
                        } else {
                            record.state = NodeState::Failed { reason };
                            self.emit_event(RunEvent::NodeState {
                                node_id: node.id.clone(),
                                state: record.state.clone(),
                                attempt: attempts_used,
                            });
                            run_failed = true;
                            break;
                        }
                    }
                    Ok(Err(err)) => {
                        let finished_at = SystemTime::now();
                        record.attempts.push(NodeAttemptRecord {
                            attempt: attempts_used,
                            started_at: start_time,
                            finished_at,
                            inputs: attempt_inputs.clone(),
                            output: None,
                            error: Some(err.to_string()),
                        });
                        record.state = NodeState::Failed {
                            reason: err.to_string(),
                        };
                        self.emit_event(RunEvent::LogLine {
                            node_id: node.id.clone(),
                            level: "error".into(),
                            message: format!(
                                "node '{}' error on attempt {}: {err}",
                                node.id, attempts_used
                            ),
                        });
                        self.emit_event(RunEvent::NodeState {
                            node_id: node.id.clone(),
                            state: record.state.clone(),
                            attempt: attempts_used,
                        });
                        run_failed = true;
                        break;
                    }
                    Err(_) => {
                        let finished_at = SystemTime::now();
                        let reason = format!("timeout after {timeout_ms} ms");
                        record.attempts.push(NodeAttemptRecord {
                            attempt: attempts_used,
                            started_at: start_time,
                            finished_at,
                            inputs: attempt_inputs.clone(),
                            output: None,
                            error: Some(reason.clone()),
                        });
                        self.emit_event(RunEvent::LogLine {
                            node_id: node.id.clone(),
                            level: "warn".into(),
                            message: format!(
                                "node '{}' timed out on attempt {} after {} ms",
                                node.id, attempts_used, timeout_ms
                            ),
                        });
                        if attempts_used < max_attempts {
                            let delay_ms = compute_backoff_ms(&node.retry, attempts_used, &mut rng);
                            time::sleep(Duration::from_millis(delay_ms)).await;
                            continue;
                        } else {
                            record.state = NodeState::Failed {
                                reason: reason.clone(),
                            };
                            self.emit_event(RunEvent::NodeState {
                                node_id: node.id.clone(),
                                state: record.state.clone(),
                                attempt: attempts_used,
                            });
                            run_failed = true;
                            break;
                        }
                    }
                }
            }

            if !node_succeeded && run_failed {
                break;
            }
        }

        if run_failed {
            for record in records.values_mut() {
                if !matches!(
                    record.state,
                    NodeState::Succeeded | NodeState::Failed { .. }
                ) {
                    record.state = NodeState::Skipped;
                    self.emit_event(RunEvent::NodeState {
                        node_id: record.node_id.clone(),
                        state: record.state.clone(),
                        attempt: 0,
                    });
                }
            }
        } else {
            // Ensure any nodes not processed (due to being disconnected) are marked as skipped.
            for (node_id, record) in records.iter_mut() {
                if !matches!(
                    record.state,
                    NodeState::Succeeded | NodeState::Failed { .. }
                ) {
                    if processed.get(node_id).copied().unwrap_or(false) {
                        record.state = NodeState::Succeeded;
                    } else {
                        record.state = NodeState::Skipped;
                    }
                    self.emit_event(RunEvent::NodeState {
                        node_id: record.node_id.clone(),
                        state: record.state.clone(),
                        attempt: 0,
                    });
                }
            }
        }

        let final_success = !run_failed && evaluate_success(&self.opening.success, &outputs_cache);

        let mut node_traces = Vec::new();
        let mut hasher = Hasher::new();

        for node in &self.opening.nodes {
            let record = records.get(&node.id).expect("node record exists");
            let final_attempt = record.attempts.last().cloned();
            if let Some(last) = final_attempt.clone() {
                if let Some(ref outputs) = last.output {
                    hasher.update(outputs.hash().as_bytes());
                }
                if let Some(ref error) = last.error {
                    hasher.update(error.as_bytes());
                }
            }
            let trace = NodeTrace {
                node_id: node.id.clone(),
                state: record.state.clone(),
                final_attempt: final_attempt.map(|attempt| NodeAttemptTrace {
                    attempt: attempt.attempt,
                    inputs: attempt.inputs,
                    outputs: attempt.output.clone(),
                    output_hash: attempt
                        .output
                        .as_ref()
                        .map(|output| output.hash().to_hex().to_string()),
                    error: attempt.error.clone(),
                }),
            };
            node_traces.push(trace);
        }

        let final_hash = hasher.finalize().to_hex().to_string();
        let trace = RunTrace {
            trace_id: self.trace_id,
            opening_id: self.opening_id,
            nodes: node_traces,
            ladder: Vec::new(),
            final_hash,
            success: final_success,
        };

        self.emit_event(RunEvent::Completed {
            trace: trace.clone(),
        });

        Ok(RunReport {
            trace,
            node_records: records.into_values().collect(),
        })
    }
}

fn compute_backoff_ms<R: Rng>(retry: &Retry, attempt: u32, rng: &mut R) -> u64 {
    if attempt == 0 {
        return retry.initial_backoff_ms.max(10);
    }
    let multiplier_pow = retry.multiplier.powi((attempt - 1) as i32);
    let mut backoff = (retry.initial_backoff_ms as f32 * multiplier_pow).max(10.0);
    if let Some(max) = retry.max_backoff_ms {
        backoff = backoff.min(max as f32);
    }
    if retry.jitter > 0.0 {
        let jitter_span = retry.jitter.min(1.0);
        let jitter_offset = rng.gen_range(-jitter_span..=jitter_span);
        backoff *= 1.0 + jitter_offset;
    }
    backoff.max(10.0).round() as u64
}

fn compare_integer_literal(
    num: &serde_json::Number,
    expected: i64,
    op: crate::ComparisonOp,
) -> bool {
    let expected = expected as i128;

    if let Some(actual) = num.as_i64() {
        return compare_i128(actual as i128, expected, op);
    }
    if let Some(actual) = num.as_u64() {
        return compare_i128(actual as i128, expected, op);
    }
    num.as_f64()
        .map(|actual| compare_f64(actual, expected as f64, op))
        .unwrap_or(false)
}

fn evaluate_predicate(predicate: &Predicate, port: &str, outputs: &NodeOutputs) -> bool {
    let Some(values) = outputs.ports.get(port) else {
        return false;
    };
    values.iter().any(|value| match (&predicate.value, value) {
        (Literal::Bool(expected), JsonValue::Bool(actual)) => {
            compare_bool(*actual, *expected, predicate.op)
        }
        (Literal::String(expected), JsonValue::String(actual)) => {
            compare_string(actual, expected, predicate.op)
        }
        (Literal::Integer(expected), JsonValue::Number(num)) => {
            compare_integer_literal(num, *expected, predicate.op)
        }
        (Literal::Float(expected), JsonValue::Number(num)) => num
            .as_f64()
            .map(|actual| compare_f64(actual, *expected, predicate.op))
            .unwrap_or(false),
        _ => false,
    })
}

fn compare_i128(actual: i128, expected: i128, op: crate::ComparisonOp) -> bool {
    match op {
        crate::ComparisonOp::Eq => actual == expected,
        crate::ComparisonOp::NotEq => actual != expected,
        crate::ComparisonOp::Gt => actual > expected,
        crate::ComparisonOp::Gte => actual >= expected,
        crate::ComparisonOp::Lt => actual < expected,
        crate::ComparisonOp::Lte => actual <= expected,
    }
}

fn compare_bool(actual: bool, expected: bool, op: crate::ComparisonOp) -> bool {
    match op {
        crate::ComparisonOp::Eq => actual == expected,
        crate::ComparisonOp::NotEq => actual != expected,
        _ => false,
    }
}

fn compare_string(actual: &str, expected: &str, op: crate::ComparisonOp) -> bool {
    match op {
        crate::ComparisonOp::Eq => actual == expected,
        crate::ComparisonOp::NotEq => actual != expected,
        crate::ComparisonOp::Gt => actual > expected,
        crate::ComparisonOp::Gte => actual >= expected,
        crate::ComparisonOp::Lt => actual < expected,
        crate::ComparisonOp::Lte => actual <= expected,
    }
}

fn compare_f64(actual: f64, expected: f64, op: crate::ComparisonOp) -> bool {
    match op {
        crate::ComparisonOp::Eq => (actual - expected).abs() < f64::EPSILON,
        crate::ComparisonOp::NotEq => (actual - expected).abs() >= f64::EPSILON,
        crate::ComparisonOp::Gt => actual > expected,
        crate::ComparisonOp::Gte => actual >= expected,
        crate::ComparisonOp::Lt => actual < expected,
        crate::ComparisonOp::Lte => actual <= expected,
    }
}

fn evaluate_success(
    success: &Option<SuccessCondition>,
    outputs: &HashMap<String, NodeOutputs>,
) -> bool {
    let Some(condition) = success else {
        return true;
    };
    match condition {
        SuccessCondition::AnyOf(expressions) => expressions
            .iter()
            .any(|expr| evaluate_expression(expr, outputs)),
        SuccessCondition::AllOf(expressions) => expressions
            .iter()
            .all(|expr| evaluate_expression(expr, outputs)),
    }
}

fn evaluate_expression(expr: &crate::Expression, outputs: &HashMap<String, NodeOutputs>) -> bool {
    match expr {
        crate::Expression::Exists(reference) => outputs
            .get(&reference.node)
            .and_then(|output| output.ports.get(&reference.port))
            .map(|values| !values.is_empty())
            .unwrap_or(false),
        crate::Expression::Comparison(port_predicate) => outputs
            .get(&port_predicate.reference.node)
            .map(|output| {
                evaluate_predicate(
                    &port_predicate.predicate,
                    &port_predicate.reference.port,
                    output,
                )
            })
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integer_predicate_handles_large_unsigned_values_for_gt() {
        let mut outputs = NodeOutputs::default();
        outputs.push("out", json!(u64::MAX));

        let predicate = Predicate {
            op: crate::ComparisonOp::Gt,
            value: Literal::Integer(-1),
        };

        assert!(evaluate_predicate(&predicate, "out", &outputs));
    }

    #[test]
    fn integer_predicate_handles_large_unsigned_values_for_lt() {
        let mut outputs = NodeOutputs::default();
        outputs.push("out", json!(u64::MAX));

        let predicate = Predicate {
            op: crate::ComparisonOp::Lt,
            value: Literal::Integer(i64::MAX),
        };

        assert!(!evaluate_predicate(&predicate, "out", &outputs));
    }

    #[test]
    fn string_predicate_supports_ordering_ops() {
        let mut outputs = NodeOutputs::default();
        outputs.push("out", json!("lion"));

        let gt_predicate = Predicate {
            op: crate::ComparisonOp::Gt,
            value: Literal::String("hawk".into()),
        };
        assert!(evaluate_predicate(&gt_predicate, "out", &outputs));

        let lt_predicate = Predicate {
            op: crate::ComparisonOp::Lt,
            value: Literal::String("zebra".into()),
        };
        assert!(evaluate_predicate(&lt_predicate, "out", &outputs));
    }
}
