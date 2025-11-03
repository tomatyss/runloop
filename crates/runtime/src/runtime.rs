use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use cfg_if::cfg_if;
use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use wasi_cap_std_sync::WasiCtxBuilder;
use wasi_common::pipe::WritePipe;
use wasmtime::{Engine, Linker, Store};

use crate::audit::AuditSink;
use crate::caps::Caps;
use crate::error::Error;
use crate::module_cache::ModuleCache;
use crate::output::OutputRing;
use crate::spec::{AgentIdentity, AgentSpec};
use crate::stats::{AgentStats, read_stats};
use crate::wasi_dir::CapabilityDir;

use runloop_core::ids::AgentId;

/// Runtime embedding for agent Wasm modules.
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    engine: Engine,
    modules: ModuleCache,
    agents: DashMap<AgentId, Arc<AgentProcess>>,
    _audit: AuditSink,
}

struct AgentProcess {
    _id: AgentId,
    _identity: AgentIdentity,
    stdout_ring: OutputRing,
    stderr_ring: OutputRing,
    _stdout_buffer: Arc<RwLock<Vec<u8>>>,
    _stderr_buffer: Arc<RwLock<Vec<u8>>>,
    _caps: Caps,
    tid: AtomicU32,
    join: Mutex<Option<thread::JoinHandle<Result<(), Error>>>>,
    inbox: mpsc::Sender<Vec<u8>>, // placeholder until bus wiring lands
}

struct StoreData {
    wasi: wasi_common::WasiCtx,
}

