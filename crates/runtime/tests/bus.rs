use bytes::Bytes;
use runloop_bus::{Bus, Message};
use runloop_rmp::Header;
use runloop_runtime::{AgentIdentity, AgentSpec, Error, RuntimeBuilder};
use tempfile::tempdir;

fn write_wasm(path: &std::path::Path) {
    let wasm = wat::parse_str("(module (func (export \"_start\")))").expect("valid wat");
    std::fs::write(path, wasm).expect("write wasm");
}

fn write_policy(path: &std::path::Path) {
    std::fs::write(
        path,
        "[capabilities]\nfs = []\nnet = []\ntime = false\nkb_read = false\nkb_write = false\nmodel = false\n",
    )
    .expect("write policy");
}

#[test]
fn bus_send_errors_propagate() {
    let temp = tempdir().expect("tempdir");
    let bus_path = temp.path().join("bus.sock");

    // Prepare a closed bus handle.
    let bus = {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let mut server = Bus::bind(&bus_path).await.expect("bind bus");
            let bus = Bus::connect(&bus_path).await.expect("connect bus");
            server.close();
            bus
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
