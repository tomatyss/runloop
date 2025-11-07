use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::error::CapDeniedInfo;

/// Reason reported when the readiness barrier fails.
#[derive(Debug, Clone)]
pub enum ReadyFailure {
    Timeout,
    Host {
        info: Option<CapDeniedInfo>,
        message: String,
    },
    ChannelClosed,
}

/// Guest acknowledgement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyAckKind {
    NotifyHostcall,
    MailboxFirstRecv,
}

impl ReadyAckKind {
    pub fn as_label(&self) -> &'static str {
        match self {
            ReadyAckKind::NotifyHostcall => "notify_hostcall",
            ReadyAckKind::MailboxFirstRecv => "mailbox_first_recv",
        }
    }
}

enum ReadyEvent {
    Ready(ReadyAckKind),
    Failed(ReadyFailure),
}

struct ReadyInner {
    host_ready: AtomicBool,
    guest_ready: AtomicBool,
    ack: AtomicU8,
    completed: AtomicBool,
    tx: Mutex<Option<Sender<ReadyEvent>>>,
}

impl ReadyInner {
    fn new(tx: Sender<ReadyEvent>) -> Self {
        Self {
            host_ready: AtomicBool::new(false),
            guest_ready: AtomicBool::new(false),
            ack: AtomicU8::new(0),
            completed: AtomicBool::new(false),
            tx: Mutex::new(Some(tx)),
        }
    }

    fn signal_host_ready(&self) {
        self.host_ready.store(true, Ordering::SeqCst);
        self.try_complete();
    }

    fn signal_guest_ready(&self, ack: ReadyAckKind) {
        if !self.guest_ready.swap(true, Ordering::SeqCst) {
            let code = match ack {
                ReadyAckKind::NotifyHostcall => 1u8,
                ReadyAckKind::MailboxFirstRecv => 2u8,
            };
            self.ack.store(code, Ordering::SeqCst);
            self.try_complete();
        }
    }

    fn try_complete(&self) {
        if self.completed.load(Ordering::SeqCst) {
            return;
        }
        if self.host_ready.load(Ordering::SeqCst) && self.guest_ready.load(Ordering::SeqCst) {
            let ack = match self.ack.load(Ordering::SeqCst) {
                1 => ReadyAckKind::NotifyHostcall,
                2 => ReadyAckKind::MailboxFirstRecv,
                _ => ReadyAckKind::MailboxFirstRecv,
            };
            self.finish(ReadyEvent::Ready(ack));
        }
    }

    fn fail(&self, failure: ReadyFailure) {
        self.finish(ReadyEvent::Failed(failure));
    }

    fn finish(&self, event: ReadyEvent) {
        if self.completed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut guard) = self.tx.lock()
            && let Some(tx) = guard.take()
        {
            let _ = tx.send(event);
        }
    }
}

/// Host-side handle for reporting readiness progress or failure.
#[derive(Clone)]
pub struct ReadyHandle {
    inner: Arc<ReadyInner>,
}

impl ReadyHandle {
    pub fn signal_host_ready(&self) {
        self.inner.signal_host_ready();
    }

    pub fn fail(&self, failure: ReadyFailure) {
        self.inner.fail(failure);
    }
}

/// Guest-side emitter wired into hostcalls.
#[derive(Clone, Default)]
pub struct ReadyEmitter {
    inner: Option<Arc<ReadyInner>>,
}

impl ReadyEmitter {
    fn from_inner(inner: Arc<ReadyInner>) -> Self {
        Self { inner: Some(inner) }
    }

    #[cfg(test)]
    pub(crate) fn noop() -> Self {
        Self { inner: None }
    }

    pub fn notify_hostcall(&self) {
        if let Some(inner) = &self.inner {
            inner.signal_guest_ready(ReadyAckKind::NotifyHostcall);
        }
    }

    pub fn notify_mailbox_recv(&self) {
        if let Some(inner) = &self.inner {
            inner.signal_guest_ready(ReadyAckKind::MailboxFirstRecv);
        }
    }
}

/// Waiter used by the runtime thread to await readiness.
pub struct ReadyWaiter {
    rx: Receiver<ReadyEvent>,
}

impl ReadyWaiter {
    pub fn wait_until(&self, deadline: Instant) -> Result<ReadyAckKind, ReadyFailure> {
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remaining) {
                Ok(ReadyEvent::Ready(ack)) => return Ok(ack),
                Ok(ReadyEvent::Failed(failure)) => return Err(failure),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(ReadyFailure::ChannelClosed),
            }
        }
        Err(ReadyFailure::Timeout)
    }
}

/// Create a new readiness barrier, returning the host handle, guest emitter, and waiter.
pub fn ready_barrier() -> (ReadyHandle, ReadyEmitter, ReadyWaiter) {
    let (tx, rx) = mpsc::channel();
    let inner = Arc::new(ReadyInner::new(tx));
    (
        ReadyHandle {
            inner: inner.clone(),
        },
        ReadyEmitter::from_inner(inner),
        ReadyWaiter { rx },
    )
}
