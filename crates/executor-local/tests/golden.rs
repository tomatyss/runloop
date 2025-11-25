use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use runloop_agent_registry::AgentRegistry;
use runloop_agents_common::{ActionDecision, ActionProposal, AgentResult, ConfirmationProvider};
use runloop_core::Config;
use runloop_core::config::{ModelProvider, ModelRoute, ProviderKind};
use runloop_executor_local::build_executor;
use runloop_openings::Runner;
use serde::Deserialize;
use tempfile::tempdir;

struct TestConfirmation;

#[async_trait]
impl ConfirmationProvider for TestConfirmation {
    async fn confirm(&self, _proposal: ActionProposal) -> AgentResult<ActionDecision> {
        Ok(ActionDecision::approved(Some("test".into())))
    }
}

#[derive(Deserialize)]
struct GoldenCase {
    name: String,
    inputs: BTreeMap<String, String>,
    expectations: Expectations,
}

#[derive(Deserialize)]
struct Expectations {
    recipient_email: String,
    min_words: usize,
    max_words: usize,
    has_citations: bool,
}

#[tokio::test]
#[ignore] // Run with: cargo test -- --ignored golden
async fn golden_compose_email() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let inputs_path = repo_root.join("tests/golden/compose_email/inputs.json");
    let inputs_json = std::fs::read_to_string(&inputs_path).expect("inputs.json");
    let cases: Vec<GoldenCase> = serde_json::from_str(&inputs_json).expect("parse inputs");

    let agents_dir = repo_root.join("agents");
    let opening_yaml =
        std::fs::read_to_string(repo_root.join("examples/openings/compose_email.yaml"))
            .expect("opening fixture");

    for case in cases {
        println!("Running golden case: {}", case.name);

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
        config.agents.search_dirs = vec![agents_dir.to_string_lossy().into_owned()];
        // Use local (null) provider
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

        // build_executor seeds the KB with John Smith
        let executor =
            build_executor(config.clone(), confirmation, registry).expect("build executor");

        // Parse YAML to Value to modify params
        let mut doc: serde_yaml::Value = serde_yaml::from_str(&opening_yaml).expect("parse yaml");
        if let Some(params) = doc.get_mut("params").and_then(|v| v.as_mapping_mut()) {
            for (k, v) in &case.inputs {
                let key = serde_yaml::Value::String(k.clone());
                match params.get_mut(&key) {
                    Some(existing) if existing.is_mapping() => {
                        let mapping = existing.as_mapping_mut().expect("mapping");
                        mapping.insert(
                            serde_yaml::Value::String("default".into()),
                            serde_yaml::Value::String(v.clone()),
                        );
                    }
                    Some(existing) => {
                        *existing = serde_yaml::Value::String(v.clone());
                    }
                    None => {
                        params.insert(key, serde_yaml::Value::String(v.clone()));
                    }
                }
            }
        }
        let modified_yaml = serde_yaml::to_string(&doc).expect("serialize yaml");

        let opening = runloop_openings::parse_opening_str(&modified_yaml).expect("parse opening");

        let runner = Runner::new(opening, executor);
        let report = runner.run().await.expect("run opening");

        assert!(
            report.trace.success,
            "Opening run failed for case: {}",
            case.name
        );

        // Verify expectations
        verify_outputs(&report, &case.expectations);
    }
}

fn verify_outputs(report: &runloop_openings::RunReport, expectations: &Expectations) {
    // 1. Check recipient email from contacts node
    let contacts_node = report
        .trace
        .nodes
        .iter()
        .find(|n| n.node_id == "contacts")
        .expect("contacts node trace");

    let contact_attempt = contacts_node
        .final_attempt
        .as_ref()
        .expect("contacts attempt");
    let contact_outputs = contact_attempt.outputs.as_ref().expect("contacts outputs");

    let contact_out = contact_outputs
        .ports
        .get("out")
        .and_then(|vals| vals.first())
        .expect("contacts output");

    let contact_email = contact_out
        .get("email")
        .and_then(|v| v.as_str())
        .expect("contact email");

    assert_eq!(
        contact_email, expectations.recipient_email,
        "Recipient email mismatch"
    );

    // 2. Check draft properties
    let draft_node = report
        .trace
        .nodes
        .iter()
        .find(|n| n.node_id == "draft")
        .expect("draft node trace");

    let draft_attempt = draft_node.final_attempt.as_ref().expect("draft attempt");
    let draft_outputs = draft_attempt.outputs.as_ref().expect("draft outputs");

    let draft_out = draft_outputs
        .ports
        .get("out")
        .and_then(|vals| vals.first())
        .expect("draft output");

    let word_count = draft_out
        .get("word_count")
        .and_then(|v| v.as_u64())
        .expect("word count") as usize;

    assert!(
        word_count >= expectations.min_words,
        "Word count {} < min {}",
        word_count,
        expectations.min_words
    );
    assert!(
        word_count <= expectations.max_words,
        "Word count {} > max {}",
        word_count,
        expectations.max_words
    );

    let citations = draft_out
        .get("citations")
        .and_then(|v| v.as_array())
        .expect("citations");

    if expectations.has_citations {
        assert!(!citations.is_empty(), "Expected citations but found none");
    } else {
        assert!(citations.is_empty(), "Expected no citations but found some");
    }
}
