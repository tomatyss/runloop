use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use futures_util::StreamExt;
use runloop_bus::{Bus, Message};
use runloop_core::content::{CT_EXECUTOR_AGENT_REQUEST, CT_EXECUTOR_AGENT_RESPONSE};
use runloop_core::{AgentId, AgentRef, Error as CoreError, OpeningId, TraceId};
use runloop_executor_local::LocalExecutor;
use runloop_openings::{
    Executor, NodeExecution, NodeExecutionRequest, NodeInputs, NodeKind, NodeOutputs,
};
use runloop_rmp::{Header, decode_payload, encode_payload};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::error;
use uuid::Uuid;

use crate::utils::{current_millis, next_msg_id, uuid_to_u128};

#[derive(Clone)]
pub struct AgentDispatcher {
    bus: Bus,
    local_executor: Arc<LocalExecutor>,
    workers: Arc<Mutex<HashMap<String, WorkerHandle>>>,
}

impl AgentDispatcher {
    pub fn new(bus: Bus, local_executor: Arc<LocalExecutor>) -> Self {
        Self {
            bus,
            local_executor,
            workers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn ensure_worker(&self, reference: &AgentRef) -> Result<(), CoreError> {
        let topic = agent_topic(reference);
        let mut guard = self.workers.lock().await;
        if guard.contains_key(&topic) {
            return Ok(());
        }
        let worker = self.spawn_worker(topic.clone()).await?;
        guard.insert(topic, worker);
        Ok(())
    }

    async fn spawn_worker(&self, topic: String) -> Result<WorkerHandle, CoreError> {
        let mut sub = self
            .bus
            .subscribe(&topic)
            .await
            .map_err(|err| CoreError::Bus(err.to_string()))?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let bus = self.bus.clone();
        let executor = self.local_executor.clone();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    maybe_msg = sub.next() => {
                        let Some(msg) = maybe_msg else {
                            break;
                        };
                        if msg.header.schema_id != CT_EXECUTOR_AGENT_REQUEST {
                            continue;
                        }
                        match decode_payload::<AgentInvocation>(CT_EXECUTOR_AGENT_REQUEST, &msg.body) {
                            Ok(env) => {
                                if let Err(err) = handle_invocation(&bus, &executor, env.payload).await {
                                    error!(%err, "agent invocation failed");
                                }
                            }
                            Err(err) => {
                                error!("failed to decode agent invocation: {err}");
                            }
                        }
                    }
                }
            }
        });
        Ok(WorkerHandle {
            shutdown: Some(shutdown_tx),
            join: handle,
        })
    }

    pub async fn shutdown(&self) {
        let mut guard = self.workers.lock().await;
        for (_, mut worker) in guard.drain() {
            if let Some(tx) = worker.shutdown.take() {
                let _ = tx.send(());
            }
            worker.join.abort();
        }
    }
}

