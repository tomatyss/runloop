use bytes::Bytes;
use runloop_agent_registry::AgentRegistry;
use runloop_bus::{Bus, Message};
use runloop_core::content::{CT_CTRL_RESP, CT_RUN_EVENT};
use runloop_core::{AgentDigest, ControlResponse, Error, OpeningId, RunAccepted, TraceId};
use runloop_kb::{NodeFinishedRecord, TraceStore};
use runloop_openings::{LadderHop, NodeState, RunEvent, RunTrace, Runner, parse_opening_str};
use runloop_rmp::{Header, encode_payload};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::warn;

use crate::executor_bus::{AgentDispatcher, BusExecutor};
use crate::utils::{build_header, current_millis, next_msg_id, uuid_to_u128};

pub struct RunSubmitContext<'a> {
    pub registry: &'a AgentRegistry,
    pub bus: &'a Bus,
    pub dispatcher: Arc<AgentDispatcher>,
    pub accepted_map: Arc<Mutex<HashMap<u128, RunAccepted>>>,
    pub trace_store: TraceStore,
}

pub async fn handle_run_submit(
    ctx: RunSubmitContext<'_>,
    request_id: TraceId,
    opening_yaml: &str,
    agent_digests: Vec<AgentDigest>,
    req_key: u128,
) -> Result<RunAccepted, Error> {
    RunLauncher::new(ctx)
        .launch(request_id, opening_yaml, agent_digests, req_key)
        .await
}

pub struct RunLauncher<'a> {
    registry: &'a AgentRegistry,
    trace_store: TraceStore,
    bus: &'a Bus,
    dispatcher: Arc<AgentDispatcher>,
    accepted_map: Arc<Mutex<HashMap<u128, RunAccepted>>>,
}

impl<'a> RunLauncher<'a> {
    pub fn new(ctx: RunSubmitContext<'a>) -> Self {
        Self {
            registry: ctx.registry,
            trace_store: ctx.trace_store,
            bus: ctx.bus,
            dispatcher: ctx.dispatcher,
            accepted_map: ctx.accepted_map,
        }
    }

    pub async fn launch(
        &self,
        request_id: TraceId,
        opening_yaml: &str,
        agent_digests: Vec<AgentDigest>,
        req_key: u128,
    ) -> Result<RunAccepted, Error> {
        let repository = RunRepository::new(self.trace_store.clone());
        let session = self.prepare_session(opening_yaml, &agent_digests)?;
        let accepted = session.acceptance(request_id);
        repository
            .record_started(&accepted.trace_id, &accepted.opening_id)
            .map_err(|e| Error::Kb(e.to_string()))?;
        if let Err(err) = self.accept_request(&accepted).await {
            repository.record_failed_start(&accepted.trace_id, &accepted.opening_id);
            return Err(err);
        }
        self.insert_acceptance(req_key, &accepted);
        self.spawn_runner(session, req_key, repository.clone());
        Ok(accepted)
    }

    fn prepare_session(
        &self,
        opening_yaml: &str,
        agent_digests: &[AgentDigest],
    ) -> Result<RunSession, Error> {
        let opening = parse_opening_str(opening_yaml).map_err(|e| Error::Opening(e.to_string()))?;
        if !agent_digests.is_empty() {
            verify_agent_digests(self.registry, &opening, agent_digests)?;
        }
        let opening_name = opening.name.clone();
        let runner = Runner::new(opening, self.build_executor());
        let trace_id = runner.trace_id();
        let opening_id = runner.opening_id();
        let (tx, rx) = mpsc::unbounded_channel();
        let runner = runner.with_event_tx(tx);
        Ok(RunSession {
            runner,
            trace_id,
            opening_id,
            opening_name,
            events: rx,
        })
    }

