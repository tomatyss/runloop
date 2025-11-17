mod executor_bus;

use async_trait::async_trait;
use bytes::Bytes;
use executor_bus::{AgentDispatcher, BusExecutor};
use futures_util::StreamExt;
use runloop_agent_registry::AgentRegistry;
use runloop_agents_common::{ActionDecision, ActionProposal, AgentResult, ConfirmationProvider};
use runloop_bus::{Bus, BusServerHandle, Message, PublisherKind};
use runloop_core::content::{CT_CTRL_REQ, CT_CTRL_RESP, CT_RUN_EVENT};
use runloop_core::{AgentDigest, AgentRef, Config, Error, OpeningId, TraceId};
use runloop_core::{
    ControlRequest, ControlResponse, DescribeAgentsRequest, RunAccepted, RunSubmitRequest,
};
use runloop_executor_local::{ExecutorInitError, build_executor};
use runloop_kb::{KnowledgeBase, Materializer, NodeFinishedRecord, TraceStore};
use runloop_model_broker::SecretResolver;
use runloop_openings::{NodeState, RunEvent, RunTrace, Runner, parse_opening_str};
use runloop_rmp::{Header, decode_payload, encode_payload};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use tempfile::tempdir;
use tokio::signal;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, sleep};

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();
    tracing::info!("runloopd starting");

    let config = Config::load()?;
    let registry = Arc::new(AgentRegistry::new(config.agents.search_dirs.clone()));
    let bus_path = bus_socket_path(&config)?;
    let mut bus_server = start_bus(bus_path.as_path(), &config).await?;

    let kb = KnowledgeBase::open(&config.kb).map_err(|err| Error::Kb(err.to_string()))?;
    kb.migrate()
        .map_err(|err| Error::Kb(format!("migration failed: {err}")))?;
    let trace_store = TraceStore::from_kb(kb.clone(), "agent:runloopd", Some("system".into()));

    let materializer = Materializer::new(kb.clone());

    tokio::task::spawn_blocking({
        let materializer = materializer.clone();
        move || -> Result<(), runloop_kb::Error> {
            while materializer.sync()? {}
            Ok(())
        }
    })
    .await
    .map_err(|err| Error::Kb(format!("materializer startup join error: {err}")))?
    .map_err(|err| Error::Kb(format!("materializer catch-up failed: {err}")))?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let mat_worker = tokio::spawn(run_materializer(materializer, shutdown_rx));

    let confirmation = Arc::new(DaemonConfirmationProvider::new(
        config.security.confirm_external_actions,
    ));
    let secrets: Arc<dyn SecretResolver> = Arc::new(DaemonSecretResolver);
    let local_executor =
        build_executor(config.clone(), confirmation, secrets).map_err(|e| match e {
            ExecutorInitError::Config(err) => err,
            other => Error::Runtime(other.to_string()),
        })?;

    // Connect as daemon publisher
    let bus = Bus::connect_as(bus_path.as_path(), PublisherKind::Agent)
        .await
        .map_err(|e| Error::Bus(e.to_string()))?;

    let dispatcher = Arc::new(AgentDispatcher::new(bus.clone(), local_executor));

    // Spawn control-plane loop
    let (ctrl_shutdown_tx, ctrl_shutdown_rx) = oneshot::channel();
    let ctrl_ctx = ControlPlaneCtx {
        config: config.clone(),
        registry: registry.clone(),
        bus: bus.clone(),
        dispatcher: dispatcher.clone(),
        trace_store: trace_store.clone(),
    };
    let ctrl_task = tokio::spawn(run_control_plane_with_ready(
        ctrl_ctx,
        ctrl_shutdown_rx,
        None,
    ));

    wait_for_shutdown().await;
    tracing::info!("shutdown signal received; stopping services");
    let _ = shutdown_tx.send(());
    let _ = ctrl_shutdown_tx.send(());
    if let Err(err) = mat_worker.await {
        tracing::warn!("materializer task ended unexpectedly: {err}");
    }
    match ctrl_task.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!("control plane task returned error: {err}");
            return Err(err);
        }
        Err(join_err) => {
            tracing::warn!("control plane task join error: {join_err}");
        }
    }
    dispatcher.shutdown().await;
    drop(bus);
    bus_server.close();

    Ok(())
}

