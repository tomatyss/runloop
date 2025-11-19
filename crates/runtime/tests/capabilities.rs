use std::fs;
use std::io::Write;
use std::sync::Arc;

use runloop_kb::{CapAuditRecord, KnowledgeBase};
use runloop_runtime::{AgentIdentity, AgentSpec, AuditPolicy, Error, RuntimeBuilder};
use serde_json::Value;

fn write_wasm(path: &std::path::Path) {
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

fn write_policy(path: &std::path::Path) {
    let mut file = fs::File::create(path).expect("policy file");
    writeln!(
        file,
        "[capabilities]\nnet = []\nfs = []\ntime = false\nkb_read = false\nkb_write = false\nmodel = false\n"
    )
    .expect("write policy");
}

#[test]
fn time_capability_denied_records_audit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("agent.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_wasm(&wasm_path);
    write_policy(&policy_path);

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
    assert_eq!(audits.len(), 1, "expected single audit record");
    assert_eq!(audits[0].cap, "time.now");
    assert_eq!(audits[0].decision, runloop_kb::AuditDecision::Deny);
    assert_eq!(audits[0].reason, "cap_missing");
    assert_ne!(
        audits[0].args_hash, [0u8; 32],
        "args hash should be populated"
    );
    assert_eq!(audits[0].severity, runloop_kb::AuditSeverity::Warn);

    let rows = kb
        .query_events("SELECT payload_json FROM events WHERE kind = 'cap.audit'")
        .expect("query events");
    assert_eq!(rows.rows.len(), 1, "ledger should contain cap.audit");
    let payload_raw = rows.rows[0]
        .as_object()
        .and_then(|row| row.get("payload_json"))
        .and_then(Value::as_str)
        .expect("payload string");
    let payload_json: Value = serde_json::from_str(payload_raw).expect("payload parses as json");
    assert_eq!(
        payload_json.get("cap"),
        Some(&Value::String("time.now".into()))
    );
    assert_eq!(
        payload_json.get("decision"),
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
    write_wasm(&wasm_path);
    write_policy(&policy_path);

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
