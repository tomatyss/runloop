use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use runloop_agent_registry::AgentRegistry;
use runloop_agents_common::{ActionDecision, ActionProposal, AgentResult, ConfirmationProvider};
use runloop_core::Config;
use runloop_core::config::{ModelProvider, ModelRoute, ProviderKind, TestingConfig};
use runloop_executor_local::build_executor;
use runloop_openings::{Runner, parse_opening_str};
use serde_json::Value;
use tempfile::tempdir;

struct TestConfirmation;

#[async_trait]
impl ConfirmationProvider for TestConfirmation {
    async fn confirm(&self, _proposal: ActionProposal) -> AgentResult<ActionDecision> {
        Ok(ActionDecision::approved(Some("test".into())))
    }
}

#[tokio::test]
async fn compose_email_opening_runs_end_to_end() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let agents_dir = repo_root.join("agents");
    let opening_yaml =
        std::fs::read_to_string(repo_root.join("examples/openings/compose_email.yaml"))
            .expect("opening fixture");

    let tmp = tempdir().expect("temp");
    let workdir = tmp.path().join("workdir");
    let sockets_dir = tmp.path().join("sock");
    let kb_root = tmp.path().join("kb");
    let secrets_dir = tmp.path().join("secrets");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::create_dir_all(&sockets_dir).unwrap();
    std::fs::create_dir_all(&kb_root).unwrap();
    std::fs::create_dir_all(&secrets_dir).unwrap();

    let mut config = Config::default();
    config.runtime.workdir = workdir.to_string_lossy().into_owned();
    config.runtime.sockets_dir = sockets_dir.to_string_lossy().into_owned();
    config.runtime.socket_path = Some(
        sockets_dir
            .join("local.sock")
            .to_string_lossy()
            .into_owned(),
    );
    config.kb.root_dir = kb_root.to_string_lossy().into_owned();
    config.security.secrets.root = Some(secrets_dir.to_string_lossy().into_owned());
    config.security.testing = Some(TestingConfig {
        allow_missing_secrets: true,
        expose_raw_secrets: true,
        ..Default::default()
    });
    config.agents.search_dirs = vec![agents_dir.to_string_lossy().into_owned()];
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

    let confirmation = Arc::new(TestConfirmation);
    let registry = Arc::new(AgentRegistry::new(config.agents.search_dirs.clone()));
    let executor = build_executor(config.clone(), confirmation, registry).expect("build executor");

    let opening = parse_opening_str(&opening_yaml).expect("parse opening");
    let runner = Runner::new(opening, executor);
    let report = runner.run().await.expect("run opening");
    if !report.trace.success {
        eprintln!("compose_email trace failed: {:#?}", report.trace);
    }
    assert!(report.trace.success, "compose_email trace should succeed");

    let send_record = report
        .node_records
        .iter()
        .find(|rec| rec.node_id == "send")
        .expect("send node present");
    let send_output = send_record
        .attempts
        .last()
        .and_then(|attempt| attempt.output.as_ref())
        .expect("send output present");
    let mail_json = send_output
        .ports
        .get("out")
        .and_then(|values| values.first())
        .cloned()
        .expect("mailer output available");
    let mail: Value = mail_json;
    assert_eq!(mail.get("status").and_then(Value::as_str), Some("dry-run"));
    assert!(
        mail.get("message_id").is_some(),
        "expected message_id in mail result"
    );
    assert!(
        mail.get("delivered_at_ms")
            .and_then(Value::as_u64)
            .is_some(),
        "expected delivered_at_ms in mail result"
    );
}
