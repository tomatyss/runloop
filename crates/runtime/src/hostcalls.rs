use std::cell::Cell;
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as sync_mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::audit::{AuditCategory, AuditSink};
use crate::caps::{CapabilitySet, Caps, NetLocation};
use crate::error::{CapDeniedInfo, CapKind, Error};
use crate::ready::ReadyEmitter;
use crate::runtime::AuditPolicy;
use crate::secrets::SecretProvider;
use anyhow::anyhow;
use blake3::Hash as Blake3Hash;
use parking_lot::Mutex;
use runloop_core::ids::{AgentId, TraceId};
use runloop_kb::{AuditDecision, AuditSeverity, CapAuditRecord, KnowledgeBase};
use runloop_model_broker::{Broker, BrokerError, ModelRequest, ModelResult};
use runloop_rmp::Header;
use tokio::runtime::{Builder as TokioBuilder, Handle as TokioHandle};
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
    async_handle: Option<TokioHandle>,
    ready: ReadyEmitter,
    last_denial: Arc<Mutex<Option<CapDeniedInfo>>>,
    audit_policy: AuditPolicy,
}

fn redact_secret_id(secret_id: &str) -> String {
    let hash: Blake3Hash = blake3::hash(secret_id.as_bytes());
    format!("secret#{}", hash.to_hex())
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
        async_handle: Option<TokioHandle>,
        ready: ReadyEmitter,
        audit_policy: AuditPolicy,
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
            async_handle,
            ready,
            last_denial: Arc::new(Mutex::new(None)),
            audit_policy,
        }
    }

    fn allow(&self, cap: &str, op: &str, target: &str, args: &[u8]) {
        self.hostcall_stats.allowed.fetch_add(1, Ordering::Relaxed);
        self.record_cap_audit(
            AuditDecision::Allow,
            CapAuditDetails {
                cap,
                op,
                target,
                args,
                reason: "granted",
                severity: AuditSeverity::Info,
            },
        );
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
        self.last_denial.lock().take();
    }

    pub(crate) fn consume_denial_flag(&self) -> bool {
        self.deny_flag.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn take_last_denial(&self) -> Option<CapDeniedInfo> {
        self.last_denial.lock().take()
    }

    pub(crate) fn notify_ready_hostcall(&self) {
        self.ready.notify_hostcall();
    }

    pub(crate) fn notify_mailbox_recv(&self) {
        self.ready.notify_mailbox_recv();
    }

    fn broker_complete(&self, request: ModelRequest) -> ModelResult {
        if let Some(handle) = &self.async_handle {
            let (tx, rx) = sync_mpsc::sync_channel(1);
            let broker = Arc::clone(&self.broker);
            handle.spawn({
                async move {
                    let result = broker.complete(&request).await;
                    let _ = tx.send(result);
                }
            });
            return rx.recv().unwrap_or(Err(BrokerError::Cancelled));
        }
        let runtime = TokioBuilder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| BrokerError::ProviderFault {
                code: "runtime_init".into(),
                message: err.to_string(),
            })?;
        runtime.block_on(self.broker.complete(&request))
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
        self.record_cap_audit(
            AuditDecision::Deny,
            CapAuditDetails {
                cap,
                op,
                target,
                args,
                reason,
                severity: AuditSeverity::Warn,
            },
        );
        let info = CapDeniedInfo {
            cap: CapKind::from_label(cap),
            op: op.to_string(),
            detail: target.to_string(),
            reason: reason.to_string(),
            audit_event: None,
        };
        *self.last_denial.lock() = Some(info);
    }

    fn record_cap_audit(&self, decision: AuditDecision, details: CapAuditDetails<'_>) {
        if !self.audit_policy.should_emit(decision) {
            return;
        }
        let record = CapAuditRecord::new(
            self.trace_id,
            self.agent_id,
            details.cap,
            details.op,
            details.target,
            details.args,
            decision,
            details.reason,
            details.severity,
        );
        self.kb.record_cap_audit(record);
    }

    fn ensure_time(&self, op: &str) -> Result<(), WasmtimeError> {
        if self.caps.time {
            self.allow("time.now", op, "", &[]);
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
        self.allow("net.http", "http_request", host, host.as_bytes());
        Ok(())
    }

    fn ensure_kb_read(&self, namespace: &str) -> Result<(), WasmtimeError> {
        if permits_namespace(&self.caps.kb_read, namespace) {
            self.allow("kb.read", "kb_read", namespace, namespace.as_bytes());
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
            self.allow("kb.write", "kb_write", namespace, namespace.as_bytes());
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
            self.allow("model.use", "model_complete", "", &[]);
            Ok(())
        } else {
            Err(self.deny("model.use", "model_complete", "", &[], "cap_missing"))
        }
    }

    fn ensure_secret(&self, secret_id: &str) -> Result<(), WasmtimeError> {
        let redacted = redact_secret_id(secret_id);
        if self.caps.permits_secret(secret_id) {
            self.allow(
                "secrets.get",
                "resolve_secret",
                &redacted,
                redacted.as_bytes(),
            );
            Ok(())
        } else {
            Err(self.deny(
                "secrets.get",
                "resolve_secret",
                &redacted,
                redacted.as_bytes(),
                "secret_not_permitted",
            ))
        }
    }

    fn ensure_exec(&self) -> Result<(), WasmtimeError> {
        if self.caps.exec {
            self.allow("exec.spawn", "exec_spawn", "", &[]);
            Ok(())
        } else {
            Err(self.deny("exec.spawn", "exec_spawn", "", &[], "cap_missing"))
        }
    }
}

