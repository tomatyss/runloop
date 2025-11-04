use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use cfg_if::cfg_if;
use dashmap::DashMap;
use futures_util::StreamExt;
use parking_lot::RwLock;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TokioJoinHandle;
use wasi_cap_std_sync::WasiCtxBuilder;
use wasi_common::pipe::WritePipe;
use wasmtime::{Engine, Linker, Store, Trap};

use crate::audit::AuditSink;
use crate::caps::Caps;
use crate::error::Error;
use crate::hostcalls::{self, AgentMailbox, HostState, HostcallStats, StoreData};
use crate::module_cache::ModuleCache;
use crate::output::OutputRing;
use crate::secrets::{SecretProvider, SecretStore};
use crate::spec::{AgentIdentity, AgentSpec};
use crate::stats::{AgentStats, read_stats};
use crate::wasi_dir::CapabilityDir;

use runloop_bus::{Bus, Message};
use runloop_core::ids::AgentId;
use runloop_kb::KnowledgeBase;
use runloop_model_broker::Broker;

/// Runtime embedding for agent Wasm modules.
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    engine: Engine,
    modules: ModuleCache,
    agents: DashMap<AgentId, Arc<AgentProcess>>,
    audit: AuditSink,
    kb: Arc<KnowledgeBase>,
    broker: Arc<Broker>,
    secrets: Arc<dyn SecretProvider>,
    hostcall_stats: Arc<HostcallStats>,
    bus: Option<Bus>,
    async_spawner: Option<AsyncSpawner>,
}

struct AsyncSpawner {
    handle: TokioHandle,
    _runtime: Option<Arc<tokio::runtime::Runtime>>,
}

impl AsyncSpawner {
    fn new(handle: TokioHandle, runtime: Option<Arc<tokio::runtime::Runtime>>) -> Self {
        Self {
            handle,
            _runtime: runtime,
        }
    }

    fn spawn<F>(&self, fut: F) -> TokioJoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(fut)
    }

    fn block_on<F, T>(&self, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.handle.block_on(fut)
    }
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
    inbox: mpsc::Sender<Vec<u8>>,
    mailbox: Arc<AgentMailbox>,
    bus_task: Mutex<Option<TokioJoinHandle<()>>>,
    _host_state: Arc<HostState>,
}

enum StartupSignal {
    Ready,
    Failed { cap_denied: bool, message: String },
}

/// Builder for configuring runtime dependencies.
pub struct RuntimeBuilder {
    kb: Arc<KnowledgeBase>,
    broker: Arc<Broker>,
    secrets: Arc<dyn SecretProvider>,
    bus: Option<Bus>,
    audit_capacity: usize,
    async_handle: Option<TokioHandle>,
}

impl RuntimeBuilder {
    #[must_use]
    pub fn new() -> Self {
        let secrets: Arc<dyn SecretProvider> = Arc::new(SecretStore::new());
        Self {
            kb: Arc::new(KnowledgeBase::new()),
            broker: Arc::new(Broker::new()),
            secrets,
            bus: None,
            audit_capacity: 512,
            async_handle: None,
        }
    }

    #[must_use]
    pub fn knowledge_base(mut self, kb: Arc<KnowledgeBase>) -> Self {
        self.kb = kb;
        self
    }

    #[must_use]
    pub fn model_broker(mut self, broker: Arc<Broker>) -> Self {
        self.broker = broker;
        self
    }

    #[must_use]
    pub fn secrets(mut self, secrets: Arc<dyn SecretProvider>) -> Self {
        self.secrets = secrets;
        self
    }

    #[must_use]
    pub fn bus(mut self, bus: Bus) -> Self {
        self.bus = Some(bus);
        self
    }

    #[must_use]
    pub fn audit_capacity(mut self, capacity: usize) -> Self {
        self.audit_capacity = capacity.max(1);
        self
    }

    #[must_use]
    pub fn async_handle(mut self, handle: TokioHandle) -> Self {
        self.async_handle = Some(handle);
        self
    }