impl Runtime {
    /// Construct a new runtime instance with a configured Wasmtime engine.
    pub fn new() -> Result<Self, Error> {
        let mut config = wasmtime::Config::default();
        config
            .wasm_multi_memory(true)
            .wasm_reference_types(true)
            .cranelift_debug_verifier(false)
            .parallel_compilation(true);
        let engine = Engine::new(&config)?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                engine,
                modules: ModuleCache::new(),
                agents: DashMap::new(),
                _audit: AuditSink::default(),
            }),
        })
    }

    /// Spawn a new agent instance. The guest runs on a dedicated OS thread.
    pub fn spawn(&self, spec: AgentSpec) -> Result<AgentHandle, Error> {
        let id = AgentId::new();
        if self.inner.agents.contains_key(&id) {
            return Err(Error::AgentAlreadyExists(id.to_string()));
        }

        let module = self
            .inner
            .modules
            .load(&self.inner.engine, &spec.wasm_path)
            .map_err(|err| Error::spawn_failed(spec.wasm_path.clone(), err.to_string()))?;

        let stdout_ring = OutputRing::new(spec.stdout_capacity);
        let stderr_ring = OutputRing::new(spec.stderr_capacity);
        let stdout_buffer = Arc::new(RwLock::new(Vec::new()));
        let stderr_buffer = Arc::new(RwLock::new(Vec::new()));
        let (inbox_tx, mut inbox_rx) = mpsc::channel::<Vec<u8>>(32);

        let process = Arc::new(AgentProcess {
            _id: id,
            _identity: spec.identity.clone(),
            stdout_ring: stdout_ring.clone(),
            stderr_ring: stderr_ring.clone(),
            _stdout_buffer: stdout_buffer.clone(),
            _stderr_buffer: stderr_buffer.clone(),
            _caps: spec.caps.clone(),
            tid: AtomicU32::new(0),
            join: Mutex::new(None),
            inbox: inbox_tx,
        });

        let engine = self.inner.engine.clone();
        let policy_caps = spec.caps.clone();
        let wasm_path = spec.wasm_path.clone();
        let stdout_writer = RingWriter::new(stdout_ring.clone(), stdout_buffer.clone());
        let stderr_writer = RingWriter::new(stderr_ring.clone(), stderr_buffer.clone());
        let process_for_thread = Arc::clone(&process);

        let join_handle = thread::Builder::new()
            .name(format!("agent-{}", spec.identity.name()))
            .spawn(move || {
                let _tid_guard = TidGuard::new(&process_for_thread.tid);
                let tid = current_thread_id();
                if tid != 0 {
                    process_for_thread.tid.store(tid, Ordering::SeqCst);
                }

                let stdout_pipe = WritePipe::new(stdout_writer);
                let stderr_pipe = WritePipe::new(stderr_writer);

                let mut wasi_builder = WasiCtxBuilder::new();
                if spec.argv.is_empty() {
                    wasi_builder
                        .arg(spec.identity.name())
                        .map_err(|err| Error::Config(format!("invalid argv: {err}")))?;
                } else {
                    for arg in &spec.argv {
                        wasi_builder
                            .arg(arg)
                            .map_err(|err| Error::Config(format!("invalid argv: {err}")))?;
                    }
                }
                if spec.cwd.is_some() && !spec.env.contains_key("PWD") {
                    let pwd = spec.working_dir.as_ref().map(|p| p.as_str()).unwrap_or(".");
                    wasi_builder
                        .env("PWD", pwd)
                        .map_err(|err| Error::Config(format!("invalid env: {err}")))?;
                }

                for (key, value) in &spec.env {
                    wasi_builder
                        .env(key, value)
                        .map_err(|err| Error::Config(format!("invalid env: {err}")))?;
                }

                wasi_builder.stdout(Box::new(stdout_pipe.clone()));
                wasi_builder.stderr(Box::new(stderr_pipe.clone()));
                wasi_builder.stdin(Box::new(wasi_common::pipe::ReadPipe::new(Cursor::new(
                    Vec::new(),
                ))));

                let wasi_ctx = wasi_builder.build();

                if let Some(cwd) = &spec.cwd {
                    if let Some(cap) = policy_caps
                        .fs
                        .iter()
                        .find(|cap| Path::new(cap.root.as_str()) == cwd)
                    {
                        if let Ok(dir) = Dir::open_ambient_dir(cwd, ambient_authority()) {
                            let base_dir: Box<dyn wasi_common::WasiDir> =
                                Box::new(wasi_cap_std_sync::dir::Dir::from_cap_std(dir));
                            let wrapped: Box<dyn wasi_common::WasiDir> = if cap.write {
                                Box::new(CapabilityDir::read_write(base_dir))
                            } else {
                                Box::new(CapabilityDir::read_only(base_dir))
                            };
                            let guest_path =
                                spec.working_dir.as_ref().map(|p| p.as_str()).unwrap_or(".");
                            let _ = wasi_ctx.push_preopened_dir(wrapped, Path::new(guest_path));
                        }
                    }
                }

                for entry in &policy_caps.fs {
                    let host_path = PathBuf::from(entry.root.as_str());
                    let dir = Dir::open_ambient_dir(&host_path, ambient_authority())
                        .map_err(|err| Error::spawn_failed(host_path.clone(), err.to_string()))?;
                    let base_dir: Box<dyn wasi_common::WasiDir> =
                        Box::new(wasi_cap_std_sync::dir::Dir::from_cap_std(dir));
                    let wrapped: Box<dyn wasi_common::WasiDir> = if entry.write {
                        Box::new(CapabilityDir::read_write(base_dir))
                    } else {
                        Box::new(CapabilityDir::read_only(base_dir))
                    };
                    wasi_ctx
                        .push_preopened_dir(wrapped, Path::new(entry.root.as_str()))
                        .map_err(|err| Error::spawn_failed(host_path.clone(), err.to_string()))?;
                }

                let mut linker: Linker<StoreData> = Linker::new(&engine);
                wasmtime_wasi::add_to_linker(&mut linker, |data| &mut data.wasi)
                    .map_err(|err| Error::SpawnFailed(err.to_string()))?;

                let store_data = StoreData { wasi: wasi_ctx };
                let mut store = Store::new(&engine, store_data);
                let instance = linker
                    .instantiate(&mut store, &module)
                    .map_err(|err| Error::spawn_failed(wasm_path.clone(), err.to_string()))?;

                if let Ok(start) = instance.get_typed_func::<(), ()>(&mut store, "_start") {
                    start
                        .call(&mut store, ())
                        .map_err(|err| Error::spawn_failed(wasm_path.clone(), err.to_string()))?;
                }

                // Drain placeholder inbox.
                inbox_rx.close();
                while inbox_rx.blocking_recv().is_some() {}

                Ok(())
            })
            .map_err(|err| Error::spawn_failed(spec.wasm_path.clone(), err.to_string()))?;

        process.join.lock().unwrap().replace(join_handle);

        self.inner.agents.insert(id, process);

        Ok(AgentHandle {
            runtime: self.inner.clone(),
            agent_id: id,
        })
    }

    pub fn kill(&self, agent_id: AgentId) -> Result<(), Error> {
        if let Some((_, process)) = self.inner.agents.remove(&agent_id) {
            if let Some(handle) = process.join.lock().unwrap().take() {
                match handle.join() {
                    Ok(result) => result,
                    Err(_) => Err(Error::AgentJoinFailed("thread panicked".into())),
                }
            } else {
                Ok(())
            }
        } else {
            Err(Error::UnknownAgent)
        }
    }

    pub fn send(&self, agent_id: AgentId, payload: Vec<u8>) -> Result<(), Error> {
        let entry = self
            .inner
            .agents
            .get(&agent_id)
            .ok_or(Error::UnknownAgent)?;
        entry
            .inbox
            .try_send(payload)
            .map_err(|err| Error::SpawnFailed(format!("mailbox full: {err}")))
    }

    pub fn stats(&self, agent_id: AgentId) -> Result<AgentStats, Error> {
        let entry = self
            .inner
            .agents
            .get(&agent_id)
            .ok_or(Error::UnknownAgent)?;
        let tid = entry.tid.load(Ordering::SeqCst);
        let tid = if tid == 0 { None } else { Some(tid) };
        read_stats(tid)
    }

    pub fn handle(&self, agent_id: AgentId) -> Option<AgentHandle> {
        if self.inner.agents.contains_key(&agent_id) {
            Some(AgentHandle {
                runtime: self.inner.clone(),
                agent_id,
            })
        } else {
            None
        }
    }
}

