use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use runloop_agent_contact_resolver as contact_agent;
use runloop_agent_contact_resolver::ContactIntent;
use runloop_agent_context_gatherer as context_agent;
use runloop_agent_context_gatherer::ContextRequest;
use runloop_agent_critic as critic_agent;
use runloop_agent_critic::ReviewRequest;
use runloop_agent_mailer as mailer_agent;
use runloop_agent_mailer::{DraftData as MailerDraftData, MailRequest};
use runloop_agent_registry::{AgentBundle, AgentRegistry, digest_file_hex};
use runloop_agent_writer as writer_agent;
use runloop_agent_writer::DraftRequest;
use runloop_agents_common::{
    ActionDecision, ActionProposal, AgentContext, AgentError, AgentResult, ConfirmationProvider,
    ContextBundle, DraftArtifact, MailResult, ResolvedContact, Review,
};
use runloop_core::ids::{AgentId, EventId, OpeningId, TraceId};
use runloop_core::{AgentRef, Config};
use runloop_kb::{KnowledgeBase, Materializer, Provenance, StateDelta};
use runloop_model_broker::{Broker, BrokerInitError, SecretResolver};
use runloop_openings::{
    Executor, Node, NodeExecution, NodeExecutionRequest, NodeInputs, NodeKind, NodeOutputs,
    RunnerError,
};
use runloop_runtime::{
    AgentIdentity, AgentSpec, AuditPolicy, Runtime, RuntimeBuilder, SecretProvider,
    secret_provider_from_config,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ExecutorInitError {
    #[error(transparent)]
    Config(#[from] runloop_core::Error),
    #[error(transparent)]
    Kb(#[from] runloop_kb::Error),
    #[error(transparent)]
    Broker(#[from] BrokerInitError),
    #[error(transparent)]
    Runtime(#[from] runloop_runtime::Error),
}

struct ProviderResolver {
    provider: Arc<dyn SecretProvider>,
}

impl ProviderResolver {
    fn new(provider: Arc<dyn SecretProvider>) -> Self {
        Self { provider }
    }
}

impl SecretResolver for ProviderResolver {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        self.provider.resolve(secret_id)
    }
}

pub fn build_executor(
    config: Config,
    confirmation: Arc<dyn ConfirmationProvider>,
    registry: Arc<AgentRegistry>,
) -> Result<Arc<LocalExecutor>, ExecutorInitError> {
    let kb = KnowledgeBase::open(&config.kb)?;
    kb.migrate()?;
    catch_up_views(&kb)?;
    seed_contact(&kb)?;

    let secrets = secret_provider_from_config(&config);
    let secret_resolver: Arc<dyn SecretResolver> = Arc::new(ProviderResolver::new(secrets.clone()));
    let broker = Arc::new(Broker::new(config.models.broker.clone(), secret_resolver)?);
    let audit_policy = AuditPolicy::new(
        config.security.caps.audit_on_allow,
        config.security.caps.audit_on_deny,
    );
    let runtime = RuntimeBuilder::new()
        .knowledge_base(Arc::new(kb.clone()))
        .model_broker(broker.clone())
        .secrets(secrets)
        .audit_policy(audit_policy)
        .allow_missing_secrets(config.allow_missing_secrets())
        .expose_raw_secrets(config.expose_raw_secrets())
        .build()?;

    Ok(Arc::new(LocalExecutor::new(
        config,
        kb,
        confirmation,
        registry,
        runtime,
        broker,
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
    catch_up_views(kb)?;
    Ok(())
}

pub struct LocalExecutor {
    config: Config,
    kb: KnowledgeBase,
    broker: Arc<Broker>,
    confirmation: Arc<dyn ConfirmationProvider>,
    registry: Arc<AgentRegistry>,
    runtime: Runtime,
    workdir: PathBuf,
}

impl LocalExecutor {
    pub fn runtime(&self) -> Runtime {
        self.runtime.clone()
    }

    pub fn broker(&self) -> Arc<Broker> {
        self.broker.clone()
    }

    pub fn new(
        config: Config,
        kb: KnowledgeBase,
        confirmation: Arc<dyn ConfirmationProvider>,
        registry: Arc<AgentRegistry>,
        runtime: Runtime,
        broker: Arc<Broker>,
    ) -> Self {
        let workdir = PathBuf::from(&config.runtime.workdir);
        Self {
            config,
            kb,
            broker,
            confirmation,
            registry,
            runtime,
            workdir,
        }
    }

    async fn exec_agent(
        &self,
        reference: &AgentRef,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        match reference.name.as_str() {
            "contact_resolver" => self.exec_contact(reference, &request).await,
            "context_gatherer" => self.exec_context(reference, &request).await,
            "writer" => self.exec_writer(reference, &request).await,
            "critic" => self.exec_critic(reference, &request).await,
            "mailer" => self.exec_mailer(reference, &request).await,
            "system_tra" => self.exec_system_tra(reference, &request).await,
            _ => self.exec_generic(reference, &request).await,
        }
    }

    async fn exec_system_tra(
        &self,
        reference: &AgentRef,
        request: &NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let input_value = request
            .node
            .with
            .get("input")
            .ok_or_else(|| RunnerError::Executor("system_tra requires an 'input'".into()))?;

        let input_arg = stringify_input(input_value)
            .map_err(|err| RunnerError::Executor(format!("invalid input for system_tra: {err}")))?;

        if let Some(bundle) = self.wasm_bundle(reference) {
            let args = vec![
                reference_spec(reference),
                "--input".into(),
                input_arg.clone(),
            ];
            match self
                .invoke_agent::<JsonValue>(
                    reference,
                    &bundle,
                    &request.node.id,
                    args,
                    None,
                    self.agent_timeout(request),
                )
                .await
            {
                Ok(output) => {
                    let mut outputs = NodeOutputs::default();
                    outputs.push("out", output);
                    return Ok(NodeExecution::Completed(outputs));
                }
                Err(err) => return Err(err),
            };
        }

        Err(RunnerError::Executor(
            "system_tra missing wasm entry".into(),
        ))
    }

    async fn exec_generic(
        &self,
        reference: &AgentRef,
        request: &NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let bundle = self.require_wasm_bundle(reference)?;
        let mut args = vec![reference_spec(reference)];
        for (key, value) in &request.node.with {
            let rendered = stringify_input(value).map_err(|err| {
                RunnerError::Executor(format!(
                    "invalid parameter '{key}' for node {}: {err}",
                    request.node.id
                ))
            })?;
            args.push(format!("--{key}"));
            args.push(rendered);
        }
        let mut env = Vec::new();
        let with_json = serde_json::to_string(&request.node.with).map_err(|err| {
            RunnerError::Executor(format!(
                "failed to encode params for node {}: {err}",
                request.node.id
            ))
        })?;
        env.push(("RUNLOOP_NODE_INPUT".into(), with_json));

        let output: JsonValue = self
            .invoke_agent(
                reference,
                &bundle,
                &request.node.id,
                args,
                Some(env),
                self.agent_timeout(request),
            )
            .await?;

        let mut ports = NodeOutputs::default();
        if let Some(map) = output.as_object() {
            for port in &bundle.described.ports.outputs {
                if let Some(value) = map.get(port) {
                    ports.push(port, value.clone());
                }
            }
        }
        if ports.ports.is_empty() {
            ports.push("out", output);
        }
        Ok(NodeExecution::Completed(ports))
    }

    fn node_param<T: DeserializeOwned>(
        &self,
        node: &Node,
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

    fn ctx(&self, request: &NodeExecutionRequest<'_>) -> AgentContext {
        self.ctx_with_confirmation_override(request, None)
    }

    fn ctx_with_confirmation_override(
        &self,
        request: &NodeExecutionRequest<'_>,
        confirmation: Option<Arc<dyn ConfirmationProvider>>,
    ) -> AgentContext {
        let confirmation = confirmation.or_else(|| Some(self.confirmation.clone()));
        AgentContext::new(
            self.kb.clone(),
            Some(self.broker.clone()),
            self.workdir.clone(),
            request.trace_id,
            request.opening_id,
            request.agent_id,
            confirmation,
        )
    }

    fn read_port<T: DeserializeOwned>(
        &self,
        inputs: &NodeInputs,
        port: &str,
    ) -> Result<Option<T>, RunnerError> {
        if let Some(values) = inputs.ports.get(port)
            && let Some(value) = values.first()
        {
            return serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|err| {
                    RunnerError::Executor(format!("failed to decode port '{port}': {err}"))
                });
        }
        Ok(None)
    }

    fn agent_timeout(&self, request: &NodeExecutionRequest<'_>) -> Duration {
        Duration::from_millis(request.node.timeout_ms.unwrap_or(30_000))
    }

    async fn exec_contact(
        &self,
        reference: &AgentRef,
        request: &NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let query: String = self
            .node_param(request.node, "query")?
            .or_else(|| {
                self.node_param(request.node, "recipient_query")
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        if query.trim().is_empty() {
            return Err(RunnerError::Executor(
                "contact_resolver requires a 'query'".into(),
            ));
        }
        if let Some(bundle) = self.wasm_bundle(reference) {
            let args = vec![reference_spec(reference), "--query".into(), query.clone()];
            match self
                .invoke_agent::<ResolvedContact>(
                    reference,
                    &bundle,
                    &request.node.id,
                    args,
                    None,
                    self.agent_timeout(request),
                )
                .await
            {
                Ok(contact) => {
                    let mut outputs = NodeOutputs::default();
                    outputs.push(
                        "out",
                        serde_json::to_value(contact).expect("contact to JSON"),
                    );
                    return Ok(NodeExecution::Completed(outputs));
                }
                Err(err) if self.should_fallback(&err) => {}
                Err(err) => return Err(err),
            }
        }
        self.exec_contact_native(request, query).await
    }

    async fn exec_context(
        &self,
        reference: &AgentRef,
        request: &NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let topic: String = self
            .node_param(request.node, "topic")?
            .unwrap_or_else(|| "general update".into());
        let contact = self.read_port(request.inputs, "contact")?;
        if let Some(bundle) = self.wasm_bundle(reference) {
            let mut args = vec![reference_spec(reference), "--topic".into(), topic.clone()];
            if let Some(ref contact) = contact {
                args.push("--contact-base64".into());
                args.push(encode_json_arg(contact)?);
            }
            match self
                .invoke_agent::<ContextBundle>(
                    reference,
                    &bundle,
                    &request.node.id,
                    args,
                    None,
                    self.agent_timeout(request),
                )
                .await
            {
                Ok(context_bundle) => {
                    let mut outputs = NodeOutputs::default();
                    outputs.push(
                        "out",
                        serde_json::to_value(context_bundle).expect("context to JSON"),
                    );
                    return Ok(NodeExecution::Completed(outputs));
                }
                Err(err) if self.should_fallback(&err) => {}
                Err(err) => return Err(err),
            }
        }
        self.exec_context_native(request, topic, contact).await
    }

    async fn exec_writer(
        &self,
        reference: &AgentRef,
        request: &NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let recipient: ResolvedContact = self
            .read_port(request.inputs, "recipients")?
            .ok_or_else(|| RunnerError::Executor("writer missing recipients input".into()))?;
        let context: ContextBundle = self
            .read_port(request.inputs, "context")?
            .ok_or_else(|| RunnerError::Executor("writer missing context input".into()))?;
        let topic: String = self
            .node_param(request.node, "topic")?
            .unwrap_or_else(|| "update".into());
        let tone = self
            .node_param::<String>(request.node, "tone")?
            .unwrap_or_else(|| {
                request
                    .node
                    .with
                    .get("tone")
                    .and_then(|json| json.as_str())
                    .unwrap_or("neutral-friendly")
                    .to_string()
            });
        let model = self
            .node_param::<String>(request.node, "model")?
            .or_else(|| Some(self.config.models.default.clone()));
        if let Some(bundle) = self.wasm_bundle(reference) {
            let mut args = vec![
                reference_spec(reference),
                "--recipient-base64".into(),
                encode_json_arg(&recipient)?,
                "--topic".into(),
                topic.clone(),
                "--tone".into(),
                tone.clone(),
            ];
            args.push("--context-base64".into());
            args.push(encode_json_arg(&context)?);
            match self
                .invoke_agent::<WriterAgentOutput>(
                    reference,
                    &bundle,
                    &request.node.id,
                    args,
                    None,
                    self.agent_timeout(request),
                )
                .await
            {
                Ok(output) => {
                    let artifact = self
                        .persist_draft(
                            request.trace_id,
                            request.opening_id,
                            request.node.id.as_str(),
                            &recipient,
                            &output,
                        )
                        .map_err(|err| RunnerError::Executor(err.to_string()))?;
                    let mut outputs = NodeOutputs::default();
                    outputs.push(
                        "out",
                        serde_json::to_value(artifact).expect("draft to JSON"),
                    );
                    return Ok(NodeExecution::Completed(outputs));
                }
                Err(err) if self.should_fallback(&err) => {}
                Err(err) => return Err(err),
            }
        }
        self.exec_writer_native(request, recipient, context, topic, tone, model)
            .await
    }

    async fn exec_critic(
        &self,
        reference: &AgentRef,
        request: &NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let draft: DraftArtifact = self
            .read_port(request.inputs, "in")?
            .ok_or_else(|| RunnerError::Executor("critic missing draft input".into()))?;
        if let Some(bundle) = self.wasm_bundle(reference) {
            let args = vec![
                reference_spec(reference),
                "--draft-base64".into(),
                encode_json_arg(&MinimalDraft::from(&draft))?,
            ];
            match self
                .invoke_agent::<Review>(
                    reference,
                    &bundle,
                    &request.node.id,
                    args,
                    None,
                    self.agent_timeout(request),
                )
                .await
            {
                Ok(review) => {
                    let mut outputs = NodeOutputs::default();
                    outputs.push("ok", JsonValue::Bool(review.ok));
                    outputs.push(
                        "review",
                        serde_json::to_value(review).expect("review to JSON"),
                    );
                    return Ok(NodeExecution::Completed(outputs));
                }
                Err(err) if self.should_fallback(&err) => {}
                Err(err) => return Err(err),
            }
        }
        self.exec_critic_native(request, draft).await
    }

    async fn exec_mailer(
        &self,
        reference: &AgentRef,
        request: &NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let draft: DraftArtifact = self
            .read_port(request.inputs, "draft")?
            .ok_or_else(|| RunnerError::Executor("mailer missing draft input".into()))?;
        let contact: ResolvedContact = self
            .read_port(request.inputs, "contact")?
            .ok_or_else(|| RunnerError::Executor("mailer missing contact input".into()))?;
        let review: Review = self
            .read_port(request.inputs, "review")?
            .ok_or_else(|| RunnerError::Executor("mailer missing review input".into()))?;
        let topic: String = self
            .node_param(request.node, "topic")?
            .unwrap_or_else(|| "update".into());
        let decision = self.request_confirmation(request, &draft, &contact).await?;
        if let Some(bundle) = self.wasm_bundle(reference) {
            if let Some(action) = decision.as_ref()
                && !action.approved
            {
                return Err(RunnerError::Executor("send cancelled by operator".into()));
            }
            let args = vec![
                reference_spec(reference),
                "--draft-base64".into(),
                encode_json_arg(&MinimalDraft::from(&draft))?,
                "--contact-base64".into(),
                encode_json_arg(&contact)?,
                "--review-base64".into(),
                encode_json_arg(&review)?,
                "--topic".into(),
                topic.clone(),
            ];
            match self
                .invoke_agent::<MailerAgentOutput>(
                    reference,
                    &bundle,
                    &request.node.id,
                    args,
                    None,
                    self.agent_timeout(request),
                )
                .await
            {
                Ok(mail) => {
                    let mail_result = self.record_mail_event(
                        request,
                        &draft,
                        &topic,
                        &mail,
                        decision
                            .as_ref()
                            .and_then(|d| d.rationale.as_ref().map(ToOwned::to_owned)),
                    )?;
                    let mut outputs = NodeOutputs::default();
                    outputs.push("ok", JsonValue::Bool(true));
                    outputs.push(
                        "out",
                        serde_json::to_value(mail_result).expect("mail result to JSON"),
                    );
                    return Ok(NodeExecution::Completed(outputs));
                }
                Err(err) if self.should_fallback(&err) => {}
                Err(err) => return Err(err),
            }
        }
        let override_confirmation = decision.map(|decision| {
            Arc::new(PreApprovedConfirmation::new(decision)) as Arc<dyn ConfirmationProvider>
        });
        self.exec_mailer_native(
            request,
            draft,
            contact,
            review,
            topic,
            override_confirmation,
        )
        .await
    }

    async fn request_confirmation(
        &self,
        request: &NodeExecutionRequest<'_>,
        draft: &DraftArtifact,
        contact: &ResolvedContact,
    ) -> Result<Option<ActionDecision>, RunnerError> {
        if self.config.security.confirm_external_actions {
            let proposal = ActionProposal {
                id: format!("node:{}", request.node.id),
                trace_id: request.trace_id,
                opening_id: request.opening_id,
                agent: AgentId::new(),
                summary: format!("Send draft email \"{}\"", draft.rationale),
                recipients: vec![contact.email.clone()],
                artifact_path: draft.path.clone(),
            };
            return self
                .confirmation
                .confirm(proposal)
                .await
                .map(Some)
                .map_err(|err| RunnerError::Executor(err.to_string()));
        }
        Ok(None)
    }

    async fn invoke_agent<T>(
        &self,
        reference: &AgentRef,
        bundle: &AgentBundle,
        node_id: &str,
        args: Vec<String>,
        env: Option<Vec<(String, String)>>,
        timeout: Duration,
    ) -> Result<T, RunnerError>
    where
        T: DeserializeOwned,
    {
        let output = self
            .spawn_and_collect(
                reference,
                bundle,
                node_id,
                args,
                env.unwrap_or_default(),
                timeout,
            )
            .await?;
        serde_json::from_str(&output)
            .map_err(|err| RunnerError::Executor(format!("invalid {} output: {err}", reference)))
    }

    async fn spawn_and_collect(
        &self,
        reference: &AgentRef,
        bundle: &AgentBundle,
        node_id: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
        timeout: Duration,
    ) -> Result<String, RunnerError> {
        let wasm_entry = bundle
            .wasm_entry
            .as_ref()
            .ok_or_else(|| RunnerError::Executor("agent missing wasm entry".into()))?;
        let policy_path = bundle
            .policy_path
            .clone()
            .unwrap_or_else(|| bundle.manifest_dir.join("policy.caps"));
        let identity = agent_identity(reference);
        let mut spec = AgentSpec::builder(identity, wasm_entry.path.clone())
            .policy_path(policy_path)
            .argv(args)
            .stdout_capacity(256 * 1024)
            .stderr_capacity(64 * 1024);
        for (key, value) in env {
            spec = spec.env(key, value);
        }
        if let Some(dir) = bundle.manifest_dir.to_str() {
            spec = spec.cwd(dir);
        }
        let mut spec = spec
            .build()
            .map_err(|err| RunnerError::Executor(err.to_string()))?;
        spec.spawn_ready_timeout_ms = Some(timeout.as_millis() as u64);
        let handle = self
            .runtime
            .spawn(spec)
            .map_err(|err| RunnerError::Executor(err.to_string()))?;
        let deadline = Instant::now() + timeout;
        let mut last_stdout_len = 0usize;
        let mut last_stderr_len = 0usize;
        loop {
            let stdout = handle.stdout();
            if stdout.len() != last_stdout_len {
                if let Some(json_text) = try_parse_json(&stdout) {
                    let _ = self.runtime.kill(handle.id());
                    return Ok(json_text);
                }
                last_stdout_len = stdout.len();
            }
            let stderr = handle.stderr();
            if stderr.len() > last_stderr_len {
                let chunk = &stderr[last_stderr_len..];
                last_stderr_len = stderr.len();
                let message = String::from_utf8_lossy(chunk).trim().to_string();
                if !message.is_empty() {
                    let _ = self.runtime.kill(handle.id());
                    return Err(RunnerError::Executor(format!(
                        "agent {} failed: {message}",
                        reference_spec(reference)
                    )));
                }
            }
            if Instant::now() >= deadline {
                let _ = self.runtime.kill(handle.id());
                return Err(RunnerError::Timeout {
                    node_id: node_id.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            sleep(Duration::from_millis(15)).await;
        }
    }

    async fn exec_contact_native(
        &self,
        request: &NodeExecutionRequest<'_>,
        query: String,
    ) -> Result<NodeExecution, RunnerError> {
        let ctx = self.ctx(request);
        let contact = contact_agent::resolve(
            &ctx,
            ContactIntent {
                recipient_query: query,
            },
        )
        .await
        .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        outputs.push(
            "out",
            serde_json::to_value(contact).expect("contact to JSON"),
        );
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_context_native(
        &self,
        request: &NodeExecutionRequest<'_>,
        topic: String,
        contact: Option<ResolvedContact>,
    ) -> Result<NodeExecution, RunnerError> {
        let ctx = self.ctx(request);
        let bundle = context_agent::gather(&ctx, ContextRequest { topic, contact })
            .await
            .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        outputs.push(
            "out",
            serde_json::to_value(bundle).expect("context to JSON"),
        );
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_writer_native(
        &self,
        request: &NodeExecutionRequest<'_>,
        recipient: ResolvedContact,
        context: ContextBundle,
        topic: String,
        tone: String,
        model: Option<String>,
    ) -> Result<NodeExecution, RunnerError> {
        let ctx = self.ctx(request);
        let draft = writer_agent::draft(
            &ctx,
            DraftRequest {
                recipient,
                topic: topic.clone(),
                context,
                tone: Some(tone),
                length_hint: None,
                model,
                max_words: Some(180),
            },
        )
        .await
        .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        outputs.push("out", serde_json::to_value(draft).expect("draft to JSON"));
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_critic_native(
        &self,
        _request: &NodeExecutionRequest<'_>,
        draft: DraftArtifact,
    ) -> Result<NodeExecution, RunnerError> {
        let review = critic_agent::critique(ReviewRequest { draft })
            .await
            .map_err(map_agent_error)?;
        let mut outputs = NodeOutputs::default();
        outputs.push("ok", JsonValue::Bool(review.ok));
        outputs.push(
            "review",
            serde_json::to_value(review).expect("review to JSON"),
        );
        Ok(NodeExecution::Completed(outputs))
    }

    async fn exec_mailer_native(
        &self,
        request: &NodeExecutionRequest<'_>,
        draft: DraftArtifact,
        contact: ResolvedContact,
        review: Review,
        topic: String,
        confirmation_override: Option<Arc<dyn ConfirmationProvider>>,
    ) -> Result<NodeExecution, RunnerError> {
        let ctx = self.ctx_with_confirmation_override(request, confirmation_override);
        let mail = mailer_agent::send(
            &ctx,
            MailRequest {
                draft: MailerDraftData {
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
        outputs.push("ok", JsonValue::Bool(true));
        outputs.push(
            "out",
            serde_json::to_value(mail).expect("mail result to JSON"),
        );
        Ok(NodeExecution::Completed(outputs))
    }

    fn persist_draft(
        &self,
        trace_id: TraceId,
        opening_id: OpeningId,
        node_id: &str,
        recipient: &ResolvedContact,
        output: &WriterAgentOutput,
    ) -> Result<DraftArtifact, runloop_kb::Error> {
        let drafts_dir = self.workdir.join("artifacts").join("drafts");
        std::fs::create_dir_all(&drafts_dir)?;
        let filename = format!("draft-{}.md", Uuid::new_v4());
        let path = drafts_dir.join(filename);
        std::fs::write(&path, &output.body_md)?;
        let sha256 = format!("{:x}", Sha256::digest(output.body_md.as_bytes()));
        let payload = json!({
            "kind": "draft_email.md",
            "path": path.to_string_lossy(),
            "sha256": sha256,
            "summary": format!("Draft email to {}", recipient.name),
            "citations": output.citations,
        });
        let provenance = Provenance {
            trace_id: trace_id.to_string(),
            opening_id: opening_id.to_string(),
            agent_id: format!("agent:{}", node_id),
            inputs_hash: None,
            rationale: Some(output.rationale.clone()),
        };
        let event_id = self.kb.propose(StateDelta::new(
            "artifact.created",
            format!("agent:{node_id}"),
            Some("user".into()),
            payload,
            provenance,
        ))?;
        Ok(DraftArtifact {
            artifact_id: event_id,
            path,
            sha256,
            body_md: output.body_md.clone(),
            rationale: output.rationale.clone(),
            citations: output
                .citations
                .iter()
                .map(|value| EventId(*value))
                .collect(),
            word_count: output.word_count,
        })
    }

    fn record_mail_event(
        &self,
        request: &NodeExecutionRequest<'_>,
        draft: &DraftArtifact,
        topic: &str,
        mail: &MailerAgentOutput,
        rationale: Option<String>,
    ) -> Result<MailResult, RunnerError> {
        let payload = json!({
            "to": mail.recipients,
            "subject": topic,
            "artifact_id": draft.artifact_id.0
        });
        let provenance = Provenance {
            trace_id: request.trace_id.to_string(),
            opening_id: request.opening_id.to_string(),
            agent_id: format!("agent:{}", request.node.id),
            inputs_hash: None,
            rationale,
        };
        self.kb
            .propose(StateDelta::new(
                "email.sent",
                format!("agent:{}", request.node.id),
                Some("user".into()),
                payload,
                provenance,
            ))
            .map_err(|err| RunnerError::Executor(err.to_string()))?;
        Ok(MailResult {
            status: mail.status.clone(),
            recipients: mail.recipients.clone(),
            artifact_id: draft.artifact_id,
            message_id: mail.message_id.clone(),
            delivered_at_ms: mail.delivered_at_ms,
        })
    }

    fn wasm_bundle(&self, reference: &AgentRef) -> Option<AgentBundle> {
        self.registry.bundle(reference).ok().and_then(|bundle| {
            let entry = bundle.wasm_entry.as_ref()?;
            if !entry.path.is_file() {
                return None;
            }
            match digest_file_hex(&entry.path) {
                Ok(digest) => {
                    if digest != entry.blake3 {
                        tracing::warn!(
                            agent = %reference_spec(reference),
                            path = %entry.path.display(),
                            expected = %entry.blake3,
                            actual = %digest,
                            "wasm digest mismatch; falling back to native implementation"
                        );
                        return None;
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        agent = %reference_spec(reference),
                        path = %entry.path.display(),
                        %err,
                        "failed to verify wasm digest; falling back to native implementation"
                    );
                    return None;
                }
            }
            Some(bundle)
        })
    }

    fn require_wasm_bundle(&self, reference: &AgentRef) -> Result<AgentBundle, RunnerError> {
        let bundle = self
            .registry
            .bundle(reference)
            .map_err(|err| RunnerError::Executor(err.to_string()))?;
        let Some(entry) = bundle.wasm_entry.as_ref() else {
            return Err(RunnerError::Executor(format!(
                "agent '{}' is missing entry_wasm",
                reference_spec(reference)
            )));
        };
        if !entry.path.is_file() {
            return Err(RunnerError::Executor(format!(
                "agent '{}' missing wasm binary at {}",
                reference_spec(reference),
                entry.path.display()
            )));
        }
        let digest = digest_file_hex(&entry.path).map_err(|err| {
            RunnerError::Executor(format!(
                "{} digest check failed: {err}",
                entry.path.display()
            ))
        })?;
        if digest != entry.blake3 {
            return Err(RunnerError::Executor(format!(
                "agent '{}' wasm digest mismatch (expected {}, got {})",
                reference_spec(reference),
                entry.blake3,
                digest
            )));
        }
        Ok(bundle)
    }

    fn should_fallback(&self, err: &RunnerError) -> bool {
        matches!(err, RunnerError::Executor(msg) if msg.contains("missing wasm entry") || msg.contains("No such file or directory"))
    }
}

fn map_agent_error(err: AgentError) -> RunnerError {
    RunnerError::Executor(err.to_string())
}

fn stringify_input(value: &JsonValue) -> Result<String, serde_json::Error> {
    match value {
        JsonValue::String(text) => Ok(text.clone()),
        other => serde_json::to_string(other),
    }
}

struct PreApprovedConfirmation {
    decision: ActionDecision,
}

impl PreApprovedConfirmation {
    fn new(decision: ActionDecision) -> Self {
        Self { decision }
    }
}

#[async_trait]
impl ConfirmationProvider for PreApprovedConfirmation {
    async fn confirm(&self, _proposal: ActionProposal) -> AgentResult<ActionDecision> {
        Ok(self.decision.clone())
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
                "nested opening '{name}' unsupported in local executor"
            ))),
        }
    }
}

fn reference_spec(reference: &AgentRef) -> String {
    reference.variant.as_ref().map_or_else(
        || reference.name.clone(),
        |variant| format!("{}@{}", reference.name, variant),
    )
}

fn agent_identity(reference: &AgentRef) -> AgentIdentity {
    match &reference.variant {
        Some(variant) => AgentIdentity::new(reference.name.clone()).with_variant(variant.clone()),
        None => AgentIdentity::new(reference.name.clone()),
    }
}

fn encode_json_arg<T: Serialize>(value: &T) -> Result<String, RunnerError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| RunnerError::Executor(format!("failed to encode arg: {err}")))?;
    Ok(BASE64.encode(bytes))
}

fn try_parse_json(stdout: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(stdout).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    serde_json::from_str::<JsonValue>(text).ok()?;
    Some(text.to_string())
}

#[derive(Debug, Deserialize)]
struct WriterAgentOutput {
    body_md: String,
    rationale: String,
    citations: Vec<i64>,
    word_count: usize,
}

#[derive(Debug, Deserialize)]
struct MailerAgentOutput {
    status: String,
    recipients: Vec<String>,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    delivered_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct MinimalDraft {
    body_md: String,
    word_count: usize,
    artifact_id: i64,
}

impl From<&DraftArtifact> for MinimalDraft {
    fn from(draft: &DraftArtifact) -> Self {
        Self {
            body_md: draft.body_md.clone(),
            word_count: draft.word_count,
            artifact_id: draft.artifact_id.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stringify_input_keeps_string() {
        let input = JsonValue::String("plain".into());
        let result = stringify_input(&input).unwrap();
        assert_eq!(result, "plain");
    }

    #[test]
    fn stringify_input_serializes_object() {
        let input = json!({
            "tmux_conf": "~/.tmux.conf",
            "history_limit": 42,
            "extra_tmux_lines": ["set -g mouse on"]
        });
        let result = stringify_input(&input).unwrap();
        let reparsed: JsonValue = serde_json::from_str(&result).unwrap();
        assert_eq!(reparsed, input);
    }
}
