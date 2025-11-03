use std::fs;
use std::io::Write;
use std::path::PathBuf;

use runloop_runtime::{AgentIdentity, AgentSpec, Runtime};

fn write_wasm(path: &PathBuf) {
    let wasm = wat::parse_str("(module (func (export \"_start\")))").expect("valid wat");
    fs::write(path, wasm).expect("write wasm");
}

fn write_policy(path: &PathBuf) {
    let mut file = fs::File::create(path).expect("policy file");
    writeln!(
        file,
        "[capabilities]\nnet = []\nfs = []\ntime = false\nkb_read = false\nkb_write = false\nmodel = false\n"
    )
    .expect("write policy");
}

#[test]
fn spawn_trivial_agent() {
    let runtime = Runtime::new().expect("runtime");

    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("agent.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_wasm(&wasm_path);
    write_policy(&policy_path);

    let spec = AgentSpec::builder(AgentIdentity::new("smoke"), &wasm_path)
        .policy_path(&policy_path)
        .build()
        .expect("spec");

    let handle = runtime.spawn(spec).expect("spawn");
    runtime.kill(handle.id()).expect("kill");
}