    async fn accept_request(&self, accepted: &RunAccepted) -> Result<(), Error> {
        let response = ControlResponse::RunAccepted(accepted.clone());
        let header = build_header(
            CT_CTRL_RESP,
            uuid_to_u128(accepted.request_id.0),
            next_msg_id(),
        );
        let body =
            encode_payload(CT_CTRL_RESP, &response, None).map_err(|e| Error::Rmp(e.to_string()))?;
        let frame =
            Message::new(header, Bytes::from(body)).map_err(|e| Error::Bus(e.to_string()))?;
        self.bus
            .publish("rlp/ctrl", frame)
            .await
            .map_err(|e| Error::Bus(e.to_string()))
    }

    fn spawn_runner(&self, session: RunSession, req_key: u128, repository: RunRepository) {
        let bus = self.bus.clone();
        let streamer = RunStreamer::new(bus, repository, self.accepted_map.clone());
        streamer.spawn(session, req_key);
    }

    fn build_executor(&self) -> Arc<BusExecutor> {
        Arc::new(BusExecutor::new(self.bus.clone(), self.dispatcher.clone()))
    }

    fn insert_acceptance(&self, req_key: u128, accepted: &RunAccepted) {
        let mut guard = match self.accepted_map.lock() {
            Ok(guard) => guard,
            Err(_) => {
                warn!("accepted_map poisoned; skipping idempotency insert");
                return;
            }
        };
        guard.insert(req_key, accepted.clone());
    }
}

pub struct RunSession {
    runner: Runner<BusExecutor>,
    trace_id: TraceId,
    opening_id: OpeningId,
    opening_name: String,
    events: mpsc::UnboundedReceiver<RunEvent>,
}

impl RunSession {
    fn acceptance(&self, request_id: TraceId) -> RunAccepted {
        RunAccepted {
            request_id,
            trace_id: self.trace_id,
            opening_id: self.opening_id,
            opening_name: self.opening_name.clone(),
        }
    }

    fn topic(&self) -> String {
        format!("rlp/runs/{}/events", self.trace_id)
    }

    fn trace_key(&self) -> u128 {
        uuid_to_u128(self.trace_id.0)
    }
}

#[derive(Clone)]
pub struct RunRepository {
    trace_store: TraceStore,
}

impl RunRepository {
    pub fn new(trace_store: TraceStore) -> Self {
        Self { trace_store }
    }

    fn record_started(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
    ) -> Result<(), runloop_kb::Error> {
        self.trace_store.record_run_started(trace_id, opening_id)
    }

    fn record_finished(&self, trace_id: &TraceId, opening_id: &OpeningId, status: &str) {
        if let Err(err) = self
            .trace_store
            .record_run_finished(trace_id, opening_id, status)
        {
            warn!(%err, "failed to persist run.finished");
        }
    }

    fn record_failed_start(&self, trace_id: &TraceId, opening_id: &OpeningId) {
        if let Err(err) = self
            .trace_store
            .record_run_finished(trace_id, opening_id, "failed")
        {
            warn!(%err, "failed to persist failed run start");
        }
    }

    fn record_nodes(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        records: &[NodeFinishedRecord],
    ) {
        if let Err(err) = self.trace_store.record_nodes(trace_id, opening_id, records) {
            warn!(%err, "failed to persist node summaries");
        }
    }

    fn record_run_trace(&self, trace: &RunTrace) {
        if let Err(err) = self.trace_store.record_run_trace(trace) {
            warn!(%err, "failed to persist run trace");
        }
    }
}

pub struct RunStreamer {
    bus: Bus,
    repository: RunRepository,
    accepted_map: Arc<Mutex<HashMap<u128, RunAccepted>>>,
}

impl RunStreamer {
    pub fn new(
        bus: Bus,
        repository: RunRepository,
        accepted_map: Arc<Mutex<HashMap<u128, RunAccepted>>>,
    ) -> Self {
        Self {
            bus,
            repository,
            accepted_map,
        }
    }

