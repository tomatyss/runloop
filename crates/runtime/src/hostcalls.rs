use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::audit::{AuditCategory, AuditSink};
use crate::caps::{CapabilitySet, Caps, NetLocation};
use crate::error::Error;
use crate::secrets::SecretProvider;
use anyhow::anyhow;
use parking_lot::Mutex;
use runloop_core::ids::{AgentId, TraceId};
use runloop_kb::{AuditDecision, AuditSeverity, CapAuditRecord, KnowledgeBase};
use runloop_model_broker::{Broker, CompletionRequest};
use tokio::sync::mpsc;
use url::Url;
use wasmtime::{AsContext, AsContextMut, Caller, Error as WasmtimeError, Linker, Memory};
use wasmtime_wasi::p1::WasiP1Ctx;

/// Store data embedded inside each Wasmtime store.
pub(crate) struct StoreData {
    pub wasi: WasiP1Ctx,
    pub state: Arc<HostState>,
    pub mailbox: Arc<AgentMailbox>,
}

/// Shared state exposed to hostcalls.
#[derive(Clone)]
pub(crate) struct HostState {
    caps: Caps,
    audit: AuditSink,
    kb: Arc<KnowledgeBase>,
    broker: Arc<Broker>,
    secrets: Arc<dyn SecretProvider>,
    hostcall_stats: Arc<HostcallStats>,
    agent_id: AgentId,
    trace_id: TraceId,
    identity: String,
}

impl HostState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        caps: Caps,
        audit: AuditSink,
        kb: Arc<KnowledgeBase>,
        broker: Arc<Broker>,
        secrets: Arc<dyn SecretProvider>,
        hostcall_stats: Arc<HostcallStats>,
        agent_id: AgentId,
        identity: String,
    ) -> Self {
        Self {
            caps,
            audit,
            kb,
            broker,
            secrets,
            hostcall_stats,
            agent_id,
            trace_id: TraceId::new(),
            identity,
        }
    }

    fn allow(&self) {
        self.hostcall_stats.allowed.fetch_add(1, Ordering::Relaxed);
    }

    fn deny(&self, cap: &str, op: &str, target: &str, args: &[u8], reason: &str) -> WasmtimeError {
        self.hostcall_stats.denied.fetch_add(1, Ordering::Relaxed);
        self.audit.record(
            AuditCategory::CapabilityDenied,
            format!(
                "capability denied: agent={} cap={} op={} target={} reason={}",
                self.identity, cap, op, target, reason
            ),
        );
        let record = CapAuditRecord::new(
            self.trace_id,
            self.agent_id,
            cap,
            op,
            target,
            args,
            AuditDecision::Deny,
            reason,
            AuditSeverity::Warn,
        );
        self.kb.record_cap_audit(record);
        anyhow!("capability denied: {cap} ({reason})").into()
    }

    fn ensure_time(&self, op: &str) -> Result<(), WasmtimeError> {
        if self.caps.time {
            self.allow();
            Ok(())
        } else {
            Err(self.deny("time.now", op, "", &[], "cap_missing"))
        }
    }

    fn ensure_net(&self, host: &str) -> Result<(), WasmtimeError> {
        if self.caps.net_hosts.is_empty() {
            return Err(self.deny(
                "net.http",
                "http_request",
                host,
                host.as_bytes(),
                "no_hosts",
            ));
        }
        if let Ok(url) = Url::parse(host) {
            if url.scheme() == "http" && !self.caps.net_allow_http {
                return Err(self.deny(
                    "net.http",
                    "http_request",
                    host,
                    host.as_bytes(),
                    "http_not_permitted",
                ));
            }
        }
        if !self
            .caps
            .net_hosts
            .iter()
            .any(|allowed| host_allows(allowed, host))
        {
            return Err(self.deny(
                "net.http",
                "http_request",
                host,
                host.as_bytes(),
                "host_not_permitted",
            ));
        }
        self.allow();
        Ok(())
    }

    fn ensure_kb_read(&self, namespace: &str) -> Result<(), WasmtimeError> {
        if permits_namespace(&self.caps.kb_read, namespace) {
            self.allow();
            Ok(())
        } else {
            Err(self.deny(
                "kb.read",
                "kb_read",
                namespace,
                namespace.as_bytes(),
                "namespace_not_permitted",
            ))
        }
    }

    fn ensure_kb_write(&self, namespace: &str) -> Result<(), WasmtimeError> {
        if permits_namespace(&self.caps.kb_write, namespace) {
            self.allow();
            Ok(())
        } else {
            Err(self.deny(
                "kb.write",
                "kb_write",
                namespace,
                namespace.as_bytes(),
                "namespace_not_permitted",
            ))
        }
    }

    fn ensure_model(&self) -> Result<(), WasmtimeError> {
        if self.caps.model {
            self.allow();
            Ok(())
        } else {
            Err(self.deny("model.use", "model_complete", "", &[], "cap_missing"))
        }
    }

    fn ensure_secret(&self, secret_id: &str) -> Result<(), WasmtimeError> {
        if self.caps.permits_secret(secret_id) {
            self.allow();
            Ok(())
        } else {
            Err(self.deny(
                "secrets.get",
                "resolve_secret",
                secret_id,
                secret_id.as_bytes(),
                "secret_not_permitted",
            ))
        }
    }

    fn ensure_exec(&self) -> Result<(), WasmtimeError> {
        if self.caps.exec {
            self.allow();
            Ok(())
        } else {
            Err(self.deny("exec.spawn", "exec_spawn", "", &[], "cap_missing"))
        }
    }
}

