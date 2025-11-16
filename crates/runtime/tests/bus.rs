use bytes::Bytes;
use runloop_bus::{Bus, Message};
use runloop_rmp::Header;
use runloop_runtime::{AgentIdentity, AgentSpec, Error, RuntimeBuilder};
use tempfile::tempdir;

fn write_wasm(path: &std::path::Path) {
    let wasm = wat::parse_str(
        r#"(module
            (import "runloop" "notify_ready" (func $notify_ready))
            (func (export "_start")
                (call $notify_ready)
            )
        )"#,
    )
    .expect("valid wat");
    std::fs::write(path, wasm).expect("write wasm");
}

fn write_policy(path: &std::path::Path) {
    std::fs::write(
        path,
        "[capabilities]\nfs = []\nnet = []\ntime = false\nkb_read = false\nkb_write = false\nmodel = false\n",
    )
    .expect("write policy");
}

fn current_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[test]
fn bus_send_errors_propagate() {
    let temp = tempdir().expect("tempdir");
    let bus_path = temp.path().join("bus.sock");

    // Prepare a closed bus handle.
    let (bus, mut server) = {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let server = Bus::bind(&bus_path).await.expect("bind bus");
            let bus = Bus::connect(&bus_path).await.expect("connect bus");
            (bus, server)
        })
    };

    let runtime = RuntimeBuilder::new().bus(bus).build().expect("runtime");

    let wasm_path = temp.path().join("agent.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_wasm(&wasm_path);
    write_policy(&policy_path);

    let spec = AgentSpec::builder(AgentIdentity::new("bus-test"), &wasm_path)
        .policy_path(&policy_path)
        .build()
        .expect("spec");
    let handle = runtime.spawn(spec).expect("spawn");

    // Drop the server after the agent is running so subsequent sends fail.
    server.close();

    let header = Header {
        msg_id: 1,
        ..Header::default()
    };
    let message = Message::new(header, Bytes::from_static(b"")).expect("message");

    let err = runtime
        .send(handle.id(), message)
        .expect_err("send should fail");
    assert!(matches!(err, Error::SpawnFailed(_)));

    runtime.kill(handle.id()).expect("kill");
}

#[test]
fn bus_send_inside_runtime_does_not_panic() {
    let temp = tempdir().expect("tempdir");
    let bus_path = temp.path().join("reentrant.sock");

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (bus, mut server) = rt.block_on(async {
        let server = Bus::bind(&bus_path).await.expect("bind bus");
        let bus = Bus::connect(&bus_path).await.expect("connect bus");
        (bus, server)
    });

    let runtime = RuntimeBuilder::new()
        .bus(bus)
        .async_handle(rt.handle().clone())
        .build()
        .expect("runtime");

    let wasm_path = temp.path().join("agent.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_wasm(&wasm_path);
    write_policy(&policy_path);

    let spec = AgentSpec::builder(AgentIdentity::new("bus-nested"), &wasm_path)
        .policy_path(&policy_path)
        .build()
        .expect("spec");
    let handle = runtime.spawn(spec).expect("spawn");

    let mut header = Header::default();
    header.msg_id = 7;
    header.created_at_ms = current_millis();
    header.ttl_ms = 60_000;
    let message = Message::new(header, Bytes::from_static(b"abc")).expect("message");

    let runtime_ref = &runtime;
    let agent_id = handle.id();
    rt.block_on(async move {
        runtime_ref
            .send(agent_id, message)
            .expect("send should succeed without panic");
    });

    server.close();
    runtime.kill(handle.id()).expect("kill");
}