    pub fn build(self) -> Result<Runtime, Error> {
        let mut config = wasmtime::Config::default();
        config
            .wasm_multi_memory(true)
            .wasm_reference_types(true)
            .cranelift_debug_verifier(false)
            .parallel_compilation(true);
        let engine = Engine::new(&config)?;

        let audit = AuditSink::new(self.audit_capacity);
        let hostcall_stats = Arc::new(HostcallStats::new());

        let async_spawner = match self.async_handle {
            Some(handle) => Some(AsyncSpawner::new(handle, None)),
            None => match TokioHandle::try_current() {
                Ok(handle) => Some(AsyncSpawner::new(handle, None)),
                Err(_) => {
                    let runtime = Arc::new(
                        tokio::runtime::Builder::new_multi_thread()
                            .enable_all()
                            .build()
                            .map_err(|err| {
                                Error::Config(format!("async runtime init failed: {err}"))
                            })?,
                    );
                    let handle = runtime.handle().clone();
                    Some(AsyncSpawner::new(handle, Some(runtime)))
                }
            },
        };

        let inner = RuntimeInner {
            engine,
            modules: ModuleCache::new(),
            agents: DashMap::new(),
            audit,
            kb: self.kb,
            broker: self.broker,
            secrets: self.secrets,
            hostcall_stats,
            bus: self.bus,
            async_spawner,
        };

        Ok(Runtime {
            inner: Arc::new(inner),
        })
    }
}

impl Runtime {
    /// Construct a new runtime instance with a configured Wasmtime engine.
    pub fn new() -> Result<Self, Error> {
        RuntimeBuilder::new().build()
    }

    /// Spawn a new agent instance. The guest runs on a dedicated OS thread.

