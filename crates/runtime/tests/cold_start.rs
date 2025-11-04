use std::fs;
use std::io::Write;
use std::time::{Duration, Instant};

use runloop_runtime::{AgentIdentity, AgentSpec, Runtime};

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

        let stdout = {
            let deadline = Instant::now() + Duration::from_millis(20);
            loop {
                let buf = handle.stdout();
                if std::str::from_utf8(&buf)
                    .map(|s| s.contains("ready"))
                    .unwrap_or(false)
                {
                    break buf;
                }
                if Instant::now() >= deadline {
                    break buf;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let stdout_str = std::str::from_utf8(&stdout).expect("stdout is valid utf-8");
        assert!(
            stdout_str.contains("ready"),
            "stdout missing \"ready\": {stdout_str:?}"
        );
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
