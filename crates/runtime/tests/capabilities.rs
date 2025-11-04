use std::fs;
use std::io::Write;
use std::sync::Arc;

use runloop_kb::{CapAuditRecord, KnowledgeBase};
use runloop_runtime::{AgentIdentity, AgentSpec, Error, RuntimeBuilder};

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

    let stats = runtime.hostcall_stats();
    assert!(stats.denied() >= 1);
}