struct WorkerHandle {
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentInvocation {
    node: runloop_openings::Node,
    inputs: NodeInputs,
    trace_id: TraceId,
    opening_id: OpeningId,
    attempt: u32,
    reply_topic: String,
}

#[derive(Debug, Serialize, Deserialize)]
enum AgentResponse {
    Completed(NodeOutputs),
    Failed { retryable: bool, reason: String },
}

async fn handle_invocation(
    bus: &Bus,
    executor: &Arc<LocalExecutor>,
    invocation: AgentInvocation,
) -> Result<(), CoreError> {
    let AgentInvocation {
        node,
        inputs,
        trace_id,
        opening_id,
        attempt,
        reply_topic,
    } = invocation;

    let response = match node.kind {
        NodeKind::Agent { .. } => {
            let request = NodeExecutionRequest {
                node: &node,
                inputs: &inputs,
                attempt,
                trace_id,
                opening_id,
                agent_id: AgentId::new(),
            };
            match executor.execute(request).await {
                Ok(NodeExecution::Completed(outputs)) => AgentResponse::Completed(outputs),
                Ok(NodeExecution::Failed { retryable, reason }) => {
                    AgentResponse::Failed { retryable, reason }
                }
                Err(err) => AgentResponse::Failed {
                    retryable: false,
                    reason: err.to_string(),
                },
            }
        }
        NodeKind::Opening { name } => AgentResponse::Failed {
            retryable: false,
            reason: format!("nested opening '{name}' unsupported"),
        },
    };

    let body = encode_payload(CT_EXECUTOR_AGENT_RESPONSE, &response, None)
        .map_err(|err| CoreError::Runtime(err.to_string()))?;
    let header = Header {
        schema_id: CT_EXECUTOR_AGENT_RESPONSE,
        trace_id: uuid_to_u128(trace_id.0),
        msg_id: next_msg_id(),
        created_at_ms: current_millis(),
        ..Header::default()
    };
    let message =
        Message::new(header, Bytes::from(body)).map_err(|err| CoreError::Bus(err.to_string()))?;
    bus.publish(&reply_topic, message)
        .await
        .map_err(|err| CoreError::Bus(err.to_string()))
}

fn agent_topic(reference: &AgentRef) -> String {
    format!("agent/{}", reference.name)
}

pub struct BusExecutor {
    bus: Bus,
    dispatcher: Arc<AgentDispatcher>,
}

impl BusExecutor {
    pub fn new(bus: Bus, dispatcher: Arc<AgentDispatcher>) -> Self {
        Self { bus, dispatcher }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::AgentRef;
    use runloop_openings::{Node, Retry, SchemaHints, SourceLocation};
    use serde_json::Map as JsonMap;

    fn sample_node() -> Node {
        Node {
            id: "demo".into(),
            kind: NodeKind::Agent {
                reference: AgentRef::new("writer", None),
            },
            with: JsonMap::new(),
            schema_hints: SchemaHints::default(),
            retry: Retry::default(),
            timeout_ms: Some(1000),
            budget_tokens: None,
            tags: Vec::new(),
            location: SourceLocation { line: 1, column: 1 },
        }
    }

    #[test]
    fn executor_invocation_round_trip() {
        let invocation = AgentInvocation {
            node: sample_node(),
            inputs: NodeInputs::default(),
            trace_id: TraceId::new(),
            opening_id: OpeningId::new(),
            attempt: 1,
            reply_topic: "reply/topic".into(),
        };
        let body = encode_payload(CT_EXECUTOR_AGENT_REQUEST, &invocation, None).unwrap();
        let decoded = decode_payload::<AgentInvocation>(CT_EXECUTOR_AGENT_REQUEST, &body).unwrap();
        assert_eq!(decoded.payload.node.id, "demo");
        assert_eq!(decoded.payload.reply_topic, "reply/topic");
    }

    #[test]
    fn executor_response_round_trip() {
        let outputs = NodeOutputs::default();
        let response = AgentResponse::Completed(outputs);
        let body = encode_payload(CT_EXECUTOR_AGENT_RESPONSE, &response, None).unwrap();
        let decoded = decode_payload::<AgentResponse>(CT_EXECUTOR_AGENT_RESPONSE, &body).unwrap();
        assert!(matches!(decoded.payload, AgentResponse::Completed(_)));
    }
}

#[async_trait::async_trait]
impl Executor for BusExecutor {
    async fn execute(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, runloop_openings::RunnerError> {
        let reference = match &request.node.kind {
            NodeKind::Agent { reference } => reference.clone(),
            NodeKind::Opening { name } => {
                return Err(runloop_openings::RunnerError::Executor(format!(
                    "nested opening '{name}' unsupported"
                )));
            }
        };
        self.dispatcher
            .ensure_worker(&reference)
            .await
            .map_err(|err| runloop_openings::RunnerError::Executor(err.to_string()))?;

        let reply_topic = format!(
            "rlp/runs/{}/agents/{}/{}",
            request.trace_id,
            reference.name,
            Uuid::new_v4()
        );
        let mut reply_sub = self
            .bus
            .subscribe(&reply_topic)
            .await
            .map_err(|err| runloop_openings::RunnerError::Executor(err.to_string()))?;

        let invocation = AgentInvocation {
            node: request.node.clone(),
            inputs: request.inputs.clone(),
            trace_id: request.trace_id,
            opening_id: request.opening_id,
            attempt: request.attempt,
            reply_topic: reply_topic.clone(),
        };

        let body = encode_payload(CT_EXECUTOR_AGENT_REQUEST, &invocation, None)
            .map_err(|err| runloop_openings::RunnerError::Executor(err.to_string()))?;
        let header = Header {
            schema_id: CT_EXECUTOR_AGENT_REQUEST,
            trace_id: uuid_to_u128(request.trace_id.0),
            msg_id: next_msg_id(),
            created_at_ms: current_millis(),
            ..Header::default()
        };
        let message = Message::new(header, Bytes::from(body))
            .map_err(|err| runloop_openings::RunnerError::Executor(err.to_string()))?;

        let topic = agent_topic(&reference);
        self.bus
            .publish(&topic, message)
            .await
            .map_err(|err| runloop_openings::RunnerError::Executor(err.to_string()))?;

        match reply_sub.next().await {
            Some(msg) => {
                if msg.header.schema_id != CT_EXECUTOR_AGENT_RESPONSE {
                    return Err(runloop_openings::RunnerError::Executor(
                        "agent response schema mismatch".into(),
                    ));
                }
                let env = decode_payload::<AgentResponse>(CT_EXECUTOR_AGENT_RESPONSE, &msg.body)
                    .map_err(|err| runloop_openings::RunnerError::Executor(err.to_string()))?;
                match env.payload {
                    AgentResponse::Completed(outputs) => Ok(NodeExecution::Completed(outputs)),
                    AgentResponse::Failed { retryable, reason } => {
                        Ok(NodeExecution::Failed { retryable, reason })
                    }
                }
            }
            None => Err(runloop_openings::RunnerError::Executor(
                "agent reply channel closed".into(),
            )),
        }
    }
}
