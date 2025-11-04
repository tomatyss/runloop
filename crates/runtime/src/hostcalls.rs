use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::audit::{AuditCategory, AuditSink};
use crate::caps::{CapabilitySet, Caps, NetLocation};
use crate::error::Error;
use crate::secrets::SecretProvider;
use parking_lot::Mutex;
use runloop_core::ids::{AgentId, TraceId};
use runloop_kb::{AuditDecision, AuditSeverity, CapAuditRecord, KnowledgeBase};
use runloop_model_broker::{Broker, CompletionRequest};
use tokio::sync::mpsc;
use url::Url;
use wasi_common::WasiCtx;
use wasmtime::{Caller, Linker, Trap};

/// Store data embedded inside each Wasmtime store.
pub(crate) struct StoreData {
    pub wasi: WasiCtx,
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

    fn deny(&self, cap: &str, op: &str, target: &str, args: &[u8], reason: &str) -> Trap {
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
        Trap::new(format!("capability denied: {cap} ({reason})"))
    }

    fn ensure_time(&self, op: &str) -> Result<(), Trap> {
        if self.caps.time {
            self.allow();
            Ok(())
        } else {
            Err(self.deny("time.now", op, "", &[], "cap_missing"))
        }
    }

    fn ensure_net(&self, host: &str) -> Result<(), Trap> {
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

    fn ensure_kb_read(&self, namespace: &str) -> Result<(), Trap> {
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

    fn ensure_kb_write(&self, namespace: &str) -> Result<(), Trap> {
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

    fn ensure_model(&self) -> Result<(), Trap> {
        if self.caps.model {
            self.allow();
            Ok(())
        } else {
            Err(self.deny("model.use", "model_complete", "", &[], "cap_missing"))
        }
    }

    fn ensure_secret(&self, secret_id: &str) -> Result<(), Trap> {
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

    fn ensure_exec(&self) -> Result<(), Trap> {
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
    linker.func_wrap("runloop", "time_now", host_time_now)?;
    linker.func_wrap("runloop", "http_request", host_http_request)?;
    linker.func_wrap("runloop", "kb_read", host_kb_read)?;
    linker.func_wrap("runloop", "kb_write", host_kb_write)?;
    linker.func_wrap("runloop", "model_complete", host_model_complete)?;
    linker.func_wrap("runloop", "resolve_secret", host_resolve_secret)?;
    linker.func_wrap("runloop", "exec_spawn", host_exec_spawn)?;
    linker.func_wrap("runloop", "mailbox_recv", host_mailbox_recv)?;
    Ok(())
}

fn host_time_now(mut caller: Caller<'_, StoreData>) -> Result<i64, Trap> {
    caller.data().state.ensure_time("clock_time_get")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Trap::new("system clock before unix epoch"))?;
    Ok(now.as_micros() as i64)
}

fn host_http_request(mut caller: Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<i32, Trap> {
    let url = read_utf8(&mut caller, ptr, len)?;
    caller.data().state.ensure_net(&url)?;
    // MVP: no actual network call; agents will see synthetic 200.
    Ok(200)
}

fn host_kb_read(mut caller: Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<i32, Trap> {
    let namespace = read_utf8(&mut caller, ptr, len)?;
    caller.data().state.ensure_kb_read(&namespace)?;
    Ok(0)
}

fn host_kb_write(mut caller: Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<i32, Trap> {
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
) -> Result<i32, Trap> {
    caller.data().state.ensure_model()?;
    if prompt_len < 0 || model_len < 0 {
        return Err(Trap::new("invalid buffer length"));
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
        return Err(Trap::new("model response too large"));
    }
    let available = prompt_len as usize;
    if rendered.len() > available {
        return Ok(-(rendered.len() as i32));
    }
    write_bytes(&mut caller, prompt_ptr, rendered)?;
    Ok(rendered.len() as i32)
}

fn host_resolve_secret(mut caller: Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<i32, Trap> {
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

fn host_exec_spawn(mut caller: Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<i32, Trap> {
    caller.data().state.ensure_exec()?;
    let command = read_utf8(&mut caller, ptr, len)?;
    // MVP: exec not implemented; simply acknowledge capability check.
    tracing::warn!(%command, "exec hostcall invoked (stub)");
    Ok(0)
}

fn host_mailbox_recv(mut caller: Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<i32, Trap> {
    let Some(message) = caller.data().mailbox.try_recv() else {
        return Ok(0);
    };
    if message.len() > len as usize {
        return Err(Trap::new("mailbox buffer too small"));
    }
    write_bytes(&mut caller, ptr, &message)?;
    Ok(message.len() as i32)
}

fn read_utf8(caller: &mut Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<String, Trap> {
    let bytes = read_bytes(caller, ptr, len)?;
    String::from_utf8(bytes).map_err(|_| Trap::new("invalid utf8 argument"))
}

fn read_bytes(caller: &mut Caller<'_, StoreData>, ptr: i32, len: i32) -> Result<Vec<u8>, Trap> {
    if len <= 0 {
        return Ok(Vec::new());
    }
    let memory = memory(caller)?;
    let mut buf = vec![0u8; len as usize];
    memory
        .read(caller.as_context(), ptr as usize, &mut buf)
        .map_err(|e| Trap::new(format!("memory read failed: {e}")))?;
    Ok(buf)
}

fn write_utf8(caller: &mut Caller<'_, StoreData>, ptr: i32, value: &str) -> Result<(), Trap> {
    write_bytes(caller, ptr, value.as_bytes())
}

fn write_bytes(caller: &mut Caller<'_, StoreData>, ptr: i32, data: &[u8]) -> Result<(), Trap> {
    if data.is_empty() {
        return Ok(());
    }
    let memory = memory(caller)?;
    memory
        .write(caller.as_context_mut(), ptr as usize, data)
        .map_err(|e| Trap::new(format!("memory write failed: {e}")))
}

fn memory(caller: &mut Caller<'_, StoreData>) -> Result<wasmtime::Memory, Trap> {
    caller
        .get_export("memory")
        .and_then(|ext| ext.into_memory())
        .ok_or_else(|| Trap::new("guest module missing exported memory"))
}
