use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use runloop_kb::{CapAuditRecord, KnowledgeBase};
use runloop_runtime::{AgentIdentity, AgentSpec, AuditPolicy, Error, RuntimeBuilder};
use serde_json::Value;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn write_time_wasm(path: &std::path::Path) {
    let wasm = wat::parse_str(
        r#"(module
            (import "runloop" "time_now" (func $time_now (result i64)))
            (func (export "_start")
                (drop (call $time_now))
            )
        )"#,
    )
    .expect("valid wat");
    fs::write(path, wasm).expect("write wasm");
}

fn write_notify_only_wasm(path: &std::path::Path) {
    let wasm = wat::parse_str(
        r#"(module
            (import "runloop" "notify_ready" (func $notify_ready))
            (memory (export "memory") 1)
            (func (export "_start")
                call $notify_ready
            )
        )"#,
    )
    .expect("valid wat");
    fs::write(path, wasm).expect("write wasm");
}

fn write_deny_policy(path: &std::path::Path) {
    let mut file = fs::File::create(path).expect("policy file");
    writeln!(
        file,
        "[capabilities]\nnet = []\nfs = []\ntime = false\nkb_read = false\nkb_write = false\nmodel = false\n"
    )
    .expect("write policy");
}

fn write_empty_policy(path: &std::path::Path) {
    let mut file = fs::File::create(path).expect("policy file");
    writeln!(file, "[capabilities]").expect("write policy");
}

fn write_time_enabled_policy(path: &std::path::Path) {
    let mut file = fs::File::create(path).expect("policy file");
    writeln!(file, "[capabilities]\ntime = true\nmodel = true\n").expect("write policy");
}

#[inline]
fn set_var_portable(key: &str, value: &OsStr) {
    #[allow(unused_unsafe)]
    unsafe {
        env::set_var(key, value);
    }
}

#[inline]
fn remove_var_portable(key: &str) {
    #[allow(unused_unsafe)]
    unsafe {
        env::remove_var(key);
    }
}

/// Set an env var for the duration of `f`, restoring the previous value.
fn with_scoped_env_var<F, R>(key: &str, value: &Path, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let previous = env::var_os(key);
    set_var_portable(key, value.as_os_str());
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    match previous {
        Some(val) => set_var_portable(key, &val),
        None => remove_var_portable(key),
    }
    result.unwrap_or_else(|panic| panic::resume_unwind(panic))
}

#[test]
fn time_capability_denied_records_audit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("agent.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_time_wasm(&wasm_path);
    write_deny_policy(&policy_path);

    let kb = Arc::new(KnowledgeBase::new());
    let runtime = RuntimeBuilder::new()
        .knowledge_base(kb.clone())
        .build()
        .expect("runtime");

    let spec = AgentSpec::builder(AgentIdentity::new("cap-deny"), &wasm_path)
        .policy_path(&policy_path)
        .build()
        .expect("spec");
    assert!(
        !spec.caps.time,
        "policy expected to disable time capability"
    );

    let result = runtime.spawn(spec);
    match result {
        Err(Error::CapDenied(_)) => {}
        Err(other) => panic!("unexpected spawn error: {other:?}"),
        Ok(handle) => {
            let kill_result = runtime.kill(handle.id());
            assert!(
                matches!(kill_result, Err(Error::CapDenied(_))),
                "expected kill to surface capability denial, got {kill_result:?}"
            );
        }
    }

    let audits: Vec<CapAuditRecord> = kb.cap_audits();
    assert_eq!(audits.len(), 2, "expected empty-caps and hostcall denials");
    assert!(
        audits
            .iter()
            .any(|a| a.cap == "caps.empty" && a.reason == "caps_empty"),
        "caps.empty audit missing"
    );
    assert!(
        audits
            .iter()
            .any(|a| a.cap == "time.now" && a.reason == "cap_missing"),
        "time.now denial audit missing"
    );

    let rows = kb
        .query_events("SELECT payload_json FROM events WHERE kind = 'cap.audit'")
        .expect("query events");
    assert_eq!(rows.rows.len(), 2, "ledger should contain both audits");
    let payloads: Vec<Value> = rows
        .rows
        .iter()
        .map(|row| {
            row.as_object()
                .and_then(|obj| obj.get("payload_json"))
                .and_then(Value::as_str)
                .map(|raw| serde_json::from_str(raw).expect("payload parses as json"))
                .expect("payload string")
        })
        .collect();
    let time_audit = payloads
        .iter()
        .find(|payload| payload.get("cap") == Some(&Value::String("time.now".into())))
        .expect("time.now audit present");
    assert_eq!(
        time_audit.get("decision"),
        Some(&Value::String("deny".into()))
    );

    let stats = runtime.hostcall_stats();
    assert!(stats.denied() >= 1);
}