async fn wait_for_shutdown() {
    tracing::info!("press Ctrl+C to stop runloopd");
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("unable to install Ctrl+C handler");
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = sleep(Duration::from_secs(u64::MAX)) => {}
    }
}

async fn run_materializer(materializer: Materializer, mut shutdown: oneshot::Receiver<()>) {
    tracing::info!("materializer loop started");
    let mut idle_backoff = Duration::from_millis(200);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("materializer loop stopping");
                break;
            }
            result = tokio::task::spawn_blocking({
                let materializer = materializer.clone();
                move || materializer.sync()
            }) => {
                match result {
                    Ok(Ok(true)) => {
                        idle_backoff = Duration::from_millis(50);
                    }
                    Ok(Ok(false)) => {
                        idle_backoff = (idle_backoff * 2).min(Duration::from_secs(5));
                        sleep(idle_backoff).await;
                    }
                    Ok(Err(err)) => {
                        tracing::error!("materializer sync failed: {err}");
                        sleep(Duration::from_secs(1)).await;
                    }
                    Err(join_err) => {
                        tracing::error!("materializer task panicked: {join_err}");
                        break;
                    }
                }
            }
        }
    }
}

fn bus_socket_path(config: &Config) -> Result<PathBuf, Error> {
    if let Some(path) = config.runtime.socket_path.as_deref() {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(Error::Config(
                "runtime.socket_path cannot be empty when specified".into(),
            ));
        }
        return Ok(PathBuf::from(trimmed));
    }
    let dir = config.runtime.sockets_dir.trim();
    if dir.is_empty() {
        return Err(Error::Config(
            "runtime.sockets_dir cannot be empty when runtime.socket_path is unset".into(),
        ));
    }
    Ok(PathBuf::from(dir).join("rmp.sock"))
}

async fn start_bus(socket_path: &Path, config: &Config) -> Result<BusServerHandle, Error> {
    let handle = Bus::bind(socket_path).await.map_err(|err| {
        Error::Bus(format!(
            "failed to bind bus at {}: {err}",
            socket_path.display()
        ))
    })?;
    let allowed = action_decision_acl(&config.bus.auth.publishers.action_decision.allowed_kinds)?;
    handle.configure_action_decision_acl(allowed.clone());
    log_action_decision_acl(socket_path, &allowed);
    Ok(handle)
}

fn action_decision_acl(kinds: &[String]) -> Result<Vec<PublisherKind>, Error> {
    let mut allowed = Vec::new();
    for raw in kinds {
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(Error::Config(
                "empty publisher kind entry in bus.auth.publishers.action_decision.allowed_kinds"
                    .into(),
            ));
        }
        let normalized = normalized.to_ascii_lowercase();
        let kind = match normalized.as_str() {
            "ui" => PublisherKind::Ui,
            "tui" => PublisherKind::Tui,
            "agent" => PublisherKind::Agent,
            other => {
                return Err(Error::Config(format!(
                    "unknown publisher kind '{other}' in bus.auth.publishers.action_decision.allowed_kinds"
                )));
            }
        };
        if !allowed.contains(&kind) {
            allowed.push(kind);
        }
    }
    Ok(allowed)
}

fn log_action_decision_acl(path: &Path, allowed: &[PublisherKind]) {
    if allowed.is_empty() {
        tracing::warn!(
            path = %path.display(),
            "bus listening; no publishers permitted to emit action.decision"
        );
        return;
    }
    let labels: Vec<&'static str> = allowed.iter().map(publisher_kind_label).collect();
    tracing::info!(path = %path.display(), allowed = %labels.join(","), "bus listening");
}

fn publisher_kind_label(kind: &PublisherKind) -> &'static str {
    match kind {
        PublisherKind::Ui => "ui",
        PublisherKind::Tui => "tui",
        PublisherKind::Agent => "agent",
    }
}

struct ControlPlaneCtx {
    config: Config,
    registry: Arc<AgentRegistry>,
    bus: Bus,
    dispatcher: Arc<AgentDispatcher>,
    trace_store: TraceStore,
}

