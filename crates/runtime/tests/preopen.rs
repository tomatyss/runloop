use std::fs;
use std::io::Write;
use std::time::{Duration, Instant};

use runloop_runtime::{AgentHandle, AgentIdentity, AgentSpec, Runtime};

fn write_preopen_listing_wasm(path: &std::path::Path) {
    let wat = wat::parse_str(
        r#"(module
            (import "wasi_snapshot_preview1" "fd_prestat_get"
                (func $fd_prestat_get (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_prestat_dir_name"
                (func $fd_prestat_dir_name (param i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_write"
                (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 2)
            (data (i32.const 0) "\n")
            (func $write (param $ptr i32) (param $len i32)
                (i32.store (i32.const 704) (local.get $ptr))
                (i32.store (i32.const 708) (local.get $len))
                (drop (call $fd_write (i32.const 1) (i32.const 704) (i32.const 1) (i32.const 720))))
            (func (export "_start")
                (local $fd i32)
                (local $ret i32)
                (local $name_len i32)
                (local.set $fd (i32.const 3))
                (block $exit
                    (loop $scan
                        (local.set $ret (call $fd_prestat_get (local.get $fd) (i32.const 512)))
                        (if (i32.ne (local.get $ret) (i32.const 0))
                            (then (br $exit)))
                        (local.set $name_len (i32.load (i32.const 516)))
                        (drop (call $fd_prestat_dir_name (local.get $fd) (i32.const 544) (local.get $name_len)))
                        (call $write (i32.const 544) (local.get $name_len))
                        (call $write (i32.const 0) (i32.const 1))
                        (local.set $fd (i32.add (local.get $fd) (i32.const 1)))
                        (br $scan))))
        )"#,
    )
    .expect("valid wat");
    fs::write(path, wat).expect("write wasm");
}

fn wait_for_lines(handle: &AgentHandle, expected: usize, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let stdout = handle.stdout();
        if let Ok(output) = String::from_utf8(stdout.clone()) {
            let lines: Vec<String> = output
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect();
            if lines.len() >= expected {
                return lines;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "stdout did not contain {expected} lines within {:?}",
                timeout
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn wasi_preopen_names_match_caps() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rw_root = temp.path().join("rw");
    let ro_root = temp.path().join("ro");
    fs::create_dir_all(&rw_root).expect("mkdir rw");
    fs::create_dir_all(&ro_root).expect("mkdir ro");

    let wasm_path = temp.path().join("list.wasm");
    write_preopen_listing_wasm(&wasm_path);

    let policy_path = temp.path().join("policy.caps");
    let mut file = fs::File::create(&policy_path).expect("policy file");
    writeln!(
        file,
        "[capabilities]\nfs_ro = [\"{}\"]\nfs_rw = [\"{}\"]",
        ro_root.display(),
        rw_root.display()
    )
    .expect("write policy");

    let runtime = Runtime::new().expect("runtime");
    let spec = AgentSpec::builder(AgentIdentity::new("preopen-list"), &wasm_path)
        .policy_path(&policy_path)
        .build()
        .expect("spec");

    assert_eq!(
        spec.caps.fs.len(),
        2,
        "expected two filesystem capabilities"
    );

    let handle = runtime.spawn(spec).expect("spawn");
    let mut actual = wait_for_lines(&handle, 2, Duration::from_millis(100));
    actual.sort();

    let mut expected = vec![ro_root.display().to_string(), rw_root.display().to_string()];
    expected.sort();

    assert_eq!(actual, expected, "preopen listing mismatch");

    runtime.kill(handle.id()).expect("kill");
}
