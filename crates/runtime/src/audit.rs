#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tracing::warn;

/// Broad classification of audit events emitted by the runtime.
#[derive(Debug, Clone, Copy)]
pub enum AuditCategory {
    CapabilityDenied,
    HostError,
}

/// Immutable audit record.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub ts_ms: u64,
    pub category: AuditCategory,
    pub message: String,
}

impl AuditEvent {
    pub fn new(category: AuditCategory, message: impl Into<String>) -> Self {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        Self {
            ts_ms,
            category,
            message: message.into(),
        }
    }
}

/// In-memory ring buffer of audit events (until KB integration is ready).
#[derive(Clone)]
pub struct AuditSink {
    capacity: usize,
    events: Arc<Mutex<VecDeque<AuditEvent>>>,
}

impl AuditSink {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            events: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
        }
    }

    pub fn record(&self, category: AuditCategory, message: impl Into<String>) {
        let event = AuditEvent::new(category, message);
        warn!(?event, "runtime audit event");
        let mut guard = self.events.lock();
        if guard.len() == self.capacity {
            guard.pop_front();
        }
        guard.push_back(event);
    }

    pub fn snapshot(&self) -> Vec<AuditEvent> {
        self.events.lock().iter().cloned().collect()
    }
}

impl Default for AuditSink {
    fn default() -> Self {
        Self::new(512)
    }
}
