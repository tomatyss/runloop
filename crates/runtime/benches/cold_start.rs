use std::fs;
use std::io::Write;

use criterion::{Criterion, criterion_group, criterion_main};
use runloop_runtime::{AgentIdentity, AgentSpec, Runtime};

fn prepare_wasm(path: &std::path::Path) {
    let wasm = wat::parse_str(
        r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
                (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "ready")
            (func (export "_start")
                (i32.store (i32.const 12) (i32.const 0))
                (i32.store (i32.const 16) (i32.const 5))
                (call $fd_write (i32.const 1) (i32.const 12) (i32.const 1) (i32.const 20))
                drop)
        )"#,
    )
    .expect("valid wat");
    fs::write(path, wasm).expect("write wasm");
}

fn prepare_policy(path: &std::path::Path) {
    let mut file = fs::File::create(path).expect("policy file");
    writeln!(
        file,
        "[capabilities]\nfs = []\nnet = []\ntime = false\nkb_read = false\nkb_write = false\nmodel = false\n"
    )
    .expect("write policy");
}

fn bench_cold_start(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("bench_agent.wasm");
    let policy_path = temp.path().join("bench_policy.caps");
    prepare_wasm(&wasm_path);
    prepare_policy(&policy_path);

    c.bench_function("runtime_spawn_cold_start", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            let spec =
                AgentSpec::builder(AgentIdentity::new(format!("bench-{}", counter)), &wasm_path)
                    .policy_path(&policy_path)
                    .build()
                    .expect("spec");
            let handle = runtime.spawn(spec).expect("spawn");
            runtime.kill(handle.id()).expect("kill");
        });
    });
}

criterion_group!(benches, bench_cold_start);
criterion_main!(benches);