async fn run_control_plane_with_ready(
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
                tracing::info!("control plane shutdown signal received");
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
                                    )?;
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
                                        let body = encode_payload(CT_CTRL_RESP, &resp, None)?;
                                        let frame = Message::new(header, Bytes::from(body))
                                            .map_err(|e| Error::Bus(e.to_string()))?;
                                        let _ = bus.publish("rlp/ctrl", frame).await;
                                    }
                                }
                            }
                            ControlRequest::RunCancel(_cancel) => {
                                // MVP: cancellation not implemented
                                tracing::warn!("run cancel requested but not implemented in MVP");
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
                                    tracing::warn!("describe agents request failed: {err}");
                                }
                            }
                        }
                    }
                    Err(err) => tracing::warn!("failed to decode ctrl request: {}", err),
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
    let body = encode_payload(CT_CTRL_RESP, &response, None)?;
    let frame = Message::new(header, Bytes::from(body)).map_err(|e| Error::Bus(e.to_string()))?;
    bus.publish("rlp/ctrl", frame)
        .await
        .map_err(|e| Error::Bus(e.to_string()))
}

struct RunSubmitContext<'a> {
    registry: &'a AgentRegistry,
    bus: &'a Bus,
    dispatcher: Arc<AgentDispatcher>,
    accepted_map: Arc<Mutex<HashMap<u128, RunAccepted>>>,
    trace_store: TraceStore,
}

struct RunSession {
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

struct RunLauncher<'a> {
    registry: &'a AgentRegistry,
    trace_store: TraceStore,
    bus: &'a Bus,
    dispatcher: Arc<AgentDispatcher>,
    accepted_map: Arc<Mutex<HashMap<u128, RunAccepted>>>,
}

impl<'a> RunLauncher<'a> {
    fn new(ctx: RunSubmitContext<'a>) -> Self {
        Self {
            registry: ctx.registry,
            trace_store: ctx.trace_store,
            bus: ctx.bus,
            dispatcher: ctx.dispatcher,
            accepted_map: ctx.accepted_map,
        }
    }

    async fn launch(
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
        let body = encode_payload(CT_CTRL_RESP, &response, None)?;
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
                tracing::warn!("accepted_map poisoned; skipping idempotency insert");
                return;
            }
        };
        guard.insert(req_key, accepted.clone());
    }
}

#[derive(Clone)]
struct RunRepository {
    trace_store: TraceStore,
}

impl RunRepository {
    fn new(trace_store: TraceStore) -> Self {
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
            tracing::warn!(%err, "failed to persist run.finished");
        }
    }

    fn record_failed_start(&self, trace_id: &TraceId, opening_id: &OpeningId) {
        if let Err(err) = self
            .trace_store
            .record_run_finished(trace_id, opening_id, "failed")
        {
            tracing::warn!(%err, "failed to persist failed run start");
        }
    }

    fn record_nodes(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        records: &[NodeFinishedRecord],
    ) {
        if let Err(err) = self
            .trace_store
            .record_nodes(trace_id, opening_id, records)
        {
            tracing::warn!(%err, "failed to persist node summaries");
        }
    }

    fn record_run_trace(&self, trace: &RunTrace) {
        if let Err(err) = self.trace_store.record_run_trace(trace) {
            tracing::warn!(%err, "failed to persist run trace");
        }
    }
}

struct RunStreamer {
    bus: Bus,
    repository: RunRepository,
    accepted_map: Arc<Mutex<HashMap<u128, RunAccepted>>>,
}