struct CapAuditDetails<'a> {
    cap: &'a str,
    op: &'a str,
    target: &'a str,
    args: &'a [u8],
    reason: &'a str,
    severity: AuditSeverity,
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
        .func_wrap("runloop", "exec_spawn_capture", host_exec_spawn_capture)
        .map_err(|err| Error::SpawnFailed(err.to_string()))?;
    linker
        .func_wrap("runloop", "notify_ready", host_notify_ready)
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

const MODEL_ESTREAM: i32 = -1;
const MODEL_EBUDGET: i32 = -2;
const MODEL_ETIMEOUT: i32 = -3;
const MODEL_EPROVIDER: i32 = -4;
const MODEL_EINVAL: i32 = -5;
const MODEL_ENOSPACE: i32 = -6;
const MODEL_ECANCELLED: i32 = -7;

fn host_model_complete(
    mut caller: Caller<'_, StoreData>,
    req_ptr: i32,
    req_len: i32,
    out_ptr: i32,
    out_cap: i32,
    meta_ptr: i32,
    meta_cap: i32,
) -> Result<i32, WasmtimeError> {
    caller.data().state.ensure_model()?;
    if req_len <= 0 || out_cap < 0 || meta_cap < 0 {
        return Err(host_error("invalid buffer length"));
    }

    let request_bytes = read_bytes(&mut caller, req_ptr, req_len)?;
    let request: ModelRequest = match rmp_serde::from_slice(&request_bytes) {
        Ok(req) => req,
        Err(_) => return Ok(MODEL_EINVAL),
    };

    let result = caller.data().state.broker_complete(request);
    let response = match result {
        Ok(output) => output,
        Err(err) => return Ok(map_broker_error(err)),
    };

    let output_bytes = response.text.as_bytes();
    if output_bytes.len() > i32::MAX as usize {
        return Ok(MODEL_ENOSPACE);
    }
    if output_bytes.len() > out_cap as usize {
        return Ok(MODEL_ENOSPACE);
    }
    write_bytes(&mut caller, out_ptr, output_bytes)?;

    if meta_cap > 0 {
        if meta_cap < 4 {
            return Ok(MODEL_ENOSPACE);
        }
        let meta = response.meta();
        let meta_bytes = match rmp_serde::to_vec(&meta) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(MODEL_EPROVIDER),
        };
        if meta_bytes.len() > (meta_cap as usize - 4) {
            return Ok(MODEL_ENOSPACE);
        }
        // Encode meta as: [u32 little-endian length][msgpack payload].
        let len_bytes = (meta_bytes.len() as u32).to_le_bytes();
        write_bytes(&mut caller, meta_ptr, &len_bytes)?;
        let meta_data_ptr = meta_ptr
            .checked_add(4)
            .ok_or_else(|| host_error("meta pointer overflow"))?;
        write_bytes(&mut caller, meta_data_ptr, &meta_bytes)?;
    }

    Ok(output_bytes.len() as i32)
}

