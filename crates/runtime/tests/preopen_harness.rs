use std::fs;
use wasmtime::{Engine, Linker, Module, Store};
use wasmtime_wasi::p1;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

const OFLAGS_CREATE: i32 = 1;

const RIGHTS_FD_READ: u64 = 1 << 1;
const RIGHTS_FD_WRITE: u64 = 1 << 6;
const RIGHTS_FD_ALLOCATE: u64 = 1 << 8;
const RIGHTS_PATH_CREATE_FILE: u64 = 1 << 10;
const RIGHTS_PATH_OPEN: u64 = 1 << 13;
const RIGHTS_FD_FILESTAT_SET_SIZE: u64 = 1 << 22;

const READ_RIGHTS: u64 = RIGHTS_FD_READ | RIGHTS_PATH_OPEN;
const WRITE_RIGHTS: u64 = READ_RIGHTS
    | RIGHTS_FD_WRITE
    | RIGHTS_FD_ALLOCATE
    | RIGHTS_FD_FILESTAT_SET_SIZE
    | RIGHTS_PATH_CREATE_FILE;

struct HarnessState {
    wasi: p1::WasiP1Ctx,
}

fn compile_module(engine: &Engine) -> Module {
    let wat = r#"(module
        (import "wasi_snapshot_preview1" "path_open"
            (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "fd_close"
            (func $fd_close (param i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "probe.txt")
        (func (export "try_open") (param $fd i32) (param $oflags i32) (param $rights i64) (result i32)
            (local $ret i32)
            (local $new_fd i32)
            (i32.store (i32.const 32) (i32.const 0))
            (local.set $ret (call $path_open
                (local.get $fd)
                (i32.const 0)
                (i32.const 0)
                (i32.const 9)
                (local.get $oflags)
                (local.get $rights)
                (local.get $rights)
                (i32.const 0)
                (i32.const 32)))
            (if (i32.eqz (local.get $ret))
                (then
                    (local.set $new_fd (i32.load (i32.const 32)))
                    (drop (call $fd_close (local.get $new_fd)))))
            (return (local.get $ret)))
    )"#;
    Module::new(engine, wat).expect("compile module")
}

#[test]
fn preopen_rights_enforced_via_wasmtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ro_dir = temp.path().join("ro");
    let rw_dir = temp.path().join("rw");
    fs::create_dir_all(&ro_dir).expect("mkdir ro");
    fs::create_dir_all(&rw_dir).expect("mkdir rw");
    fs::write(ro_dir.join("probe.txt"), b"readonly").expect("seed ro file");
    let rw_probe = rw_dir.join("probe.txt");
    if rw_probe.exists() {
        fs::remove_file(&rw_probe).expect("clean rw probe");
    }

    let engine = Engine::default();
    let module = compile_module(&engine);

    let mut linker: Linker<HarnessState> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |state: &mut HarnessState| &mut state.wasi)
        .expect("linker add");

    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.arg("probe");
    wasi_builder
        .preopened_dir(
            &ro_dir,
            ro_dir.to_str().unwrap(),
            DirPerms::READ,
            FilePerms::READ,
        )
        .expect("preopen ro");
    wasi_builder
        .preopened_dir(
            &rw_dir,
            rw_dir.to_str().unwrap(),
            DirPerms::all(),
            FilePerms::all(),
        )
        .expect("preopen rw");

    let wasi = wasi_builder.build_p1();
    let mut store = Store::new(&engine, HarnessState { wasi });
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate module");

    let try_open = instance
        .get_typed_func::<(i32, i32, i64), i32>(&mut store, "try_open")
        .expect("export try_open");

    // Preopens are allocated starting at fd 3 in insertion order.
    let errno_ro_read = try_open
        .call(&mut store, (3, 0, READ_RIGHTS as i64))
        .expect("call ro read");
    assert_eq!(errno_ro_read, 0, "ro directory should allow reads");

    let errno_ro_write = try_open
        .call(&mut store, (3, OFLAGS_CREATE, WRITE_RIGHTS as i64))
        .expect("call ro write");
    assert!(errno_ro_write != 0, "ro directory should deny writes");

    let errno_rw_write = try_open
        .call(&mut store, (4, OFLAGS_CREATE, WRITE_RIGHTS as i64))
        .expect("call rw write");
    assert_eq!(errno_rw_write, 0, "rw directory should permit writes");

    // Verify the write actually occurred for completeness.
    assert!(rw_probe.exists(), "rw directory should contain probe file",);
    let content = fs::read(&rw_probe).expect("read probe");
    assert!(content.is_empty(), "probe file should be empty");
}
