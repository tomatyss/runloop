//! Knowledge base access layer (MVP: in-memory event log with canonical audit events).

use blake3::Hasher;
use parking_lot::Mutex;
use runloop_core::ids::{AgentId, TraceId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Shared knowledge base handle.
#[derive(Clone, Default)]
pub struct KnowledgeBase {
    events: Arc<Mutex<Vec<Event>>>,
}

impl KnowledgeBase {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a capability audit decision.
    pub fn record_cap_audit(&self, record: CapAuditRecord) {
        self.events.lock().push(Event::CapAudit(record));
    }

    /// Snapshot audit events for diagnostics/tests.
    #[must_use]
    pub fn cap_audits(&self) -> Vec<CapAuditRecord> {
        self.events
            .lock()
            .iter()
            .filter_map(|event| match event {
                Event::CapAudit(record) => Some(record.clone()),
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
enum Event {
    CapAudit(CapAuditRecord),
}

/// Decision enum describing whether a capability check allowed or denied an operation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditDecision {
    Allow,
    Deny,
}

/// Severity marker for audit entries.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditSeverity {
    Info,
    Warn,
    Error,
}

/// Canonical capability audit record (stored as JCS JSON in future revisions).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapAuditRecord {
    pub ts_ms: u64,
    pub trace_id: TraceId,
    pub agent_id: AgentId,
    pub cap: String,
    pub op: String,
    pub target: String,
    pub args_hash: [u8; 32],
    pub decision: AuditDecision,
    pub reason: String,
    pub severity: AuditSeverity,
}

impl CapAuditRecord {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        trace_id: TraceId,
        agent_id: AgentId,
        cap: impl Into<String>,
        op: impl Into<String>,
        target: impl Into<String>,
        args: &[u8],
        decision: AuditDecision,
        reason: impl Into<String>,
        severity: AuditSeverity,
    ) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(args);
        let digest = hasher.finalize();
        Self {
            ts_ms: timestamp_ms(),
            trace_id,
            agent_id,
            cap: cap.into(),
            op: op.into(),
            target: target.into(),
            args_hash: digest.into(),
            decision,
            reason: reason.into(),
            severity,
        }
    }
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub use self::{AuditDecision, AuditSeverity, CapAuditRecord};
