use async_trait::async_trait;
use bytes::Bytes;
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
use runloop_kb::{KnowledgeBase, Materializer, Provenance, StateDelta};
use runloop_model_broker::SecretResolver;
use runloop_openings::{RunEvent, Runner, parse_opening_str};
use runloop_rmp::{Header, decode_payload, encode_payload};
use std::collections::BTreeMap;
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

    // Connect as daemon publisher
    let bus = Bus::connect_as(bus_path.as_path(), PublisherKind::Agent)
        .await
        .map_err(|e| Error::Bus(e.to_string()))?;

    // Spawn control-plane loop
    let (ctrl_shutdown_tx, ctrl_shutdown_rx) = oneshot::channel();
    let ctrl_task = tokio::spawn(run_control_plane_with_ready(
        config.clone(),
        registry.clone(),
        kb.clone(),
        bus.clone(),
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

async fn run_control_plane_with_ready(
    config: Config,
    registry: Arc<AgentRegistry>,
    kb: KnowledgeBase,
    bus: Bus,
    mut shutdown: oneshot::Receiver<()>,
    ready: Option<oneshot::Sender<()>>,
) -> Result<(), Error> {
    let mut inbox = bus
        .subscribe("rlp/ctrl")
        .await
        .map_err(|e| Error::Bus(e.to_string()))?;
    if let Some(tx) = ready {
        let _ = tx.send(());
    }
    use std::collections::HashMap;
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
                                        config: &config,
                                        registry: registry.as_ref(),
                                        kb: &kb,
                                        bus: &bus,
                                        accepted_map: accepted.clone(),
                                    },
                                    req_id,
                                    &opening_yaml,
                                    agent_digests,
                                    req_key,
                                )
                                .await
                                {
                                    Ok(acc) => {
                                        accepted.lock().unwrap().insert(req_key, acc);
                                    }
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
    config: &'a Config,
    registry: &'a AgentRegistry,
    kb: &'a KnowledgeBase,
    bus: &'a Bus,
    accepted_map: Arc<Mutex<std::collections::HashMap<u128, RunAccepted>>>,
}

async fn handle_run_submit(
    ctx: RunSubmitContext<'_>,
    request_id: TraceId,
    opening_yaml: &str,
    agent_digests: Vec<AgentDigest>,
    req_key: u128,
) -> Result<RunAccepted, Error> {
    let RunSubmitContext {
        config,
        registry,
        kb,
        bus,
        accepted_map,
    } = ctx;
    // Parse opening
    let opening = parse_opening_str(opening_yaml).map_err(|e| Error::Opening(e.to_string()))?;
    let opening_name = opening.name.clone();
    if !agent_digests.is_empty() {
        verify_agent_digests(registry, &opening, &agent_digests)?;
    }
    // Build executor
    let confirmation = Arc::new(DaemonConfirmationProvider::new(
        config.security.confirm_external_actions,
    ));
    let secrets = Arc::new(DaemonSecretResolver);
    let executor = build_executor(config.clone(), confirmation, secrets).map_err(|e| match e {
        ExecutorInitError::Config(err) => err,
        other => Error::Runtime(other.to_string()),
    })?;

    // Create runner and setup event channel
    let runner = Runner::new(opening, executor);
    let trace_id = runner.trace_id();
    let opening_id = runner.opening_id();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let runner = runner.with_event_tx(tx);

    // Respond with RunAccepted
    let resp = ControlResponse::RunAccepted(RunAccepted {
        request_id,
        trace_id,
        opening_id,
        opening_name: opening_name.clone(),
    });
    let header = build_header(CT_CTRL_RESP, uuid_to_u128(request_id.0), next_msg_id());
    let body = encode_payload(CT_CTRL_RESP, &resp, None)?;
    let frame = Message::new(header, Bytes::from(body)).map_err(|e| Error::Bus(e.to_string()))?;
    bus.publish("rlp/ctrl", frame)
        .await
        .map_err(|e| Error::Bus(e.to_string()))?;

    // Spawn the run in background
    let bus_clone = bus.clone();
    let kb_clone = kb.clone();
    let accepted_map = accepted_map.clone();
    tokio::spawn(async move {
        let topic = format!("rlp/runs/{}/events", trace_id);
        let mut run_future = Box::pin(async move { runner.run().await });
        let bus = bus_clone;
        let trace_key = uuid_to_u128(trace_id.0);
        // Give clients a moment to subscribe after acceptance
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Publish run started event after a brief grace period
        let _ = publish_to_topic(&bus, &topic, trace_key, serde_json::json!({
            "kind": "status",
            "level": "info",
            "message": "run started",
            "meta": {"ts_ms": current_millis(), "run_id": trace_id.to_string(), "opening_id": opening_id.to_string()}
        })).await;
        // Drain runner events and publish as CT_RUN_EVENT
        loop {
            tokio::select! {
                res = &mut run_future => {
                    match res {
                        Ok(report) => {
                            // Emit node finishes and run finished summary
                            let success = report.trace.success;
                            let status_str = if success { "ok" } else { "error" };
                            let payload = serde_json::json!({
                                "kind": "status",
                                "level": if success {"info"} else {"error"},
                                "message": format!("run {}", status_str),
                                "meta": {"ts_ms": current_millis(), "run_id": trace_id.to_string(), "opening_id": opening_id.to_string(), "success": success}
                            });
                            let _ = publish_to_topic(&bus, &topic, trace_key, payload).await;
                            // Persist run.finished
                            let _ = record_kb_run_event(&kb_clone, &trace_id, &opening_id, "run.finished", if success {"finished"} else {"failed"});
                        }
                        Err(err) => {
                            let payload = serde_json::json!({
                                "kind": "status",
                                "level": "error",
                                "message": format!("run failed: {err}"),
                                "meta": {"ts_ms": current_millis(), "run_id": trace_id.to_string(), "opening_id": opening_id.to_string(), "success": false}
                            });
                            let _ = publish_to_topic(&bus, &topic, trace_key, payload).await;
                            let _ = record_kb_run_event(&kb_clone, &trace_id, &opening_id, "run.finished", "failed");
                        }
                    }
                    break;
                }
                maybe_event = rx.recv() => {
                    if let Some(event) = maybe_event {
                        let payload = match event {
                            RunEvent::NodeState { node_id, state, attempt } => serde_json::json!({
                                "kind": "plan",
                                "message": format!("node {} state update", node_id),
                                "meta": {"ts_ms": current_millis(), "run_id": trace_id.to_string(), "node_id": node_id, "attempt": attempt, "state": format!("{:?}", state)}
                            }),
                            RunEvent::LogLine { node_id, level, message } => serde_json::json!({
                                "kind": "log",
                                "level": level,
                                "message": message,
                                "meta": {"ts_ms": current_millis(), "run_id": trace_id.to_string(), "node_id": node_id}
                            }),
                            RunEvent::TraceLine { line } => serde_json::json!({
                                "kind": "trace",
                                "level": "info",
                                "message": line,
                                "meta": {"ts_ms": current_millis(), "run_id": trace_id.to_string()}
                            }),
                            RunEvent::Completed { .. } => continue,
                        };
                        let _ = publish_to_topic(&bus, &topic, trace_key, payload).await;
                    } else {
                        break;
                    }
                }
            }
        }
        // Remove from idempotency map after completion
        let _ = accepted_map.lock().unwrap().remove(&req_key);
    });

    // Persist run.started into KB (minimal)
    record_kb_run_event(kb, &trace_id, &opening_id, "run.started", "started")
        .map_err(|e| Error::Kb(e.to_string()))?;

    Ok(RunAccepted {
        request_id,
        trace_id,
        opening_id,
        opening_name,
    })
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

fn record_kb_run_event(
    kb: &KnowledgeBase,
    trace_id: &TraceId,
    opening_id: &OpeningId,
    kind: &str,
    status: &str,
) -> Result<(), runloop_kb::Error> {
    let payload = serde_json::json!({
        "opening_id": opening_id.to_string(),
        "status": status,
    });
    let provenance = Provenance {
        trace_id: trace_id.to_string(),
        opening_id: opening_id.to_string(),
        agent_id: "agent:runloopd".into(),
        inputs_hash: None,
        rationale: None,
    };
    kb.propose(StateDelta::new(
        kind,
        "agent:runloopd",
        Some("system".into()),
        payload,
        provenance,
    ))?;
    Ok(())
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

fn uuid_to_u128(id: uuid::Uuid) -> u128 {
    id.as_u128()
}

fn next_msg_id() -> u64 {
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
        let (ready_tx, ready_rx) = oneshot::channel();
        let (ctrl_shutdown_tx, ctrl_shutdown_rx) = oneshot::channel();
        let ctrl = tokio::spawn(run_control_plane_with_ready(
            config.clone(),
            registry.clone(),
            kb.clone(),
            bus.clone(),
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
}
