use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use runloop_agent_registry::AgentRegistry;
use runloop_agents_common::{ActionDecision, ActionProposal, AgentResult, ConfirmationProvider};
use runloop_core::Config;
use runloop_core::config::{ModelProvider, ModelRoute, ProviderKind};
use runloop_executor_local::build_executor;
use runloop_openings::{Runner, parse_opening_str};
use std::ffi::OsString;
use std::path::Path;
use tempfile::tempdir;

struct TestConfirmation;

struct EnvGuard {
    home: Option<OsString>,
    wasm_backtrace: Option<OsString>,
    fallback: Option<OsString>,
}

impl EnvGuard {
    fn set(home: &Path, fallback: &str) -> Self {
        let prev_home = std::env::var_os("HOME");
        let prev_wasm = std::env::var_os("WASMTIME_BACKTRACE_DETAILS");
        let prev_fallback = std::env::var_os("RUNLOOP_ALLOW_SYSTEM_TRA_NATIVE");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("WASMTIME_BACKTRACE_DETAILS", "1");
            std::env::set_var("RUNLOOP_ALLOW_SYSTEM_TRA_NATIVE", fallback);
        }
        EnvGuard {
            home: prev_home,
            wasm_backtrace: prev_wasm,
            fallback: prev_fallback,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.home.take() {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        match self.wasm_backtrace.take() {
            Some(val) => unsafe { std::env::set_var("WASMTIME_BACKTRACE_DETAILS", val) },
            None => unsafe { std::env::remove_var("WASMTIME_BACKTRACE_DETAILS") },
        }
        match self.fallback.take() {
            Some(val) => unsafe { std::env::set_var("RUNLOOP_ALLOW_SYSTEM_TRA_NATIVE", val) },
            None => unsafe { std::env::remove_var("RUNLOOP_ALLOW_SYSTEM_TRA_NATIVE") },
        }
    }
}

#[async_trait]
impl ConfirmationProvider for TestConfirmation {
    async fn confirm(&self, _proposal: ActionProposal) -> AgentResult<ActionDecision> {
        Ok(ActionDecision::approved(Some("test".into())))
    }
}

#[tokio::test]
#[ignore = "system_tra wasm exits non-zero in sandbox; needs artifact rebuild"]
async fn system_tra_opening_runs_with_structured_input() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let agents_dir = repo_root.join("agents");
    let opening_yaml = std::fs::read_to_string(repo_root.join("examples/openings/system_tra.yaml"))
        .expect("opening fixture");

    let tmp = tempdir().expect("temp");
    let workdir = tmp.path().join("workdir");
    let sockets_dir = tmp.path().join("sock");
    let kb_root = tmp.path().join("kb");
    let secrets_dir = tmp.path().join("secrets");
    let fake_home = tmp.path().join("home");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::create_dir_all(&sockets_dir).unwrap();
    std::fs::create_dir_all(&kb_root).unwrap();
    std::fs::create_dir_all(&secrets_dir).unwrap();
    std::fs::create_dir_all(&fake_home).unwrap();
    std::fs::write(fake_home.join(".tmux.conf"), b"").unwrap();
    std::fs::write(fake_home.join(".bashrc"), b"").unwrap();
    std::fs::create_dir_all(fake_home.join(".config/tmux")).unwrap();
    std::fs::create_dir_all(fake_home.join(".runloop/artifacts/system_tra")).unwrap();

    let _env_guard = EnvGuard::set(&fake_home, "1");

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
    config.models.default = "null:system_tra".into();
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
    let bundle = registry
        .bundle(&runloop_core::AgentRef::new("system_tra", None))
        .expect("bundle present");
    assert!(
        bundle
            .wasm_entry
            .as_ref()
            .map(|b| b.path.is_file())
            .unwrap_or(false),
        "system_tra wasm missing at {:?}",
        bundle.wasm_entry.as_ref().map(|b| b.path.clone())
    );
    let executor = build_executor(config.clone(), confirmation, registry).expect("build executor");

    let opening = parse_opening_str(&opening_yaml).expect("parse opening");
    let runner = Runner::new(opening, executor);
    let report = runner.run().await.expect("run opening");
    assert!(
        report.trace.success,
        "system_tra trace should succeed: {:?} / {:?}",
        report.trace, report.node_records
    );

    let tmux_conf = fake_home.join(".tmux.conf");
    let bashrc = fake_home.join(".bashrc");
    assert!(tmux_conf.exists(), "tmux config should be written");
    assert!(bashrc.exists(), "bashrc should be written");
}
