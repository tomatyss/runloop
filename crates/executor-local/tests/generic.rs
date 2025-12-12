use std::collections::BTreeMap;
use std::fs;
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
async fn generic_agent_runs_via_registry() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let source_wasm = repo_root.join("agents/contact_resolver/bin/contact_resolver.wasm");
    let source_policy = repo_root.join("agents/contact_resolver/policy.caps");

    // Skip gracefully if WASM hasn't been built yet
    if !source_wasm.exists() {
        eprintln!(
            "Skipping generic_agent_runs_via_registry: WASM not found at {:?}\n\
             Run `just build-agents-wasm` to build the required artifacts.",
            source_wasm
        );
        return;
    }

    let tmp = tempdir().expect("temp dir");
    let agent_root = tmp.path().join("agents");
    let bundle_dir = agent_root.join("demo_contact");
    let bin_dir = bundle_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let wasm_path = bin_dir.join("contact_resolver.wasm");
    fs::copy(&source_wasm, &wasm_path).expect("copy wasm");
    fs::copy(&source_policy, bundle_dir.join("policy.caps")).expect("copy caps");
    let wasm_digest = runloop_agent_registry::digest_file_hex(&wasm_path).expect("wasm digest");

    let manifest = format!(
        r#"[agent]
name = "demo_contact"
version = "0.1.0"
kind = "wasm32-wasip1"

entry_wasm = {{ path = "bin/contact_resolver.wasm", blake3 = "{wasm_digest}" }}

[ports]
in = ["contact.query.v1"]
out = ["out"]

[caps]
file = "policy.caps"
"#
    );
    fs::write(bundle_dir.join("manifest.toml"), manifest).expect("write manifest");

    let opening_yaml = r#"version: 0
name: generic_contact
nodes:
  - id: resolver
    use: agent:demo_contact
    with:
      query: "John"
edges: []
success:
  any_of:
    - exists(resolver.out)
"#;

    let workdir = tmp.path().join("workdir");
    let sockets_dir = tmp.path().join("sock");
    let kb_root = tmp.path().join("kb");
    let secrets_dir = tmp.path().join("secrets");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&sockets_dir).unwrap();
    fs::create_dir_all(&kb_root).unwrap();
    fs::create_dir_all(&secrets_dir).unwrap();

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
        broker_mode: None,
        broker_seed: None,
        allow_missing_secrets: true,
        expose_raw_secrets: true,
    });
    config.agents.search_dirs = vec![agent_root.to_string_lossy().into_owned()];
    config.models.default = "null:generic".into();
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

    let opening = parse_opening_str(opening_yaml).expect("parse opening");
    let runner = Runner::new(opening, executor);
    let report = runner.run().await.expect("run opening");
    if !report.trace.success {
        eprintln!("generic agent trace failed: {:#?}", report.trace);
    }
    assert!(report.trace.success, "generic agent should succeed");
    let resolver_record = report
        .node_records
        .iter()
        .find(|rec| rec.node_id == "resolver")
        .expect("resolver record");
    let output = resolver_record
        .attempts
        .last()
        .and_then(|attempt| attempt.output.as_ref())
        .and_then(|out| out.ports.get("out"))
        .and_then(|values| values.first())
        .cloned()
        .expect("resolver output");
    let json: Value = output;
    let email = json
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(email.contains("acme.com"), "expected email in output");
}
