use bytes::Bytes;
use futures_util::StreamExt;
use runloop_agent_registry::AgentRegistry;
use runloop_bus::{Bus, Message};
use runloop_core::content::{CT_CTRL_REQ, CT_CTRL_RESP};
use runloop_core::{
    AgentRef, Config, ControlRequest, ControlResponse, DescribeAgentsRequest, Error, RunAccepted,
    RunSubmitRequest, TraceId,
};
use runloop_kb::TraceStore;
use runloop_rmp::{decode_payload, encode_payload};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::engine::{RunSubmitContext, handle_run_submit};
use crate::executor_bus::AgentDispatcher;
use crate::utils::{build_header, next_msg_id, uuid_to_u128};

pub struct ControlPlaneCtx {
    pub config: Config,
    pub registry: Arc<AgentRegistry>,
    pub bus: Bus,
    pub dispatcher: Arc<AgentDispatcher>,
    pub trace_store: TraceStore,
}

pub async fn run_control_plane_with_ready(
    ctx: ControlPlaneCtx,
    mut shutdown: oneshot::Receiver<()>,
    ready: Option<oneshot::Sender<()>>,
) -> Result<(), Error> {
    let ControlPlaneCtx {
        config: _config,
        registry,
        bus,
        dispatcher,
        trace_store,
    } = ctx;
    let mut inbox = bus
        .subscribe("rlp/ctrl")
        .await
        .map_err(|e| Error::Bus(e.to_string()))?;
    if let Some(tx) = ready {
        let _ = tx.send(());
    }
    let accepted: Arc<Mutex<HashMap<u128, RunAccepted>>> = Arc::new(Mutex::new(HashMap::new()));
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("control plane shutdown signal received");
                break;
            }
            maybe_msg = inbox.next() => {
                let Some(msg) = maybe_msg else {
                    break;
                };
                if msg.header.schema_id != CT_CTRL_REQ {
                    continue;
                }
                let trace_key = msg.header.trace_id;
                match decode_payload::<ControlRequest>(CT_CTRL_REQ, &msg.body) {
                    Ok(env) => {
                        match env.payload {
                            ControlRequest::RunSubmit(RunSubmitRequest {
                                request_id,
                                opening_yaml,
                                agent_digests,
                            }) => {
                                let req_id = request_id;
                                let req_key = uuid_to_u128(req_id.0);
                                let existing_accept = {
                                    let guard = accepted.lock().unwrap();
                                    guard.get(&req_key).cloned()
                                };
                                if let Some(acc) = existing_accept {
                                    // Re-send acceptance (idempotent)
                                    let header = build_header(CT_CTRL_RESP, trace_key, next_msg_id());
                                    let body = encode_payload(
                                        CT_CTRL_RESP,
                                        &ControlResponse::RunAccepted(acc.clone()),
                                        None,
                                    ).map_err(|e| Error::Rmp(e.to_string()))?;
                                    let frame = Message::new(header, Bytes::from(body))
                                        .map_err(|e| Error::Bus(e.to_string()))?;
                                    let _ = bus.publish("rlp/ctrl", frame).await;
                                    continue;
                                }
                                // Start the run
                                match handle_run_submit(
                                    RunSubmitContext {
                                        registry: registry.as_ref(),
                                        bus: &bus,
                                        dispatcher: dispatcher.clone(),
                                        accepted_map: accepted.clone(),
                                        trace_store: trace_store.clone(),
                                    },
                                    req_id,
                                    &opening_yaml,
                                    agent_digests,
                                    req_key,
                                )
                                .await
                                {
                                    Ok(_) => {}
                                    Err(err) => {
                                        let reason = format!("{err}");
                                        let resp = ControlResponse::RunRejected {
                                            request_id: req_id,
                                            reason,
                                        };
                                        let header = build_header(CT_CTRL_RESP, trace_key, next_msg_id());
                                        let body = encode_payload(CT_CTRL_RESP, &resp, None).map_err(|e| Error::Rmp(e.to_string()))?;
                                        let frame = Message::new(header, Bytes::from(body))
                                            .map_err(|e| Error::Bus(e.to_string()))?;
                                        let _ = bus.publish("rlp/ctrl", frame).await;
                                    }
                                }
                            }
                            ControlRequest::RunCancel(_cancel) => {
                                // MVP: cancellation not implemented
                                warn!("run cancel requested but not implemented in MVP");
                            }
                            ControlRequest::DescribeAgents(DescribeAgentsRequest {
                                request_id,
                                agents,
                            }) => {
                                if let Err(err) = handle_describe_agents(
                                    registry.as_ref(),
                                    &bus,
                                    trace_key,
                                    request_id,
                                    agents,
                                )
                                .await
                                {
                                    warn!("describe agents request failed: {err}");
                                }
                            }
                        }
                    }
                    Err(err) => warn!("failed to decode ctrl request: {}", err),
                }
            }
        }
    }
    Ok(())
}

async fn handle_describe_agents(
    registry: &AgentRegistry,
    bus: &Bus,
    trace_key: u128,
    request_id: TraceId,
    agents: Vec<AgentRef>,
) -> Result<(), Error> {
    let response = match registry.describe_many(agents.iter()) {
        Ok(described) => ControlResponse::AgentsDescribed {
            request_id,
            agents: described,
        },
        Err(err) => ControlResponse::AgentsDescribeFailed {
            request_id,
            reason: err.to_string(),
        },
    };
    let header = build_header(CT_CTRL_RESP, trace_key, next_msg_id());
    let body =
        encode_payload(CT_CTRL_RESP, &response, None).map_err(|e| Error::Rmp(e.to_string()))?;
    let frame = Message::new(header, Bytes::from(body)).map_err(|e| Error::Bus(e.to_string()))?;
    bus.publish("rlp/ctrl", frame)
        .await
        .map_err(|e| Error::Bus(e.to_string()))
}