#[test]
fn audit_policy_can_disable_persistence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("agent.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_time_wasm(&wasm_path);
    write_deny_policy(&policy_path);

    let kb = Arc::new(KnowledgeBase::new());
    let runtime = RuntimeBuilder::new()
        .knowledge_base(kb.clone())
        .audit_policy(AuditPolicy::new(false, false))
        .build()
        .expect("runtime");

    let spec = AgentSpec::builder(AgentIdentity::new("cap-deny"), &wasm_path)
        .policy_path(&policy_path)
        .build()
        .expect("spec");

    let _ = runtime.spawn(spec);

    let audits: Vec<CapAuditRecord> = kb.cap_audits();
    assert!(
        audits.is_empty(),
        "in-memory audit snapshot should be empty when disabled"
    );

    let rows = kb
        .query_events("SELECT payload_json FROM events WHERE kind = 'cap.audit'")
        .expect("query events");
    assert_eq!(
        rows.rows.len(),
        0,
        "ledger should not record cap.audit when disabled"
    );
}

#[test]
fn empty_caps_emit_launch_audit_and_allow_spawn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("agent_notify.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_notify_only_wasm(&wasm_path);
    write_empty_policy(&policy_path);

    let kb = Arc::new(KnowledgeBase::new());
    let runtime = RuntimeBuilder::new()
        .knowledge_base(kb.clone())
        .build()
        .expect("runtime");

    let spec = AgentSpec::builder(AgentIdentity::new("inert-agent"), &wasm_path)
        .policy_path(&policy_path)
        .build()
        .expect("spec");

    let handle = runtime.spawn(spec).expect("spawn succeeds with empty caps");
    let _ = runtime.kill(handle.id());

    let audits = kb.cap_audits();
    assert_eq!(audits.len(), 1, "only caps.empty audit expected");
    let audit = &audits[0];
    assert_eq!(audit.cap, "caps.empty");
    assert_eq!(audit.reason, "caps_empty");
    assert_eq!(audit.op, "_start");
    assert_eq!(audit.decision, runloop_kb::AuditDecision::Deny);
    assert_eq!(audit.severity, runloop_kb::AuditSeverity::Warn);
}

#[test]
fn override_to_empty_caps_emits_audit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("agent_notify.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_notify_only_wasm(&wasm_path);
    write_time_enabled_policy(&policy_path);

    with_scoped_env_var("HOME", temp.path(), || {
        // Arrange user override that strips all capabilities.
        let override_path = temp
            .path()
            .join(".runloop/policy-overrides/override_agent/policy.caps");
        fs::create_dir_all(override_path.parent().expect("override parent exists"))
            .expect("create override dir");
        write_deny_policy(&override_path);

        let kb = Arc::new(KnowledgeBase::new());
        let runtime = RuntimeBuilder::new()
            .knowledge_base(kb.clone())
            .build()
            .expect("runtime");

        let spec = AgentSpec::builder(AgentIdentity::new("override-agent"), &wasm_path)
            .policy_path(&policy_path)
            .build()
            .expect("spec");

        let handle = runtime.spawn(spec).expect("spawn succeeds with override");
        let _ = runtime.kill(handle.id());

        let audits = kb.cap_audits();
        assert_eq!(
            audits.len(),
            1,
            "only caps.empty audit expected after override"
        );
        assert_eq!(audits[0].reason, "caps_empty");
    });
}