/// Lightweight mailbox guard shared with the store.
#[derive(Clone)]
pub(crate) struct AgentMailbox {
    inner: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
}

impl AgentMailbox {
    pub fn new(rx: mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(rx)),
        }
    }

    pub fn try_recv(&self) -> Option<Vec<u8>> {
        self.inner.lock().try_recv().ok()
    }

    pub fn blocking_recv(&self) -> Option<Vec<u8>> {
        self.inner.lock().blocking_recv()
    }
}

/// Hostcall statistics (allowed vs denied decisions).
#[derive(Default)]
pub struct HostcallStats {
    allowed: AtomicU64,
    denied: AtomicU64,
}

impl HostcallStats {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn allowed(&self) -> u64 {
        self.allowed.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn denied(&self) -> u64 {
        self.denied.load(Ordering::Relaxed)
    }
}

fn host_allows(entry: &NetLocation, host: &str) -> bool {
    if let Ok(url) = Url::parse(host) {
        let host_str = url.host_str().unwrap_or(host);
        let port = url.port();
        return host_matches(entry, host_str, port);
    }
    host_matches(entry, host, None)
}

fn host_matches(entry: &NetLocation, host: &str, port: Option<u16>) -> bool {
    if let Some(expected_port) = entry.port {
        if port.unwrap_or(expected_port) != expected_port {
            return false;
        }
    }
    entry.host == host
}

fn permits_namespace(set: &CapabilitySet, namespace: &str) -> bool {
    match set {
        CapabilitySet::All => true,
        CapabilitySet::None => false,
        CapabilitySet::Domains(domains) => domains.contains(namespace),
    }
}

pub(crate) fn add_to_linker(linker: &mut Linker<StoreData>) -> Result<(), Error> {
    linker
        .func_wrap("runloop", "time_now", host_time_now)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "http_request", host_http_request)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "kb_read", host_kb_read)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "kb_write", host_kb_write)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "model_complete", host_model_complete)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "resolve_secret", host_resolve_secret)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "exec_spawn", host_exec_spawn)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "mailbox_recv", host_mailbox_recv)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    Ok(())
}

fn host_time_now(caller: Caller<'_, StoreData>) -> Result<i64, WasmtimeError> {
    caller.data().state.ensure_time("clock_time_get")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| host_error("system clock before unix epoch"))?;
    Ok(now.as_micros() as i64)
}

fn host_http_request(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    let url = read_utf8(&mut caller, ptr, len)?;
    caller.data().state.ensure_net(&url)?;
    // MVP: no actual network call; agents will see synthetic 200.
    Ok(200)
}

fn host_kb_read(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    let namespace = read_utf8(&mut caller, ptr, len)?;
    caller.data().state.ensure_kb_read(&namespace)?;
    Ok(0)
}

