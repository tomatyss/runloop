mod control;
mod engine;
mod executor_bus;
mod metrics;
mod utils;

use async_trait::async_trait;
use executor_bus::AgentDispatcher;
use runloop_agent_registry::AgentRegistry;
use runloop_agents_common::{ActionDecision, ActionProposal, AgentResult, ConfirmationProvider};
use runloop_bus::{Bus, BusServerHandle, PublisherKind};
use runloop_core::{Config, Error};
use runloop_executor_local::{ExecutorInitError, build_executor};
use runloop_kb::{KnowledgeBase, Materializer, TraceStore};
use runloop_registry::{PathOverrides, resolve_paths};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::signal;
use tokio::sync::oneshot;
use tokio::time::{Duration, sleep};

use crate::control::{ControlPlaneCtx, run_control_plane_with_ready};
use crate::metrics::spawn_metrics_task;

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();
    tracing::info!("runloopd starting");

    let config = Config::load()?;
    // config.validate() is called inside Config::load()

    let registry_paths = resolve_paths(&config, &PathOverrides::default());
    for warning in &registry_paths.warnings {
        if warning.contains("opening search dirs") {
            continue;
        }
        tracing::warn!("{warning}");
    }
    if let Some(demo_dir) = &registry_paths.demo_agents {
        tracing::info!(
            "no agent search dirs found; falling back to demo bundles under {}",
            demo_dir.display()
        );
    }
    tracing::info!("agent search dirs: {}", format_dirs(&registry_paths.agents));
    warn_unreadable_dirs(&registry_paths.agents);
    let registry = Arc::new(AgentRegistry::new(registry_paths.agents.clone()));
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
    let local_executor =
        build_executor(config.clone(), confirmation, registry.clone()).map_err(|e| match e {
            ExecutorInitError::Config(err) => err,
            other => Error::Runtime(other.to_string()),
        })?;

    // Connect as daemon publisher
    let bus = Bus::connect_as(bus_path.as_path(), PublisherKind::Agent)
        .await
        .map_err(|e| Error::Bus(e.to_string()))?;

    let metrics_interval_ms = config.observability.metrics_interval_ms as u64;
    let bus_stats = bus_server.stats_handle();
    let dispatcher = Arc::new(AgentDispatcher::new(bus.clone(), local_executor.clone()));
    let (metrics_shutdown_tx, metrics_shutdown_rx) = oneshot::channel();
    let metrics_task = spawn_metrics_task(
        bus.clone(),
        bus_stats,
        local_executor.clone(),
        metrics_interval_ms,
        metrics_shutdown_rx,
    );

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
    let _ = metrics_shutdown_tx.send(());
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
    if let Err(err) = metrics_task.await {
        tracing::warn!(?err, "metrics task ended unexpectedly");
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
    // Config::validate ensures that if socket_path is None, sockets_dir is not empty.
    if let Some(path) = config.runtime.socket_path.as_deref() {
        return Ok(PathBuf::from(path.trim()));
    }
    let dir = config.runtime.sockets_dir.trim();
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
        // Validation logic is also in Config::validate, but we map here defensively.
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(Error::Config(
                "empty publisher kind entry in bus.auth.publishers.action_decision.allowed_kinds"
                    .into(),
            ));
        }
        let normalized_lower = normalized.to_ascii_lowercase();
        let kind = match normalized_lower.as_str() {
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

fn format_dirs(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn warn_unreadable_dirs(dirs: &[PathBuf]) {
    for dir in dirs {
        if dir.exists() && fs::read_dir(dir).is_err() {
            tracing::warn!(
                path = %dir.display(),
                "agent search dir exists but is not readable by the daemon user"
            );
        }
    }
}

fn publisher_kind_label(kind: &PublisherKind) -> &'static str {
    match kind {
        PublisherKind::Ui => "ui",
        PublisherKind::Tui => "tui",
        PublisherKind::Agent => "agent",
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
    use crate::engine::verify_agent_digests;
    use crate::utils::{current_millis, next_msg_id, uuid_to_u128};
    use bytes::Bytes;
    use futures_util::StreamExt;
    use runloop_bus::Message;
    use runloop_core::config::{ModelProvider, ModelRoute, ProviderKind};
    use runloop_core::content::{CT_CTRL_REQ, CT_CTRL_RESP, CT_RUN_EVENT};
    use runloop_core::{
        AgentDigest, AgentRef, ControlRequest, ControlResponse, RunSubmitRequest, TraceId,
    };
    use runloop_openings::parse_opening_str;
    use runloop_rmp::{Header, decode_payload, encode_payload};
    use tempfile::tempdir;

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
        let local_executor =
            build_executor(config.clone(), confirmation, registry.clone()).expect("build executor");
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