    pub fn spawn(&self, mut spec: AgentSpec) -> Result<AgentHandle, Error> {
        spec.sanitize();
        let id = AgentId::new();
        if self.inner.agents.contains_key(&id) {
            return Err(Error::AgentAlreadyExists(id.to_string()));
        }

        let wasm_path = spec.wasm_path.clone();
        let module = self
            .inner
            .modules
            .load(&self.inner.engine, &spec.wasm_path)
            .map_err(|err| Error::spawn_failed(wasm_path.clone(), err.to_string()))?;

        let stdout_ring = OutputRing::new(spec.stdout_capacity);
        let stderr_ring = OutputRing::new(spec.stderr_capacity);
        let stdout_buffer = Arc::new(RwLock::new(Vec::new()));
        let stderr_buffer = Arc::new(RwLock::new(Vec::new()));
        let (inbox_tx, inbox_rx) = mpsc::channel::<Vec<u8>>(32);
        let mailbox = Arc::new(AgentMailbox::new(inbox_rx));
        let identity_label = match spec.identity.variant() {
            Some(var) => format!("{}:{var}", spec.identity.name()),
            None => spec.identity.name().to_string(),
        };

        let host_state = Arc::new(HostState::new(
            spec.caps.clone(),
            self.inner.audit.clone(),
            self.inner.kb.clone(),
            self.inner.broker.clone(),
            self.inner.secrets.clone(),
            self.inner.hostcall_stats.clone(),
            id,
            identity_label.clone(),
        ));

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
            inbox: inbox_tx.clone(),
            mailbox: mailbox.clone(),
            bus_task: Mutex::new(None),
            _host_state: host_state.clone(),
        });

        let engine = self.inner.engine.clone();
        let policy_caps = spec.caps.clone();
        let wasm_path = spec.wasm_path.clone();
        let stdout_writer = RingWriter::new(stdout_ring.clone(), stdout_buffer.clone());
        let stderr_writer = RingWriter::new(stderr_ring.clone(), stderr_buffer.clone());
        let process_for_thread = Arc::clone(&process);
        let host_state_for_thread = host_state.clone();
        let mailbox_for_thread = mailbox.clone();
        let module_for_thread = module.clone();
        let (startup_tx, startup_rx) = std_mpsc::channel();

        let join_handle = thread::Builder::new()
            .name(format!("agent-{}", spec.identity.name()))
            .spawn(move || {
                let result = (|| -> Result<(), Error> {
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

                    let mut wasi_ctx = wasi_builder.build();

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
                        let dir = Dir::open_ambient_dir(&host_path, ambient_authority()).map_err(
                            |err| Error::spawn_failed(host_path.clone(), err.to_string()),
                        )?;
                        let base_dir: Box<dyn wasi_common::WasiDir> =
                            Box::new(wasi_cap_std_sync::dir::Dir::from_cap_std(dir));
                        let wrapped: Box<dyn wasi_common::WasiDir> = if entry.write {
                            Box::new(CapabilityDir::read_write(base_dir))
                        } else {
                            Box::new(CapabilityDir::read_only(base_dir))
                        };
                        wasi_ctx
                            .push_preopened_dir(wrapped, Path::new(entry.root.as_str()))
                            .map_err(|err| {
                                Error::spawn_failed(host_path.clone(), err.to_string())
                            })?;
                    }

                    let mut linker: Linker<StoreData> = Linker::new(&engine);
                    wasmtime_wasi::add_to_linker(&mut linker, |data| &mut data.wasi)
                        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
                    hostcalls::add_to_linker(&mut linker)?;

                    let store_data = StoreData {
                        wasi: wasi_ctx,
                        state: host_state_for_thread,
                        mailbox: mailbox_for_thread,
                    };
                    let mut store = Store::new(&engine, store_data);
                    let instance = linker
                        .instantiate(&mut store, &module_for_thread)
                        .map_err(|err| Error::spawn_failed(wasm_path.clone(), err.to_string()))?;

                    let _ = startup_tx.send(StartupSignal::Ready);

                    if let Ok(start) = instance.get_typed_func::<(), ()>(&mut store, "_start") {
                        if let Err(err) = start.call(&mut store, ()) {
                            let mapped = map_trap_error(err, &wasm_path);
                            let cap_denied = matches!(mapped, Error::CapDenied(_));
                            let message = mapped.to_string();
                            let _ = startup_tx.send(StartupSignal::Failed {
                                cap_denied,
                                message,
                            });
                            return Err(mapped);
                        }
                    }

                    Ok(())
                })();
                if let Err(err) = &result {
                    let _ = startup_tx.send(StartupSignal::Failed {
                        cap_denied: matches!(err, Error::CapDenied(_)),
                        message: err.to_string(),
                    });
                }

                result
            })
            .map_err(|err| Error::spawn_failed(wasm_path.clone(), err.to_string()))?;

        let startup_status = startup_rx
            .recv()
            .map_err(|_| Error::spawn_failed(wasm_path.clone(), "startup channel closed".into()))?;
        let mut failure = match startup_status {
            StartupSignal::Ready => match startup_rx.try_recv() {
                Ok(StartupSignal::Failed {
                    cap_denied,
                    message,
                }) => Some((cap_denied, message)),
                _ => None,
            },
            StartupSignal::Failed {
                cap_denied,
                message,
            } => Some((cap_denied, message)),
        };
        if let Some((cap_denied, message)) = failure.take() {
            let err = if cap_denied {
                Error::CapDenied(message)
            } else {
                Error::spawn_failed(wasm_path.clone(), message)
            };
            match join_handle.join() {
                Ok(Ok(())) => return Err(err),
                Ok(Err(thread_err)) => return Err(thread_err),
                Err(_) => return Err(Error::AgentJoinFailed("thread panicked".into())),
            }
        }

        process.join.lock().unwrap().replace(join_handle);

        if let (Some(bus), Some(spawner)) =
            (self.inner.bus.clone(), self.inner.async_spawner.as_ref())
        {
            let topic = Bus::direct_topic(&id);
            let sender = process.inbox.clone();
            let subscribe_bus = bus.clone();
            let task = spawner.spawn(async move {
                match subscribe_bus.subscribe(&topic).await {
                    Ok(mut subscription) => {
                        while let Some(message) = subscription.next().await {
                            if sender.send(message.body.to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%topic, "failed to subscribe agent mailbox: {err}");
                    }
                }
            });
            process.bus_task.lock().unwrap().replace(task);
        }

        self.inner.agents.insert(id, process);

        Ok(AgentHandle {
            runtime: self.inner.clone(),
            agent_id: id,
        })
    }

    pub fn kill(&self, agent_id: AgentId) -> Result<(), Error> {
        if let Some((_, process)) = self.inner.agents.remove(&agent_id) {
            if let Some(task) = process.bus_task.lock().unwrap().take() {
                task.abort();
            }
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

    pub fn send(&self, agent_id: AgentId, message: Message) -> Result<(), Error> {
        if let (Some(bus), Some(spawner)) =
            (self.inner.bus.clone(), self.inner.async_spawner.as_ref())
        {
            let bus_clone = bus.clone();
            let msg = message.clone();
            return spawner.block_on(async move {
                bus_clone
                    .send(agent_id, msg)
                    .await
                    .map_err(|err| Error::spawn_failed(PathBuf::from("bus"), err.to_string()))
            });
        }

        let entry = self
            .inner
            .agents
            .get(&agent_id)
            .ok_or(Error::UnknownAgent)?;
        entry
            .inbox
            .try_send(message.body.to_vec())
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

    pub fn knowledge_base(&self) -> Arc<KnowledgeBase> {
        self.inner.kb.clone()
    }

    pub fn hostcall_stats(&self) -> Arc<HostcallStats> {
        self.inner.hostcall_stats.clone()
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

fn map_trap_error(err: Trap, wasm_path: &Path) -> Error {
    let message = err.to_string();
    if message.contains("capability denied") {
        Error::CapDenied(message)
    } else {
        Error::spawn_failed(wasm_path.to_path_buf(), message)
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