fn host_kb_write(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    let namespace = read_utf8(&mut caller, ptr, len)?;
    caller.data().state.ensure_kb_write(&namespace)?;
    Ok(0)
}

fn host_model_complete(
    mut caller: Caller<'_, StoreData>,
    prompt_ptr: i32,
    prompt_len: i32,
    model_ptr: i32,
    model_len: i32,
) -> Result<i32, WasmtimeError> {
    caller.data().state.ensure_model()?;
    if prompt_len < 0 || model_len < 0 {
        return Err(host_error("invalid buffer length"));
    }
    let prompt = read_utf8(&mut caller, prompt_ptr, prompt_len)?;
    let model = if model_len > 0 {
        Some(read_utf8(&mut caller, model_ptr, model_len)?)
    } else {
        None
    };
    let response = caller
        .data()
        .state
        .broker
        .complete(&CompletionRequest { prompt, model });
    let rendered = response.output.as_bytes();
    if rendered.len() > i32::MAX as usize {
        return Err(host_error("model response too large"));
    }
    let available = prompt_len as usize;
    if rendered.len() > available {
        return Ok(-(rendered.len() as i32));
    }
    write_bytes(&mut caller, prompt_ptr, rendered)?;
    Ok(rendered.len() as i32)
}

fn host_resolve_secret(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    let secret_id = read_utf8(&mut caller, ptr, len)?;
    caller.data().state.ensure_secret(&secret_id)?;
    if let Some(value) = caller.data().state.secrets.resolve(&secret_id) {
        write_utf8(&mut caller, ptr, &value)?;
        Ok(value.len() as i32)
    } else {
        Err(caller.data().state.deny(
            "secrets.get",
            "resolve_secret",
            &secret_id,
            secret_id.as_bytes(),
            "secret_unknown",
        ))
    }
}

fn host_exec_spawn(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    caller.data().state.ensure_exec()?;
    let command = read_utf8(&mut caller, ptr, len)?;
    // MVP: exec not implemented; simply acknowledge capability check.
    tracing::warn!(%command, "exec hostcall invoked (stub)");
    Ok(0)
}

fn host_mailbox_recv(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    let Some(message) = caller.data().mailbox.try_recv() else {
        return Ok(0);
    };
    if message.len() > len as usize {
        return Err(host_error("mailbox buffer too small"));
    }
    write_bytes(&mut caller, ptr, &message)?;
    Ok(message.len() as i32)
}

fn read_utf8(
    caller: &mut Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<String, WasmtimeError> {
    let bytes = read_bytes(caller, ptr, len)?;
    String::from_utf8(bytes).map_err(|_| host_error("invalid utf8 argument"))
}

fn read_bytes(
    caller: &mut Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<Vec<u8>, WasmtimeError> {
    if len <= 0 {
        return Ok(Vec::new());
    }
    let memory = memory(caller)?;
    let mut buf = vec![0u8; len as usize];
    memory
        .read(caller.as_context(), ptr as usize, &mut buf)
        .map_err(|e| host_error(format!("memory read failed: {e}")))?;
    Ok(buf)
}

fn write_utf8(
    caller: &mut Caller<'_, StoreData>,
    ptr: i32,
    value: &str,
) -> Result<(), WasmtimeError> {
    write_bytes(caller, ptr, value.as_bytes())
}

fn write_bytes(
    caller: &mut Caller<'_, StoreData>,
    ptr: i32,
    data: &[u8],
) -> Result<(), WasmtimeError> {
    if data.is_empty() {
        return Ok(());
    }
    let memory = memory(caller)?;
    memory
        .write(caller.as_context_mut(), ptr as usize, data)
        .map_err(|e| host_error(format!("memory write failed: {e}")))
}

fn memory(caller: &mut Caller<'_, StoreData>) -> Result<Memory, WasmtimeError> {
    caller
        .get_export("memory")
        .and_then(|ext| ext.into_memory())
        .ok_or_else(|| host_error("guest module missing exported memory"))
}

fn host_error(msg: impl Into<String>) -> WasmtimeError {
    anyhow!(msg.into()).into()
}