fn map_broker_error(err: BrokerError) -> i32 {
    match err {
        BrokerError::StreamingUnsupported => MODEL_ESTREAM,
        BrokerError::BudgetExceeded { .. } => MODEL_EBUDGET,
        BrokerError::Timeout { .. } => MODEL_ETIMEOUT,
        BrokerError::ProviderFault { .. } => MODEL_EPROVIDER,
        BrokerError::InvalidRequest { .. } => MODEL_EINVAL,
        BrokerError::Cancelled => MODEL_ECANCELLED,
        BrokerError::OutputTooLarge { .. } => MODEL_ENOSPACE,
    }
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
        let redacted = redact_secret_id(&secret_id);
        Err(caller.data().state.deny(
            "secrets.get",
            "resolve_secret",
            &redacted,
            redacted.as_bytes(),
            "secret_unknown",
        ))
    }
}

const EXEC_OUTPUT_PREVIEW_LIMIT: usize = 512;
const EXEC_POLL_INTERVAL: Duration = Duration::from_millis(25);
thread_local! {
    static EXEC_TIMEOUT_OVERRIDE_SECS: Cell<u64> = const { Cell::new(0) };
}
#[cfg(test)]
const EXEC_TIMEOUT_OVERRIDE_DISABLE: u64 = u64::MAX;
#[cfg(not(test))]
const EXEC_TIMEOUT_OVERRIDE_DISABLE: u64 = 0;
const EXEC_EINVAL: i32 = -1;
const EXEC_ESPAWN: i32 = -2;
const EXEC_ESIGNAL: i32 = -3;
const EXEC_ENOSPACE: i32 = -4;
const EXEC_CAPTURE_MAX_BUF: usize = 64 * 1024;

fn exec_timeout() -> Option<Duration> {
    EXEC_TIMEOUT_OVERRIDE_SECS.with(|cell| {
        let override_secs = cell.get();
        if cfg!(test) && override_secs == EXEC_TIMEOUT_OVERRIDE_DISABLE {
            return None;
        }
        if override_secs != 0 {
            return Some(Duration::from_secs(override_secs));
        }
        match std::env::var("RUNLOOP_EXEC_TIMEOUT_SECS") {
            Ok(val) => match val.parse::<u64>() {
                Ok(0) => None, // explicit disable
                Ok(secs) => Some(Duration::from_secs(secs)),
                Err(_) => None,
            },
            Err(_) => None,
        }
    })
}

#[cfg(test)]
fn set_exec_timeout_override(secs: u64) {
    EXEC_TIMEOUT_OVERRIDE_SECS.with(|cell| cell.set(secs));
}

#[cfg(test)]
fn clear_exec_timeout_override() {
    EXEC_TIMEOUT_OVERRIDE_SECS.with(|cell| cell.set(0));
}

#[cfg(test)]
fn disable_exec_timeout_override() {
    EXEC_TIMEOUT_OVERRIDE_SECS.with(|cell| cell.set(EXEC_TIMEOUT_OVERRIDE_DISABLE));
}

fn drain_stream_preview(
    mut reader: impl Read + Send + 'static,
    limit: usize,
) -> thread::JoinHandle<(Vec<u8>, io::Result<()>, bool)> {
    thread::spawn(move || {
        let mut preview = Vec::new();
        let mut buf = [0u8; 4096];
        let mut truncated = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let remaining = limit.saturating_sub(preview.len());
                    if remaining > 0 {
                        let take = remaining.min(n);
                        preview.extend_from_slice(&buf[..take]);
                        if n > take {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return (preview, Err(err), truncated),
            }
        }
        (preview, Ok(()), truncated)
    })
}

