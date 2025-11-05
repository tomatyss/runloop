use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::audit::{AuditCategory, AuditSink};
use crate::caps::{CapabilitySet, Caps, NetLocation};
use crate::error::Error;
use crate::secrets::SecretProvider;
use anyhow::anyhow;
use parking_lot::Mutex;
use runloop_core::ids::{AgentId, TraceId};
use runloop_kb::{AuditDecision, AuditSeverity, CapAuditRecord, KnowledgeBase};
use runloop_model_broker::{Broker, CompletionRequest};
use runloop_rmp::Header;
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
    deny_flag: Arc<AtomicBool>,
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
            deny_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    fn allow(&self) {
        self.hostcall_stats.allowed.fetch_add(1, Ordering::Relaxed);
    }

    fn deny(&self, cap: &str, op: &str, target: &str, args: &[u8], reason: &str) -> WasmtimeError {
        self.record_cap_denial(cap, op, target, args, reason);
        anyhow!("capability denied: {cap} ({reason})")
    }

    pub(crate) fn record_external_cap_denial(
        &self,
        cap: &str,
        op: &str,
        target: &str,
        reason: &str,
    ) {
        self.record_cap_denial(cap, op, target, reason.as_bytes(), reason);
        self.deny_flag.store(false, Ordering::Relaxed);
    }

    pub(crate) fn reset_denial_flag(&self) {
        self.deny_flag.store(false, Ordering::Relaxed);
    }

    pub(crate) fn consume_denial_flag(&self) -> bool {
        self.deny_flag.swap(false, Ordering::Relaxed)
    }

    fn record_cap_denial(&self, cap: &str, op: &str, target: &str, args: &[u8], reason: &str) {
        self.hostcall_stats.denied.fetch_add(1, Ordering::Relaxed);
        self.deny_flag.store(true, Ordering::Relaxed);
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
        if let Ok(url) = Url::parse(host)
            && url.scheme() == "http"
            && !self.caps.net_allow_http
        {
            return Err(self.deny(
                "net.http",
                "http_request",
                host,
                host.as_bytes(),
                "http_not_permitted",
            ));
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

/// Mailbox message envelope (header + body bytes).
#[derive(Debug)]
pub(crate) struct AgentEnvelope {
    pub header: Header,
    pub body: Vec<u8>,
}

impl AgentEnvelope {
    pub fn new(header: Header, body: Vec<u8>) -> Self {
        Self { header, body }
    }
}

struct MailboxInner {
    rx: mpsc::Receiver<AgentEnvelope>,
    peeked: Option<AgentEnvelope>,
}

/// Lightweight mailbox guard shared with the store.
#[derive(Clone)]
pub(crate) struct AgentMailbox {
    inner: Arc<Mutex<MailboxInner>>,
}

impl AgentMailbox {
    pub fn new(rx: mpsc::Receiver<AgentEnvelope>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MailboxInner { rx, peeked: None })),
        }
    }

    pub fn try_recv(&self) -> Option<AgentEnvelope> {
        let mut inner = self.inner.lock();
        if let Some(envelope) = inner.peeked.take() {
            tracing::trace!("mailbox.try_recv returning envelope from peeked");
            return Some(envelope);
        }
        inner.rx.try_recv().ok()
    }

    pub fn peek_meta(&self) -> Option<Header> {
        let mut inner = self.inner.lock();
        if inner.peeked.is_none() {
            inner.peeked = inner.rx.try_recv().ok();
            if inner.peeked.is_some() {
                tracing::trace!("mailbox.peek_meta captured new envelope");
            }
        }
        inner.peeked.as_ref().map(|env| {
            tracing::trace!("mailbox.peek_meta returning header");
            env.header.clone()
        })
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
    if let Some(expected_port) = entry.port
        && port.unwrap_or(expected_port) != expected_port
    {
        return false;
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
        .func_wrap("runloop", "mailbox_peek_meta", host_mailbox_peek_meta)
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
    if message.body.len() > len as usize {
        return Err(host_error("mailbox buffer too small"));
    }
    write_bytes(&mut caller, ptr, &message.body)?;
    Ok(message.body.len() as i32)
}

fn host_mailbox_peek_meta(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    if len <= 0 {
        return Ok(0);
    }
    let Some(header) = caller.data().mailbox.peek_meta() else {
        return Ok(0);
    };
    let trace_hex = format!("{:032x}", header.trace_id);
    let meta = format!(
        "{{\"trace_id\":\"{}\",\"msg_id\":{},\"created_at_ms\":{},\"ttl_ms\":{},\"schema_id\":{}}}",
        trace_hex, header.msg_id, header.created_at_ms, header.ttl_ms, header.schema_id,
    );
    let bytes = meta.into_bytes();
    if bytes.len() > len as usize {
        return Err(host_error("mailbox meta buffer too small"));
    }
    write_bytes(&mut caller, ptr, &bytes)?;
    Ok(bytes.len() as i32)
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
    anyhow!(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditSink;
    use crate::caps::Caps;
    use crate::secrets::SecretStore;
    use bytes::Bytes;
    use runloop_core::ids::AgentId;
    use runloop_kb::KnowledgeBase;
    use runloop_model_broker::Broker;
    use runloop_rmp::Header;
    use serde::Deserialize;
    use tokio::sync::mpsc;
    use wasmtime::{Engine, Instance, Linker, Module, Store};
    use wasmtime_wasi::p1;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Meta<'a> {
        trace_id: &'a str,
        msg_id: u64,
        created_at_ms: u64,
        ttl_ms: u32,
        schema_id: u16,
    }

    fn compile_module(engine: &Engine) -> Module {
        let wat = r#"(module
            (import "runloop" "mailbox_peek_meta" (func $mailbox_peek_meta (param i32 i32) (result i32)))
            (import "runloop" "mailbox_recv" (func $mailbox_recv (param i32 i32) (result i32)))
            (memory (export "memory") 2)
            (func (export "probe") (result i32)
                (local $meta_len i32)
                (local $body_len i32)
                (local.set $meta_len (call $mailbox_peek_meta (i32.const 0) (i32.const 256)))
                (if (i32.eqz (local.get $meta_len)) (then (return (i32.const -1))))
                (local.set $body_len (call $mailbox_recv (i32.const 512) (i32.const 128)))
                (if (i32.eqz (local.get $body_len)) (then (return (i32.const -2))))
                (i32.store8 (i32.add (i32.const 0) (local.get $meta_len)) (i32.const 0))
                (i32.store8 (i32.add (i32.const 512) (local.get $body_len)) (i32.const 0))
                (return (i32.const 0))
            ))"#;
        Module::new(engine, wat).expect("compile module")
    }

    fn snapshot_region(instance: &Instance, store: &mut Store<StoreData>, start: usize) -> Vec<u8> {
        let memory = instance
            .get_export(&mut *store, "memory")
            .and_then(|ext| ext.into_memory())
            .expect("memory export");
        let mut buffer = vec![0u8; 512];
        memory
            .read(store.as_context(), start, &mut buffer)
            .expect("read memory");
        buffer
    }

    #[test]
    fn mailbox_peek_meta_surfaces_header() {
        let engine = Engine::default();
        let module = compile_module(&engine);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let (tx, rx) = mpsc::channel(1);
        let mut header = Header::default();
        header.trace_id = 0xfeedface_u128;
        header.msg_id = 42;
        header.created_at_ms = 1_234;
        header.ttl_ms = 9_000;
        header.schema_id = 77;
        tx.blocking_send(AgentEnvelope::new(
            header.clone(),
            Bytes::from_static(b"ping").to_vec(),
        ))
        .expect("send envelope");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("probe").build_p1();

        let mailbox = AgentMailbox::new(rx);
        let kb = KnowledgeBase::new();
        let kb = std::sync::Arc::new(kb);
        let broker = std::sync::Arc::new(Broker::new());
        let secrets = std::sync::Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let host_state = std::sync::Arc::new(HostState::new(
            Caps::deny_all(),
            audit,
            kb,
            broker,
            secrets,
            std::sync::Arc::new(HostcallStats::new()),
            AgentId::new(),
            "probe".to_string(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: std::sync::Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate");

        let probe = instance
            .get_typed_func::<(), i32>(&mut store, "probe")
            .expect("export probe");

        let result = probe.call(&mut store, ()).expect("probe call");
        assert_eq!(result, 0, "probe should succeed");

        let meta_region = snapshot_region(&instance, &mut store, 0);
        let meta_cstr = meta_region.split(|b| *b == 0).next().unwrap();
        let meta: Meta<'_> = serde_json::from_slice(meta_cstr).expect("meta json");
        assert_eq!(meta.trace_id, "000000000000000000000000feedface");
        assert_eq!(meta.msg_id, header.msg_id);
        assert_eq!(meta.created_at_ms, header.created_at_ms);
        assert_eq!(meta.ttl_ms, header.ttl_ms);
        assert_eq!(meta.schema_id, header.schema_id);

        let body_region = snapshot_region(&instance, &mut store, 512);
        let body_cstr = body_region.split(|b| *b == 0).next().unwrap();
        assert_eq!(body_cstr, b"ping");
    }
}