impl RunStreamer {
    fn new(
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

    fn spawn(self, session: RunSession, req_key: u128) {
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
        tokio::task::yield_now().await;
        sleep(Duration::from_millis(20)).await;
        let _ = publish_to_topic(&bus, &topic, trace_key, composer.started()).await;
        loop {
            tokio::select! {
                res = &mut run_future => {
                    match res {
                        Ok(report) => {
                            let success = report.trace.success;
                            let payload = composer.finished(success);
                            let _ = publish_to_topic(&bus, &topic, trace_key, payload).await;
                            let nodes = node_records_from_trace(&report.trace);
                            repository.record_nodes(composer.trace_id(), composer.opening_id(), &nodes);
                            repository.record_run_trace(&report.trace);
                            repository.record_finished(composer.trace_id(), composer.opening_id(), if success { "finished" } else { "failed" });
                        }
                        Err(err) => {
                            let payload = composer.failure(&err.to_string());
                            let _ = publish_to_topic(&bus, &topic, trace_key, payload).await;
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
                                let _ = publish_to_topic(&bus, &topic, trace_key, payload).await;
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

struct EventComposer {
    trace_id: TraceId,
    opening_id: OpeningId,
}

impl EventComposer {
    fn new(trace_id: TraceId, opening_id: OpeningId) -> Self {
        Self {
            trace_id,
            opening_id,
        }
    }

    fn trace_id(&self) -> &TraceId {
        &self.trace_id
    }

    fn opening_id(&self) -> &OpeningId {
        &self.opening_id
    }

    fn started(&self) -> serde_json::Value {
        self.status_payload("info", "run started".into(), None)
    }

    fn finished(&self, success: bool) -> serde_json::Value {
        let level = if success { "info" } else { "error" };
        let message = if success {
            "run ok".to_string()
        } else {
            "run error".to_string()
        };
        self.status_payload(level, message, Some(success))
    }

    fn failure(&self, err: &str) -> serde_json::Value {
        self.status_payload("error", format!("run failed: {err}"), Some(false))
    }

    fn event_payload(&self, event: RunEvent) -> Option<serde_json::Value> {
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
        serde_json::json!({
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
        serde_json::json!({
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
        serde_json::json!({
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
        let mut meta = serde_json::json!({
            "ts_ms": current_millis(),
            "run_id": self.trace_id.to_string(),
            "opening_id": self.opening_id.to_string()
        });
        if let Some(success) = success {
            meta["success"] = serde_json::Value::Bool(success);
        }
        serde_json::json!({
            "kind": "status",
            "level": level,
            "message": message,
            "meta": meta
        })
    }
}

async fn handle_run_submit(
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


fn verify_agent_digests(
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
) -> Result<(), Error> {
    let header = Header {
        schema_id: CT_RUN_EVENT,
        created_at_ms: current_millis(),
        trace_id: trace_key,
        msg_id: next_msg_id(),
        ..Header::default()
    };
    let body = encode_payload(CT_RUN_EVENT, &payload, None)?;
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

struct NodeTracker {
    nodes: HashMap<String, NodeTelemetry>,
}

impl NodeTracker {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    fn handle(&mut self, event: &RunEvent) {
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

    fn summarize(&self) -> Vec<NodeFinishedRecord> {
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

fn build_header(schema_id: u16, trace_id: u128, msg_id: u64) -> Header {
    Header {
        schema_id,
        created_at_ms: current_millis(),
        ttl_ms: 30_000,
        trace_id,
        msg_id,
        ..Header::default()
    }
}

fn current_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn uuid_to_u128(id: uuid::Uuid) -> u128 {
    id.as_u128()
}

pub(crate) fn next_msg_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

struct DaemonSecretResolver;
impl SecretResolver for DaemonSecretResolver {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        std::env::var(secret_id).ok()
    }
}

struct DaemonConfirmationProvider {
    require: bool,
}
impl DaemonConfirmationProvider {
    fn new(require: bool) -> Self {
        Self { require }
    }
}

#[async_trait]
impl ConfirmationProvider for DaemonConfirmationProvider {
    async fn confirm(&self, _proposal: ActionProposal) -> AgentResult<ActionDecision> {
        if !self.require {
            return Ok(ActionDecision::approved(Some(
                "auto-approved (confirm_external_actions=false)".into(),
            )));
        }
        // Security: refuse automatic approval when confirmations are required in daemon mode
        Ok(ActionDecision::rejected(Some(
            "confirmation required (daemon lacks UI/TUI decision); refusing automatic approval"
                .into(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::config::{ModelProvider, ModelRoute, ProviderKind};

    fn fixture_registry_and_opening() -> (AgentRegistry, runloop_openings::Opening) {
        let agents_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("agents");
        let registry = AgentRegistry::new(vec![agents_dir.to_string_lossy().into_owned()]);
        let opening_yaml = r#"version: 0
name: digest-test
nodes:
  - id: a
    use: agent:writer
  - id: b
    use: agent:critic
edges:
  - from: a.out
    to: b.in
"#;
        let opening =
            parse_opening_str(opening_yaml).expect("opening fixture parses for digest tests");
        (registry, opening)
    }

    fn current_digests(
        registry: &AgentRegistry,
        opening: &runloop_openings::Opening,
    ) -> Vec<AgentDigest> {
        let refs = opening.agent_refs();
        registry
            .describe_many(refs.iter())
            .expect("describe succeeds")
            .into_iter()
            .map(|desc| AgentDigest {
                reference: desc.reference,
                digest: desc.digest,
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_accepts_and_streams_events() {
        let tmp = tempdir().expect("tmp");
        let sockets_dir = tmp.path().join("sock");
        let kb_root = tmp.path().join("kb");
        let workdir = tmp.path().join("work");
        std::fs::create_dir_all(&sockets_dir).unwrap();
        std::fs::create_dir_all(&kb_root).unwrap();
        std::fs::create_dir_all(&workdir).unwrap();

        let mut config = Config::default();
        config.runtime.sockets_dir = sockets_dir.to_string_lossy().into_owned();
        config.runtime.socket_path = None;
        config.runtime.workdir = workdir.to_string_lossy().into_owned();
        config.kb.root_dir = kb_root.to_string_lossy().into_owned();
        config.models.default = "null:test".into();
        config.models.broker.providers = vec![ModelProvider {
            id: "local".into(),
            kind: ProviderKind::Local,
            model_dir: None,
            base_url: None,
            secret_id: None,
            headers: Default::default(),
            schema: None,
        }];
        config.models.broker.route = vec![ModelRoute {
            pattern: "*".into(),
            provider: "local".into(),
            target_model: None,
        }];
        config.security.confirm_external_actions = false;
        let agents_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("agents");
        config.agents.search_dirs = vec![agents_dir.to_string_lossy().into_owned()];

        // Bind bus and launch control loop
        let path = bus_socket_path(&config).expect("bus path");
        let _server = Bus::bind(path.as_path()).await.expect("bind bus");
        let bus = Bus::connect_as(path.as_path(), PublisherKind::Agent)
            .await
            .expect("connect bus");
        let kb = KnowledgeBase::open(&config.kb).expect("open kb");
        kb.migrate().expect("migrate");
        let registry = Arc::new(AgentRegistry::new(config.agents.search_dirs.clone()));
        let trace_store = TraceStore::from_kb(kb.clone(), "agent:runloopd", Some("system".into()));
        let confirmation = Arc::new(DaemonConfirmationProvider::new(
            config.security.confirm_external_actions,
        ));
        let secrets: Arc<dyn SecretResolver> = Arc::new(DaemonSecretResolver);
        let local_executor =
            build_executor(config.clone(), confirmation, secrets).expect("build executor");
        let dispatcher = Arc::new(AgentDispatcher::new(bus.clone(), local_executor));
        let (ready_tx, ready_rx) = oneshot::channel();
        let (ctrl_shutdown_tx, ctrl_shutdown_rx) = oneshot::channel();
        let ctrl_ctx = ControlPlaneCtx {
            config: config.clone(),
            registry: registry.clone(),
            bus: bus.clone(),
            dispatcher: dispatcher.clone(),
            trace_store: trace_store.clone(),
        };
        let ctrl = tokio::spawn(run_control_plane_with_ready(
            ctrl_ctx,
            ctrl_shutdown_rx,
            Some(ready_tx),
        ));
        ready_rx.await.expect("control plane ready");

        // Subscribe to ctrl before submit
        let mut ctrl_sub = bus.subscribe("rlp/ctrl").await.expect("subscribe ctrl");

        // Submit RunSubmit
        let request_id = TraceId::new();
        let opening_yaml = r#"version: 0
name: unit
nodes:
  - id: a
    use: agent:writer
  - id: b
    use: agent:critic
edges:
  - from: a.out
    to: b.in
success:
  all_of:
    - b.ok == true
"#;
        let submit = ControlRequest::RunSubmit(RunSubmitRequest {
            request_id,
            opening_yaml: opening_yaml.into(),
            agent_digests: Vec::new(),
        });
        let mut header = Header::default();
        header.schema_id = CT_CTRL_REQ;
        header.created_at_ms = current_millis();
        header.ttl_ms = 30_000;
        header.trace_id = uuid_to_u128(request_id.0);
        header.msg_id = next_msg_id();
        let body = encode_payload(CT_CTRL_REQ, &submit, None).expect("encode");
        let msg = Message::new(header, Bytes::from(body)).expect("msg");
        bus.publish("rlp/ctrl", msg).await.expect("publish submit");

        // Wait for acceptance
        let mut accepted = None;
        while let Some(msg) = ctrl_sub.next().await {
            if msg.header.schema_id != CT_CTRL_RESP {
                continue;
            }
            if msg.header.trace_id != uuid_to_u128(request_id.0) {
                continue;
            }
            let decoded =
                decode_payload::<ControlResponse>(CT_CTRL_RESP, &msg.body).expect("decode resp");
            if let ControlResponse::RunAccepted(acc) = decoded.payload {
                accepted = Some(acc);
                break;
            }
            panic!("unexpected ctrl response");
        }
        let acc = accepted.expect("accepted");
        assert_eq!(acc.request_id, request_id);

        // Stream first run event
        let topic = format!("rlp/runs/{}/events", acc.trace_id);
        let mut ev_sub = bus.subscribe(&topic).await.expect("subscribe events");
        let first = tokio::time::timeout(Duration::from_millis(2000), ev_sub.next())
            .await
            .expect("event within timeout")
            .expect("some event");
        assert_eq!(first.header.schema_id, CT_RUN_EVENT);
        let env =
            decode_payload::<serde_json::Value>(CT_RUN_EVENT, &first.body).expect("decode ev");
        assert_eq!(
            env.payload.get("kind").and_then(|v| v.as_str()),
            Some("status")
        );

        let _ = ctrl_shutdown_tx.send(());
        ctrl.await
            .expect("ctrl task join")
            .expect("control plane finished cleanly");
        dispatcher.shutdown().await;
    }

    #[test]
    fn action_decision_acl_parses_known_kinds() {
        let kinds = vec!["tui".into(), "UI".into(), "agent".into(), "tui".into()];
        let acl = action_decision_acl(&kinds).expect("parsed kinds");
        assert_eq!(
            acl,
            vec![PublisherKind::Tui, PublisherKind::Ui, PublisherKind::Agent]
        );
    }

    #[test]
    fn action_decision_acl_rejects_blank_entries() {
        let kinds = vec![" ".into(), "".into()];
        let err = action_decision_acl(&kinds).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("empty publisher kind"));
    }

    #[test]
    fn action_decision_acl_rejects_unknown_values() {
        let kinds = vec!["foo".into()];
        let err = action_decision_acl(&kinds).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown publisher kind"));
    }

    #[test]
    fn verify_agent_digests_accepts_matching_values() {
        let (registry, opening) = fixture_registry_and_opening();
        let digests = current_digests(&registry, &opening);
        verify_agent_digests(&registry, &opening, &digests).expect("digests match");
    }

    #[test]
    fn verify_agent_digests_detects_digest_mismatch() {
        let (registry, opening) = fixture_registry_and_opening();
        let mut digests = current_digests(&registry, &opening);
        assert!(!digests.is_empty(), "fixture opening should include agents");
        digests[0].digest = "deadbeef".into();
        let err = verify_agent_digests(&registry, &opening, &digests).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("digest mismatch"));
    }

    #[test]
    fn verify_agent_digests_detects_missing_agent_reference() {
        let (registry, opening) = fixture_registry_and_opening();
        let mut digests = current_digests(&registry, &opening);
        assert!(!digests.is_empty(), "fixture opening should include agents");
        digests[0].reference = AgentRef::new("nonexistent", None);
        let err = verify_agent_digests(&registry, &opening, &digests).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing on daemon"));
    }

    #[test]
    fn verify_agent_digests_detects_length_mismatch() {
        let (registry, opening) = fixture_registry_and_opening();
        let mut digests = current_digests(&registry, &opening);
        digests.pop();
        let err = verify_agent_digests(&registry, &opening, &digests).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("digest count mismatch"));
    }
}