fn host_exec_spawn(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    caller.data().state.ensure_exec()?;

    if len <= 0 {
        return Ok(EXEC_EINVAL);
    }

    let command = read_utf8(&mut caller, ptr, len)?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%command, %error, "exec spawn failed");
            return Ok(EXEC_ESPAWN);
        }
    };

    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| drain_stream_preview(stdout, EXEC_OUTPUT_PREVIEW_LIMIT));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| drain_stream_preview(stderr, EXEC_OUTPUT_PREVIEW_LIMIT));

    let timeout = exec_timeout();
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if let Some(timeout) = timeout
                    && start.elapsed() > timeout
                {
                    tracing::warn!(%command, ?timeout, "exec timed out; killing process");
                    let _ = child.kill();
                    match child.wait() {
                        Ok(status) => break status,
                        Err(error) => {
                            tracing::warn!(%command, %error, "exec wait after kill failed");
                            return Ok(EXEC_ESPAWN);
                        }
                    }
                } else {
                    thread::sleep(EXEC_POLL_INTERVAL);
                }
            }
            Err(error) => {
                tracing::warn!(%command, %error, "exec wait failed");
                return Ok(EXEC_ESPAWN);
            }
        }
    };

    if let Some(handle) = stdout_handle {
        match handle.join() {
            Ok((preview, Ok(()), truncated)) if !preview.is_empty() => {
                let stdout = String::from_utf8_lossy(&preview);
                tracing::debug!(%command, stdout=%stdout, truncated, "exec stdout");
            }
            Ok((_, Err(error), _)) => {
                tracing::warn!(%command, %error, "failed to read exec stdout");
            }
            Err(_) => tracing::warn!(%command, "exec stdout drain panicked"),
            _ => {}
        }
    }
    if let Some(handle) = stderr_handle {
        match handle.join() {
            Ok((preview, Ok(()), truncated)) if !preview.is_empty() => {
                let stderr = String::from_utf8_lossy(&preview);
                tracing::debug!(%command, stderr=%stderr, truncated, "exec stderr");
            }
            Ok((_, Err(error), _)) => {
                tracing::warn!(%command, %error, "failed to read exec stderr");
            }
            Err(_) => tracing::warn!(%command, "exec stderr drain panicked"),
            _ => {}
        }
    }

    let status = match status.code() {
        Some(code) => code,
        None => {
            tracing::warn!(%command, "exec terminated by signal");
            return Ok(EXEC_ESIGNAL);
        }
    };

    Ok(status)
}

fn host_exec_spawn_capture(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
    stdout_ptr: i32,
    stdout_cap: i32,
    stderr_ptr: i32,
    stderr_cap: i32,
) -> Result<i32, WasmtimeError> {
    caller.data().state.ensure_exec()?;
    if len <= 0 || stdout_cap < 0 || stderr_cap < 0 {
        return Ok(EXEC_EINVAL);
    }

    if (stdout_cap > 0 && stdout_cap < 4) || (stderr_cap > 0 && stderr_cap < 4) {
        return Ok(EXEC_ENOSPACE);
    }

    if stdout_cap as usize > EXEC_CAPTURE_MAX_BUF || stderr_cap as usize > EXEC_CAPTURE_MAX_BUF {
        return Ok(EXEC_ENOSPACE);
    }

    let command = read_utf8(&mut caller, ptr, len)?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%command, %error, "exec capture spawn failed");
            return Ok(EXEC_ESPAWN);
        }
    };

    let stdout_limit = stdout_cap.saturating_sub(4) as usize;
    let stderr_limit = stderr_cap.saturating_sub(4) as usize;
    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| drain_stream_preview(stdout, stdout_limit));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| drain_stream_preview(stderr, stderr_limit));

    let timeout = exec_timeout();
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if let Some(timeout) = timeout
                    && start.elapsed() > timeout
                {
                    tracing::warn!(%command, ?timeout, "exec capture timed out; killing process");
                    let _ = child.kill();
                    match child.wait() {
                        Ok(status) => break status,
                        Err(error) => {
                            tracing::warn!(%command, %error, "exec capture wait after kill failed");
                            return Ok(EXEC_ESPAWN);
                        }
                    }
                } else {
                    thread::sleep(EXEC_POLL_INTERVAL);
                }
            }
            Err(error) => {
                tracing::warn!(%command, %error, "exec capture wait failed");
                return Ok(EXEC_ESPAWN);
            }
        }
    };

    let mut stdout_written = false;
    if let Some(handle) = stdout_handle {
        match handle.join() {
            Ok((preview, Ok(()), truncated)) if stdout_cap > 0 => {
                let data_cap = stdout_cap.saturating_sub(4) as usize;
                if truncated || preview.len() > data_cap {
                    return Ok(EXEC_ENOSPACE);
                }
                write_bytes(
                    &mut caller,
                    stdout_ptr,
                    &(preview.len() as u32).to_le_bytes(),
                )?;
                if !preview.is_empty() {
                    let data_ptr = stdout_ptr
                        .checked_add(4)
                        .ok_or_else(|| host_error("stdout pointer overflow"))?;
                    write_bytes(&mut caller, data_ptr, &preview)?;
                }
                stdout_written = true;
            }
            Ok((_, Err(error), _)) => {
                tracing::warn!(%command, %error, "failed to read exec stdout (capture)");
            }
            Err(_) => tracing::warn!(%command, "exec stdout drain panicked (capture)"),
            _ => {}
        }
    }
    if !stdout_written && stdout_cap >= 4 {
        write_bytes(&mut caller, stdout_ptr, &0u32.to_le_bytes())?;
    }
    let mut stderr_written = false;
    if let Some(handle) = stderr_handle {
        match handle.join() {
            Ok((preview, Ok(()), truncated)) if stderr_cap > 0 => {
                let data_cap = stderr_cap.saturating_sub(4) as usize;
                if truncated || preview.len() > data_cap {
                    return Ok(EXEC_ENOSPACE);
                }
                write_bytes(
                    &mut caller,
                    stderr_ptr,
                    &(preview.len() as u32).to_le_bytes(),
                )?;
                if !preview.is_empty() {
                    let data_ptr = stderr_ptr
                        .checked_add(4)
                        .ok_or_else(|| host_error("stderr pointer overflow"))?;
                    write_bytes(&mut caller, data_ptr, &preview)?;
                }
                stderr_written = true;
            }
            Ok((_, Err(error), _)) => {
                tracing::warn!(%command, %error, "failed to read exec stderr (capture)");
            }
            Err(_) => tracing::warn!(%command, "exec stderr drain panicked (capture)"),
            _ => {}
        }
    }
    if !stderr_written && stderr_cap >= 4 {
        write_bytes(&mut caller, stderr_ptr, &0u32.to_le_bytes())?;
    }

    let status = match status.code() {
        Some(code) => code,
        None => {
            tracing::warn!(%command, "exec (capture) terminated by signal");
            return Ok(EXEC_ESIGNAL);
        }
    };

    Ok(status)
}

