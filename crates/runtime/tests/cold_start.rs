use std::fs;
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

use runloop_runtime::{AgentHandle, AgentIdentity, AgentSpec, Runtime};

fn write_wasm(path: &std::path::Path) {
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

fn write_policy(path: &std::path::Path) {
    let mut file = fs::File::create(path).expect("policy file");
    writeln!(
        file,
        "[capabilities]\nfs = []\nnet = []\ntime = false\nkb_read = false\nkb_write = false\nmodel = false\n"
    )
    .expect("write policy");
}

fn wait_for_stdout_contains(handle: &AgentHandle, needle: &[u8], timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    loop {
        let stdout = handle.stdout();
        if stdout.windows(needle.len()).any(|window| window == needle) {
            return stdout;
        }

        if Instant::now() >= deadline {
            panic!(
                "stdout missing {:?} after {:?}: {:?}",
                needle, timeout, stdout
            );
        }

        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn cold_start_p50_under_40ms() {
    let runtime = Runtime::new().expect("runtime");

    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_path = temp.path().join("agent.wasm");
    let policy_path = temp.path().join("policy.caps");
    write_wasm(&wasm_path);
    write_policy(&policy_path);

    let mut durations = Vec::with_capacity(50);

    for i in 0..50 {
        let spec = AgentSpec::builder(AgentIdentity::new(format!("latency-{i}")), &wasm_path)
            .policy_path(&policy_path)
            .build()
            .expect("spec");

        let start = Instant::now();
        let handle = runtime.spawn(spec).expect("spawn");
        durations.push(start.elapsed());

        let _stdout = wait_for_stdout_contains(&handle, b"ready", Duration::from_millis(20));
        let stats = handle.stats().expect("stats");
        assert!(stats.rss_bytes.unwrap_or(0) > 0);

        runtime.kill(handle.id()).expect("kill");
    }

    durations.sort();
    let median = durations[durations.len() / 2];
    assert!(
        median < Duration::from_millis(40),
        "median latency {:?}",
        median
    );
}
