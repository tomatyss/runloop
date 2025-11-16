use async_trait::async_trait;
use runloop_agent_contact_resolver as contact_agent;
use runloop_agent_contact_resolver::ContactIntent;
use runloop_agent_context_gatherer as context_agent;
use runloop_agent_context_gatherer::ContextRequest;
use runloop_agent_critic as critic_agent;
use runloop_agent_critic::ReviewRequest;
use runloop_agent_mailer as mailer_agent;
use runloop_agent_mailer::{DraftData, MailRequest};
use runloop_agent_writer as writer_agent;
use runloop_agent_writer::DraftRequest;
use runloop_agents_common::{
    AgentContext, AgentError, ConfirmationProvider, ContextBundle, DraftArtifact, ResolvedContact,
    Review,
};
use runloop_core::{AgentRef, Config};
use runloop_kb::{KnowledgeBase, Materializer, Provenance, StateDelta};
use runloop_model_broker::{Broker, BrokerInitError, SecretResolver};
use runloop_openings::{
    Executor, NodeExecution, NodeExecutionRequest, NodeKind, NodeOutputs, RunnerError,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorInitError {
    #[error(transparent)]
    Config(#[from] runloop_core::Error),
    #[error(transparent)]
    Kb(#[from] runloop_kb::Error),
    #[error(transparent)]
    Broker(#[from] BrokerInitError),
}

pub fn build_executor(
    config: Config,
    confirmation: Arc<dyn ConfirmationProvider>,
    secret_resolver: Arc<dyn SecretResolver>,
) -> Result<Arc<LocalExecutor>, ExecutorInitError> {
    let kb = KnowledgeBase::open(&config.kb)?;
    kb.migrate()?;
    catch_up_views(&kb)?;
    seed_contact(&kb)?;
    let broker = Arc::new(Broker::new(config.models.broker.clone(), secret_resolver)?);
    Ok(Arc::new(LocalExecutor::new(
        config,
        kb,
        broker,
        confirmation,
    )))
}

pub fn catch_up_views(kb: &KnowledgeBase) -> Result<(), runloop_kb::Error> {
    let materializer = Materializer::new(kb.clone());
    while materializer.sync()? {}
    Ok(())
}

pub fn seed_contact(kb: &KnowledgeBase) -> Result<(), runloop_kb::Error> {
    let result =
        kb.query("SELECT contact_key FROM contacts WHERE email = 'john@acme.com' LIMIT 1")?;
    if !result.rows.is_empty() {
        return Ok(());
    }
    let payload = json!({
        "name": "John Smith",
        "email": "john@acme.com",
        "org": "Acme",
        "trust": 0.9,
        "evidence": []
    });
    let provenance = Provenance {
        trace_id: "trace:seed".into(),
        opening_id: "opening:seed".into(),
        agent_id: "agent:seed-contact".into(),
        inputs_hash: None,
        rationale: Some("seed contact for compose_email".into()),
    };
    kb.propose(StateDelta::new(
        "contact.upserted",
        "agent:seed",
        Some("system".to_string()),
        payload,
        provenance,
    ))?;
    let materializer = Materializer::new(kb.clone());
    while materializer.sync()? {}
    Ok(())
}

pub struct LocalExecutor {
    config: Config,
    kb: KnowledgeBase,
    broker: Arc<Broker>,
    confirmation: Arc<dyn ConfirmationProvider>,
}

impl LocalExecutor {
    pub fn new(
        config: Config,
        kb: KnowledgeBase,
        broker: Arc<Broker>,
        confirmation: Arc<dyn ConfirmationProvider>,
    ) -> Self {
        Self {
            config,
            kb,
            broker,
            confirmation,
        }
    }

    fn workdir(&self) -> PathBuf {
        PathBuf::from(&self.config.runtime.workdir)
    }

    async fn exec_agent(
        &self,
        reference: &AgentRef,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        if let Some(variant) = &reference.variant {
            return Err(RunnerError::Executor(format!(
                "agent '{}' variant '{variant}' unsupported in local executor",
                reference.name
            )));
        }
        match reference.name.as_str() {
            "contact_resolver" => self.exec_contact(request).await,
            "context_gatherer" => self.exec_context(request).await,
            "writer" => self.exec_writer(request).await,
            "critic" => self.exec_critic(request).await,
            "mailer" => self.exec_mailer(request).await,
            other => Err(RunnerError::Executor(format!("unknown agent '{other}'"))),
        }
    }

    fn ctx(&self, request: &NodeExecutionRequest<'_>) -> AgentContext {
        AgentContext::new(
            self.kb.clone(),
            Some(self.broker.clone()),
            self.workdir(),
            request.trace_id,
            request.opening_id,
            request.agent_id,
            Some(self.confirmation.clone()),
        )
    }

    fn node_param<T: DeserializeOwned>(
        &self,
        node: &runloop_openings::Node,
        key: &str,
    ) -> Result<Option<T>, RunnerError> {
        node.with
            .get(key)
            .map(|value| {
                serde_json::from_value(value.clone()).map_err(|err| {
                    RunnerError::Executor(format!(
                        "invalid parameter '{key}' for node {}: {err}",
                        node.id
                    ))
                })
            })
            .transpose()
    }

    fn read_port<T: DeserializeOwned>(
        &self,
        request: &NodeExecutionRequest<'_>,
        port: &str,
    ) -> Result<Option<T>, RunnerError> {
        if let Some(values) = request.inputs.ports.get(port)
            && let Some(value) = values.first()
        {
            return serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|err| {
                    RunnerError::Executor(format!(
                        "failed to decode port '{}' on node '{}': {err}",
                        port, request.node.id
                    ))
                });
        }
        Ok(None)
    }

    fn push_port<T: serde::Serialize>(
        &self,
        outputs: &mut NodeOutputs,
        port: &str,
        value: &T,
    ) -> Result<(), RunnerError> {
        let json = serde_json::to_value(value).map_err(|err| {
            RunnerError::Executor(format!("failed to encode output '{port}': {err}"))
        })?;
        outputs.push(port, json);
        Ok(())
    }

    async fn exec_contact(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let query: String = self
            .node_param(request.node, "query")?
            .or_else(|| {
                self.node_param(request.node, "recipient_query")
                    .unwrap_or(None)
            })
            .unwrap_or_default();
        if query.trim().is_empty() {
            return Err(RunnerError::Executor(
                "contact_resolver requires 'query' in node config".into(),
            ));
        }
        let ctx = self.ctx(&request);
        let contact = contact_agent::resolve(
            &ctx,
            ContactIntent {
                recipient_query: query,
            },
        )
        .await
        .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        self.push_port(&mut outputs, "out", &contact)?;
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_context(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let topic: String = self
            .node_param(request.node, "topic")?
            .unwrap_or_else(|| "general update".into());
        let contact: Option<ResolvedContact> = self.read_port(&request, "contact")?;
        let ctx = self.ctx(&request);
        let bundle = context_agent::gather(&ctx, ContextRequest { topic, contact })
            .await
            .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        self.push_port(&mut outputs, "out", &bundle)?;
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_writer(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let recipient: ResolvedContact = self
            .read_port(&request, "recipients")?
            .ok_or_else(|| RunnerError::Executor("writer missing recipients input".into()))?;
        let context: ContextBundle = self
            .read_port(&request, "context")?
            .ok_or_else(|| RunnerError::Executor("writer missing context input".into()))?;
        let topic: String = self
            .node_param(request.node, "topic")?
            .unwrap_or_else(|| "update".into());
        let tone = self.node_param(request.node, "tone")?;
        let model = self
            .node_param(request.node, "model")?
            .or_else(|| Some(self.config.models.default.clone()));
        let ctx = self.ctx(&request);
        let draft = writer_agent::draft(
            &ctx,
            DraftRequest {
                recipient,
                topic,
                context,
                tone,
                length_hint: None,
                model,
                max_words: Some(180),
            },
        )
        .await
        .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        self.push_port(&mut outputs, "out", &draft)?;
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_critic(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let draft: DraftArtifact = self
            .read_port(&request, "in")?
            .ok_or_else(|| RunnerError::Executor("critic missing draft input".into()))?;
        let review = critic_agent::critique(ReviewRequest { draft })
            .await
            .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        self.push_port(&mut outputs, "ok", &review.ok)?;
        self.push_port(&mut outputs, "review", &review)?;
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_mailer(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let draft: DraftArtifact = self
            .read_port(&request, "draft")?
            .ok_or_else(|| RunnerError::Executor("mailer missing draft input".into()))?;
        let contact: ResolvedContact = self
            .read_port(&request, "contact")?
            .ok_or_else(|| RunnerError::Executor("mailer missing contact input".into()))?;
        let review: Review = self
            .read_port(&request, "review")?
            .ok_or_else(|| RunnerError::Executor("mailer missing review input".into()))?;
        let topic: String = self
            .node_param(request.node, "topic")?
            .unwrap_or_else(|| "update".into());
        let ctx = self.ctx(&request);
        let mail = mailer_agent::send(
            &ctx,
            MailRequest {
                draft: DraftData {
                    artifact_id: draft.artifact_id,
                    path: draft.path.to_string_lossy().into_owned(),
                    body_preview: draft.body_md.clone(),
                },
                contact,
                review,
                topic,
            },
        )
        .await
        .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        self.push_port(&mut outputs, "out", &mail)?;
        self.push_port(&mut outputs, "ok", &true)?;
        Ok(NodeExecution::Completed(outputs))
    }
}

#[async_trait]
impl Executor for LocalExecutor {
    async fn execute(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        match &request.node.kind {
            NodeKind::Agent { reference } => self.exec_agent(reference, request).await,
            NodeKind::Opening { name } => Err(RunnerError::Executor(format!(
                "nested opening '{name}' unsupported in CLI executor"
            ))),
        }
    }
}

fn map_agent_error(err: AgentError) -> RunnerError {
    RunnerError::Executor(format!("{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use runloop_agents_common::{
        ActionDecision, ActionProposal, AgentResult, ConfirmationProvider,
    };
    use runloop_core::config::{ModelProvider, ModelRoute, ProviderKind};
    use runloop_openings::{NodeState, Runner, parse_opening_str};
    use rusqlite::Connection;
    use serde_json::Value as JsonValue;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn catch_up_views_is_noop_for_empty_kb() {
        let kb = KnowledgeBase::new();
        catch_up_views(&kb).expect("catch up succeeds");
    }

    #[test]
    fn seed_contact_inserts_expected_record() {
        let kb = KnowledgeBase::new();
        seed_contact(&kb).expect("seed contact succeeds");
        assert_eq!(seeded_count(&kb), 1, "contact should be inserted once");
    }

    #[test]
    fn seed_contact_is_idempotent() {
        let kb = KnowledgeBase::new();
        seed_contact(&kb).expect("first seed succeeds");
        seed_contact(&kb).expect("second seed is a no-op");
        assert_eq!(seeded_count(&kb), 1, "contact remains unique");
    }

    fn seeded_count(kb: &KnowledgeBase) -> i64 {
        kb.query("SELECT COUNT(*) AS count FROM contacts WHERE email = 'john@acme.com'")
            .expect("query contacts")
            .rows
            .first()
            .and_then(|row| row.get("count"))
            .and_then(JsonValue::as_i64)
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn executor_runs_compose_email_opening() {
        let tmp = tempdir().expect("tmpdir");
        let workdir = tmp.path().join("workdir");
        let sockets_dir = tmp.path().join("run");
        let kb_root = tmp.path().join("kb");
        let secrets_dir = tmp.path().join("secrets");
        fs::create_dir_all(&workdir).expect("workdir");
        fs::create_dir_all(&sockets_dir).expect("sockets dir");
        fs::create_dir_all(&kb_root).expect("kb dir");
        fs::create_dir_all(&secrets_dir).expect("secrets dir");

        let mut config = Config::default();
        config.runtime.workdir = workdir.to_string_lossy().into_owned();
        config.runtime.sockets_dir = sockets_dir.to_string_lossy().into_owned();
        config.runtime.socket_path = Some(
            sockets_dir
                .join("runloop.sock")
                .to_string_lossy()
                .into_owned(),
        );
        config.kb.root_dir = kb_root.to_string_lossy().into_owned();
        config.security.secrets.root = Some(secrets_dir.to_string_lossy().into_owned());
        config.models.default = "null:compose".into();
        config.models.broker.providers = vec![ModelProvider {
            id: "local".into(),
            kind: ProviderKind::Local,
            model_dir: None,
            base_url: None,
            secret_id: None,
            headers: BTreeMap::new(),
            schema: None,
        }];
        config.models.broker.route = vec![ModelRoute {
            pattern: "*".into(),
            provider: "local".into(),
            target_model: None,
        }];

        let view_db_path = PathBuf::from(&config.kb.root_dir).join(&config.kb.view_db);
        let view_conn = Connection::open(&view_db_path).expect("open view db");
        view_conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts_ms INTEGER NOT NULL,
                    actor TEXT,
                    kind TEXT NOT NULL,
                    scope TEXT,
                    payload_json TEXT NOT NULL,
                    provenance_json TEXT,
                    hash_blake3 BLOB
                );
                ",
            )
            .expect("create events table snapshot");

        let confirmation = Arc::new(TestConfirmationProvider);
        let secrets = Arc::new(TestSecretResolver);
        let executor = build_executor(config, confirmation, secrets).expect("build executor");

        let yaml = fs::read_to_string(compose_email_fixture()).expect("read compose_email");
        let opening = parse_opening_str(&yaml).expect("parse compose_email");
        let runner = Runner::new(opening, executor);
        let report = runner.run().await.expect("run opening");
        if !report.trace.success {
            let states = report
                .node_records
                .iter()
                .map(|record| format!("{}: {:?}", record.node_id, record.state))
                .collect::<Vec<_>>()
                .join(", ");
            panic!("compose_email opening should succeed: states={states}");
        }
        assert!(
            report
                .node_records
                .iter()
                .all(|record| matches!(record.state, NodeState::Succeeded)),
            "all nodes should succeed in happy-path integration"
        );
    }

    fn compose_email_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("examples")
            .join("openings")
            .join("compose_email.yaml")
    }

    struct TestConfirmationProvider;

    #[async_trait]
    impl ConfirmationProvider for TestConfirmationProvider {
        async fn confirm(&self, _proposal: ActionProposal) -> AgentResult<ActionDecision> {
            Ok(ActionDecision::approved(Some(
                "auto-approved in test".into(),
            )))
        }
    }

    struct TestSecretResolver;

    impl SecretResolver for TestSecretResolver {
        fn resolve(&self, _secret_id: &str) -> Option<String> {
            None
        }
    }
}