/// Handle exposed to callers for interacting with a spawned agent.
pub struct AgentHandle {
    runtime: Arc<RuntimeInner>,
    agent_id: AgentId,
}

impl AgentHandle {
    pub fn id(&self) -> AgentId {
        self.agent_id
    }

    pub fn stdout(&self) -> Vec<u8> {
        self.runtime
            .agents
            .get(&self.agent_id)
            .map(|p| p.stdout_ring.snapshot())
            .unwrap_or_default()
    }

    pub fn stderr(&self) -> Vec<u8> {
        self.runtime
            .agents
            .get(&self.agent_id)
            .map(|p| p.stderr_ring.snapshot())
            .unwrap_or_default()
    }

    pub fn stats(&self) -> Result<AgentStats, Error> {
        let entry = self
            .runtime
            .agents
            .get(&self.agent_id)
            .ok_or(Error::UnknownAgent)?;
        let tid = entry.tid.load(Ordering::SeqCst);
        let tid = if tid == 0 { None } else { Some(tid) };
        read_stats(tid)
    }
}

/// Writer used to feed stdout/stderr rings while retaining the full buffer.
#[derive(Clone)]
struct RingWriter {
    ring: OutputRing,
    buffer: Arc<RwLock<Vec<u8>>>,
}

impl RingWriter {
    fn new(ring: OutputRing, buffer: Arc<RwLock<Vec<u8>>>) -> Self {
        Self { ring, buffer }
    }
}

impl Write for RingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.ring.push(buf);
        self.buffer.write().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn current_thread_id() -> u32 {
    cfg_if! {
        if #[cfg(target_os = "linux")] {
            rustix::thread::gettid().as_raw_nonzero().map(|nz| nz.get()).unwrap_or(0)
        } else {
            0
        }
    }
}

struct TidGuard<'a> {
    tid: &'a AtomicU32,
}

impl<'a> TidGuard<'a> {
    fn new(tid: &'a AtomicU32) -> Self {
        Self { tid }
    }
}

impl<'a> Drop for TidGuard<'a> {
    fn drop(&mut self) {
        self.tid.store(0, Ordering::SeqCst);
    }
}