    pub fn spawn(self, session: RunSession, req_key: u128) {
        tokio::spawn(async move { self.pump(session, req_key).await });
    }

    async fn pump(self, session: RunSession, req_key: u128) {
        let RunStreamer {
            bus,
            repository,
            accepted_map,
        } = self;
        let topic = session.topic();
        let trace_key = session.trace_key();
        let RunSession {
            runner,
            trace_id,
            opening_id,
            opening_name: _,
            mut events,
        } = session;
        let mut run_future = Box::pin(async move { runner.run().await });
        let composer = EventComposer::new(trace_id, opening_id);
        let mut tracker = NodeTracker::new();
        let mut ladder = LadderRecorder::new("runloopd");
        tokio::task::yield_now().await;
        sleep(Duration::from_millis(20)).await;
        let _ = publish_to_topic(
            &bus,
            &topic,
            trace_key,
            composer.started(),
            Some(&mut ladder),
        )
        .await;
        loop {
            tokio::select! {
                res = &mut run_future => {
                    match res {
                        Ok(report) => {
                            let mut trace = report.trace;
                            let success = trace.success;
                            let payload = composer.finished(success);
                            let _ = publish_to_topic(&bus, &topic, trace_key, payload, Some(&mut ladder)).await;
                            trace.ladder = ladder.take();
                            let nodes = node_records_from_trace(&trace);
                            repository.record_nodes(composer.trace_id(), composer.opening_id(), &nodes);
                            repository.record_run_trace(&trace);
                            repository.record_finished(composer.trace_id(), composer.opening_id(), if success { "finished" } else { "failed" });
                        }
                        Err(err) => {
                            let payload = composer.failure(&err.to_string());
                            let _ = publish_to_topic(&bus, &topic, trace_key, payload, Some(&mut ladder)).await;
                            let failure_records = tracker.summarize();
                            repository.record_nodes(composer.trace_id(), composer.opening_id(), &failure_records);
                            repository.record_finished(composer.trace_id(), composer.opening_id(), "failed");
                        }
                    }
                    break;
                }
                maybe_event = events.recv() => {
                    match maybe_event {
                        Some(event) => {
                            tracker.handle(&event);
                            if let Some(payload) = composer.event_payload(event) {
                                let _ = publish_to_topic(&bus, &topic, trace_key, payload, Some(&mut ladder)).await;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        let _ = accepted_map.lock().map(|mut guard| guard.remove(&req_key));
    }
}

pub struct EventComposer {
    trace_id: TraceId,
    opening_id: OpeningId,
}

impl EventComposer {
    pub fn new(trace_id: TraceId, opening_id: OpeningId) -> Self {
        Self {
            trace_id,
            opening_id,
        }
    }

    pub fn trace_id(&self) -> &TraceId {
        &self.trace_id
    }

    pub fn opening_id(&self) -> &OpeningId {
        &self.opening_id
    }

    pub fn started(&self) -> serde_json::Value {
        self.status_payload("info", "run started".into(), None)
    }

    pub fn finished(&self, success: bool) -> serde_json::Value {
        let level = if success { "info" } else { "error" };
        let message = if success {
            "run ok".to_string()
        } else {
            "run error".to_string()
        };
        self.status_payload(level, message, Some(success))
    }

    pub fn failure(&self, err: &str) -> serde_json::Value {
        self.status_payload("error", format!("run failed: {err}"), Some(false))
    }

    pub fn event_payload(&self, event: RunEvent) -> Option<serde_json::Value> {
        match event {
            RunEvent::NodeState {
                node_id,
                state,
                attempt,
            } => Some(self.node_state(node_id, state, attempt)),
            RunEvent::LogLine {
                node_id,
                level,
                message,
            } => Some(self.log_line(node_id, level, message)),
            RunEvent::TraceLine { line } => Some(self.trace_line(line)),
            RunEvent::Completed { .. } => None,
        }
    }

    fn node_state(
        &self,
        node_id: String,
        state: runloop_openings::NodeState,
        attempt: u32,
    ) -> serde_json::Value {
        json!({
            "kind": "plan",
            "message": format!("node {node_id} state update"),
            "meta": {
                "ts_ms": current_millis(),
                "run_id": self.trace_id.to_string(),
                "node_id": node_id,
                "attempt": attempt,
                "state": format!("{state:?}")
            }
        })
    }

    fn log_line(&self, node_id: String, level: String, message: String) -> serde_json::Value {
        json!({
            "kind": "log",
            "level": level,
            "message": message,
            "meta": {
                "ts_ms": current_millis(),
                "run_id": self.trace_id.to_string(),
                "node_id": node_id
            }
        })
    }

    fn trace_line(&self, line: String) -> serde_json::Value {
        json!({
            "kind": "trace",
            "level": "info",
            "message": line,
            "meta": {
                "ts_ms": current_millis(),
                "run_id": self.trace_id.to_string()
            }
        })
    }

    fn status_payload(
        &self,
        level: &'static str,
        message: String,
        success: Option<bool>,
    ) -> serde_json::Value {
        let mut meta = json!({
            "ts_ms": current_millis(),
            "run_id": self.trace_id.to_string(),
            "opening_id": self.opening_id.to_string()
        });
        if let Some(success) = success {
            meta["success"] = serde_json::Value::Bool(success);
        }
        json!({
            "kind": "status",
            "level": level,
            "message": message,
            "meta": meta
        })
    }
}

pub struct NodeTracker {
    nodes: HashMap<String, NodeTelemetry>,
}

impl NodeTracker {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    pub fn handle(&mut self, event: &RunEvent) {
        if let RunEvent::NodeState {
            node_id,
            state,
            attempt,
        } = event
        {
            let telemetry = self
                .nodes
                .entry(node_id.clone())
                .or_insert_with(NodeTelemetry::new);
            telemetry.attempt = *attempt;
            match state {
                NodeState::Running => {
                    telemetry.status = Some("running".into());
                    telemetry.start_ts = Some(current_millis());
                    telemetry.end_ts = None;
                    telemetry.error = None;
                }
                NodeState::Succeeded => {
                    telemetry.status = Some("ok".into());
                    telemetry.end_ts = Some(current_millis());
                }
                NodeState::Failed { reason } => {
                    telemetry.status = Some("error".into());
                    telemetry.error = Some(reason.clone());
                    telemetry.end_ts = Some(current_millis());
                }
                NodeState::Skipped => {
                    telemetry.status = Some("skipped".into());
                    telemetry.end_ts = Some(current_millis());
                }
                NodeState::Cancelled => {
                    telemetry.status = Some("cancelled".into());
                    telemetry.end_ts = Some(current_millis());
                }
                NodeState::Pending => {}
            }
        }
    }

    pub fn summarize(&self) -> Vec<NodeFinishedRecord> {
        self.nodes
            .iter()
            .map(|(node_id, telemetry)| NodeFinishedRecord {
                node_id: node_id.clone(),
                status: telemetry.status.clone().unwrap_or_else(|| "pending".into()),
                attempt: telemetry.attempt,
                duration_ms: telemetry.duration_ms(),
                outputs_hash: None,
                error: telemetry.error.clone(),
            })
            .collect()
    }
}

struct NodeTelemetry {
    attempt: u32,
    start_ts: Option<u64>,
    end_ts: Option<u64>,
    status: Option<String>,
    error: Option<String>,
}

impl NodeTelemetry {
    fn new() -> Self {
        Self {
            attempt: 0,
            start_ts: None,
            end_ts: None,
            status: None,
            error: None,
        }
    }

    fn duration_ms(&self) -> u64 {
        match (self.start_ts, self.end_ts) {
            (Some(start), Some(end)) if end >= start => end - start,
            _ => 0,
        }
    }
}

fn status_from_state(state: &NodeState) -> String {
    match state {
        NodeState::Succeeded => "ok",
        NodeState::Failed { .. } => "error",
        NodeState::Skipped => "skipped",
        NodeState::Cancelled => "cancelled",
        NodeState::Pending => "pending",
        NodeState::Running => "running",
    }
    .into()
}

pub fn verify_agent_digests(
    registry: &AgentRegistry,
    opening: &runloop_openings::Opening,
    expected: &[AgentDigest],
) -> Result<(), Error> {
    let refs = opening.agent_refs();
    let descriptors = registry
        .describe_many(refs.iter())
        .map_err(|err| Error::Config(err.to_string()))?;
    let mut actual = BTreeMap::new();
    for descriptor in descriptors {
        actual.insert(descriptor.reference.clone(), descriptor.digest.clone());
    }
    if actual.len() != expected.len() {
        return Err(Error::Config(
            "agent digest count mismatch between CLI and daemon".into(),
        ));
    }
    for digest in expected {
        match actual.get(&digest.reference) {
            Some(current) if current == &digest.digest => {}
            Some(current) => {
                return Err(Error::Config(format!(
                    "agent '{}' digest mismatch (cli {}, daemon {})",
                    digest.reference, digest.digest, current
                )));
            }
            None => {
                return Err(Error::Config(format!(
                    "agent '{}' missing on daemon",
                    digest.reference
                )));
            }
        }
    }
    Ok(())
}

async fn publish_to_topic(
    bus: &Bus,
    topic: &str,
    trace_key: u128,
    payload: serde_json::Value,
    ladder: Option<&mut LadderRecorder>,
) -> Result<(), Error> {
    let kind = payload
        .get("kind")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let header = Header {
        schema_id: CT_RUN_EVENT,
        created_at_ms: current_millis(),
        trace_id: trace_key,
        msg_id: next_msg_id(),
        ..Header::default()
    };
    let body =
        encode_payload(CT_RUN_EVENT, &payload, None).map_err(|e| Error::Rmp(e.to_string()))?;
    if let Some(recorder) = ladder {
        recorder.record(topic, &header, body.len(), kind.as_deref());
    }
    let msg = Message::new(header, Bytes::from(body)).map_err(|e| Error::Bus(e.to_string()))?;
    bus.publish(topic, msg)
        .await
        .map_err(|e| Error::Bus(e.to_string()))
}

fn node_records_from_trace(trace: &RunTrace) -> Vec<NodeFinishedRecord> {
    trace
        .nodes
        .iter()
        .map(|node| {
            let (attempt, outputs_hash, error) = match node.final_attempt.as_ref() {
                Some(attempt) => (
                    attempt.attempt,
                    attempt.output_hash.clone(),
                    attempt.error.clone(),
                ),
                None => (0, None, None),
            };
            NodeFinishedRecord {
                node_id: node.node_id.clone(),
                status: status_from_state(&node.state),
                attempt,
                duration_ms: 0,
                outputs_hash,
                error,
            }
        })
        .collect()
}

pub struct LadderRecorder {
    entries: Vec<LadderHop>,
    from: String,
}

impl LadderRecorder {
    pub fn new(from: impl Into<String>) -> Self {
        Self {
            entries: Vec::new(),
            from: from.into(),
        }
    }

    pub fn record(&mut self, topic: &str, header: &Header, body_len: usize, kind: Option<&str>) {
        let frame_len = 64u32.saturating_add(body_len as u32);
        self.entries.push(LadderHop {
            ts_ms: header.created_at_ms,
            topic: topic.to_string(),
            schema_id: header.schema_id,
            frame_len,
            body_len: body_len as u32,
            from: self.from.clone(),
            to: topic.to_string(),
            msg_id: header.msg_id,
            kind: kind.map(|s| s.to_string()),
        });
    }

    pub fn take(self) -> Vec<LadderHop> {
        self.entries
    }
}