fn host_notify_ready(caller: Caller<'_, StoreData>) -> Result<(), WasmtimeError> {
    caller.data().state.notify_ready_hostcall();
    Ok(())
}

fn host_mailbox_recv(
    mut caller: Caller<'_, StoreData>,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmtimeError> {
    caller.data().state.notify_mailbox_recv();
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
    use crate::runtime::AuditPolicy;
    use crate::secrets::SecretStore;
    use bytes::Bytes;
    use rmp_serde::{from_slice as rmp_from_slice, to_vec as rmp_to_vec};
    use runloop_core::ids::{AgentId, TraceId};
    use runloop_kb::KnowledgeBase;
    use runloop_model_broker::{Broker, ModelOutputMeta, ModelParams, ModelRequest};
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
        ttl_ms: u64,
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

    fn compile_model_module(engine: &Engine) -> Module {
        let wat = r#"(module
            (import "runloop" "model_complete" (func $model_complete (param i32 i32 i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 4)
            (func (export "call") (param $req_len i32) (result i32)
                (call $model_complete
                    (i32.const 0)
                    (local.get $req_len)
                    (i32.const 1024)
                    (i32.const 512)
                    (i32.const 2048)
                    (i32.const 512)
                )
            )
        )"#;
        Module::new(engine, wat).expect("compile model module")
    }

    fn compile_exec_module(engine: &Engine) -> Module {
        let wat = r#"(module
            (import "runloop" "exec_spawn" (func $exec_spawn (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "true")
            (func (export "run") (result i32)
                (call $exec_spawn (i32.const 0) (i32.const 4))
            )
        )"#;
        Module::new(engine, wat).expect("compile exec module")
    }

    fn compile_exec_module_with_command(engine: &Engine, command: &str) -> Module {
        let len = command.len();
        let data = command.escape_default().to_string();
        let wat = format!(
            r#"(module
            (import "runloop" "exec_spawn" (func $exec_spawn (param i32 i32) (result i32)))
            (memory (export "memory") 2)
            (data (i32.const 0) "{data}")
            (func (export "run") (result i32)
                (call $exec_spawn (i32.const 0) (i32.const {len}))
            )
        )"#
        );
        Module::new(engine, wat).expect("compile exec module with command")
    }

    fn compile_exec_capture_module(
        engine: &Engine,
        command: &str,
        stdout_cap: usize,
        stderr_cap: usize,
    ) -> Module {
        let len = command.len();
        let data = command.escape_default().to_string();
        let stdout_ptr = 256;
        let stderr_ptr = stdout_ptr + stdout_cap;
        let wat = format!(
            r#"(module
            (import "runloop" "exec_spawn_capture" (func $exec_capture (param i32 i32 i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 3)
            (data (i32.const 0) "{data}")
            (func (export "run") (result i32)
                (call $exec_capture
                    (i32.const 0) (i32.const {len})
                    (i32.const {stdout_ptr}) (i32.const {stdout_cap})
                    (i32.const {stderr_ptr}) (i32.const {stderr_cap})
                )
            )
        )"#
        );
        Module::new(engine, wat).expect("compile exec capture module with command")
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
        let broker = std::sync::Arc::new(Broker::default());
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
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
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

    #[test]
    fn model_complete_writes_output_and_meta() {
        let engine = Engine::default();
        let module = compile_model_module(&engine);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("model").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.model = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "model".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate model module");

        let memory = instance
            .get_export(&mut store, "memory")
            .and_then(|ext| ext.into_memory())
            .expect("memory export");

        let request = ModelRequest {
            trace_id: TraceId::new(),
            model: "local:echo".into(),
            prompt: "hello world".into(),
            role_system: Some("Be crisp.".into()),
            params: Some(ModelParams {
                temperature: Some(0.1),
                max_tokens: Some(32),
                ..ModelParams::default()
            }),
            budget_tokens: Some(256),
            timeout_ms: Some(5000),
            cache_ttl_ms: Some(10_000),
            cache_key: Some("smoke".into()),
            stream: false,
            extras: Some(serde_json::json!({"gemini": {"safety": null}})),
        };

        let request_bytes = rmp_to_vec(&request).expect("encode request");
        memory
            .write(&mut store, 0, &request_bytes)
            .expect("write request");
        memory
            .write(&mut store, 1024, &vec![0u8; 512])
            .expect("zero output");
        memory
            .write(&mut store, 2048, &vec![0u8; 512])
            .expect("zero meta");

        let call = instance
            .get_typed_func::<i32, i32>(&mut store, "call")
            .expect("export call");
        let written = call
            .call(&mut store, request_bytes.len() as i32)
            .expect("model call");
        assert!(written > 0);

        let mut out_buf = vec![0u8; written as usize];
        memory
            .read(&mut store, 1024, &mut out_buf)
            .expect("read output");
        let output = String::from_utf8(out_buf).expect("utf8 output");
        assert_eq!(output, "hello world");

        let mut len_buf = [0u8; 4];
        memory
            .read(&mut store, 2048, &mut len_buf)
            .expect("read meta len");
        let meta_len = u32::from_le_bytes(len_buf) as usize;
        assert!(meta_len > 0);
        let mut meta_buf = vec![0u8; meta_len];
        memory
            .read(&mut store, 2052, &mut meta_buf)
            .expect("read meta payload");
        let meta: ModelOutputMeta = rmp_from_slice(&meta_buf).expect("decode meta");
        assert_eq!(meta.provider, "local");
        assert_eq!(meta.provider_model, "local:echo");
        assert!(!meta.cached);
        assert_eq!(meta.finish_reason.as_deref(), Some("stop"));

        let mut stream_request = request;
        stream_request.stream = true;
        let stream_bytes = rmp_to_vec(&stream_request).expect("encode stream request");
        memory
            .write(&mut store, 0, &stream_bytes)
            .expect("write stream request");
        let code = call
            .call(&mut store, stream_bytes.len() as i32)
            .expect("model call stream");
        assert_eq!(code, MODEL_ESTREAM);
    }

    #[test]
    fn exec_spawn_runs_command_with_exec_cap() {
        let engine = Engine::default();
        let module = compile_exec_module(&engine);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.exec = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let code = run.call(&mut store, ()).expect("exec call should succeed");
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_spawn_capture_returns_output() {
        let engine = Engine::default();
        let module = compile_exec_capture_module(&engine, "printf hi", 32, 32);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.exec = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec-capture".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec capture module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let code = run
            .call(&mut store, ())
            .expect("exec capture call succeeds");
        assert_eq!(code, 0);

        let stdout_snapshot = snapshot_region(&instance, &mut store, 256);
        let stdout_len = u32::from_le_bytes(
            stdout_snapshot
                .get(0..4)
                .unwrap_or_default()
                .try_into()
                .expect("stdout length bytes"),
        ) as usize;
        let stdout = stdout_snapshot
            .get(4..4 + stdout_len)
            .unwrap_or(&[])
            .to_vec();
        assert_eq!(stdout, b"hi");

        let stderr_snapshot =
            snapshot_region(&instance, &mut store, 256 + 32 /* stdout cap */);
        let stderr_len = u32::from_le_bytes(
            stderr_snapshot
                .get(0..4)
                .unwrap_or_default()
                .try_into()
                .expect("stderr length bytes"),
        ) as usize;
        let stderr = stderr_snapshot
            .get(4..4 + stderr_len)
            .unwrap_or(&[])
            .to_vec();
        assert!(
            stderr.is_empty() && stderr_len == 0,
            "capture should not write to stderr buffer for clean command"
        );
    }

    #[test]
    fn exec_spawn_capture_signals_truncation() {
        let engine = Engine::default();
        let module = compile_exec_capture_module(&engine, "printf 12345", 8, 8);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.exec = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec-capture-truncate".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec capture module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let code = run
            .call(&mut store, ())
            .expect("exec capture call succeeds");
        assert_eq!(code, EXEC_ENOSPACE, "truncation should surface as no-space");
    }

    #[test]
    fn exec_spawn_capture_rejects_oversized_buffers() {
        let engine = Engine::default();
        let module = compile_exec_capture_module(&engine, "printf hi", 131072, 16);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.exec = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec-capture-oversize".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec capture module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let code = run
            .call(&mut store, ())
            .expect("exec capture call succeeds");
        assert_eq!(code, EXEC_ENOSPACE, "oversized buffers should be rejected");
    }

    #[test]
    fn exec_spawn_denied_without_cap() {
        let engine = Engine::default();
        let module = compile_exec_module(&engine);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let caps = Caps::deny_all();
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec-deny".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let err = run.call(&mut store, ());
        assert!(err.is_err(), "call should be denied without exec cap");
    }

    #[test]
    fn exec_spawn_drains_large_output_without_buffering() {
        let engine = Engine::default();
        let command =
            "i=0; while [ $i -lt 1048576 ]; do printf a; i=$((i+1)); done; printf done >&2";
        let module = compile_exec_module_with_command(&engine, command);

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.exec = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec-large".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let snapshot = snapshot_region(&instance, &mut store, 0);
        let captured = String::from_utf8(
            snapshot
                .into_iter()
                .take(command.len())
                .collect::<Vec<u8>>(),
        )
        .expect("utf8 command bytes");
        assert_eq!(captured, command);

        let code = run.call(&mut store, ()).expect("exec call should succeed");
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_spawn_times_out_long_running_command() {
        let engine = Engine::default();
        let module = compile_exec_module_with_command(&engine, "sleep 5");

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.exec = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec-timeout".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        set_exec_timeout_override(1);

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let start = Instant::now();
        let code = run.call(&mut store, ()).expect("exec call should complete");
        let elapsed = start.elapsed();
        clear_exec_timeout_override();

        assert_eq!(code, EXEC_ESIGNAL);
        assert!(
            elapsed < Duration::from_secs(6),
            "exec should time out quickly; elapsed={elapsed:?}"
        );
    }

    #[test]
    fn exec_spawn_allows_long_command_without_timeout() {
        let engine = Engine::default();
        let module = compile_exec_module_with_command(&engine, "sleep 2");

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |data: &mut StoreData| &mut data.wasi)
            .expect("link wasi");
        add_to_linker(&mut linker).expect("link hostcalls");

        let wasi = wasmtime_wasi::WasiCtxBuilder::new().arg("exec").build_p1();
        let mailbox = AgentMailbox::new(mpsc::channel(1).1);
        let kb = Arc::new(KnowledgeBase::new());
        let broker = Arc::new(Broker::default());
        let secrets = Arc::new(SecretStore::new());
        let audit = AuditSink::new(16);
        let mut caps = Caps::deny_all();
        caps.exec = true;
        let host_state = Arc::new(HostState::new(
            caps,
            audit,
            kb,
            broker,
            secrets,
            Arc::new(HostcallStats::new()),
            AgentId::new(),
            "exec-no-timeout".to_string(),
            None,
            ReadyEmitter::noop(),
            AuditPolicy::default(),
        ));

        disable_exec_timeout_override();

        let store_data = StoreData {
            wasi,
            state: host_state,
            mailbox: Arc::new(mailbox),
        };

        let mut store = Store::new(&engine, store_data);
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate exec module");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("export run");

        let start = Instant::now();
        let code = run.call(&mut store, ()).expect("exec call should complete");
        clear_exec_timeout_override();

        assert_eq!(code, 0);
        assert!(
            start.elapsed() >= Duration::from_secs(2),
            "command should be allowed to run full duration without timeout"
        );
    }
}
