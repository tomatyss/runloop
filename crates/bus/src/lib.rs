//! Local message bus helpers: TTL enforcement, duplicate suppression, and drop metrics.
//!
//! The full Unix domain socket transport will build on these foundations; for now we expose
//! helpers that the future I/O layer can call before admitting frames to subscribers.

use runloop_rmp::header::Header;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const DEFAULT_DEDUPE_CAPACITY: usize = 1_024;

/// Drop metrics tagged by reason.
#[derive(Default)]
struct DropCounters {
    ttl_expired: AtomicU64,
    duplicate: AtomicU64,
}

impl DropCounters {
    fn inc_ttl(&self) {
        self.ttl_expired.fetch_add(1, Ordering::Relaxed);
    }

    fn inc_duplicate(&self) {
        self.duplicate.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> DropStats {
        DropStats {
            ttl_expired: self.ttl_expired.load(Ordering::Relaxed),
            duplicate: self.duplicate.load(Ordering::Relaxed),
        }
    }
}

/// Running counts of drop reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropStats {
    pub ttl_expired: u64,
    pub duplicate: u64,
}

struct DedupeCache {
    inner: Mutex<lru::LruCache<(u128, u64), ()>>,
}

impl DedupeCache {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(lru::LruCache::new(capacity)),
        }
    }

    /// Returns `true` if the (trace_id, msg_id) pair was already seen.
    fn contains_or_insert(&self, trace_id: u128, msg_id: u64) -> bool {
        let mut guard = self.inner.lock().expect("dedupe mutex poisoned");
        if guard.contains(&(trace_id, msg_id)) {
            true
        } else {
            guard.put((trace_id, msg_id), ());
            false
        }
    }
}

/// Reason a frame was dropped before reaching subscribers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    TtlExpired,
    Duplicate,
}

/// Notification describing a dropped frame (emit on `rlp/sys/drops`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropNotice {
    pub reason: DropReason,
    pub trace_id: u128,
    pub msg_id: u64,
    pub opening_id: u64,
    pub created_at_ms: u64,
    pub ttl_ms: u32,
    pub observed_at_ms: u64,
}

impl DropNotice {
    fn ttl_expired(header: &Header, observed_at_ms: u64) -> Self {
        Self {
            reason: DropReason::TtlExpired,
            trace_id: header.trace_id,
            msg_id: header.msg_id,
            opening_id: header.opening_id,
            created_at_ms: header.created_at_ms,
            ttl_ms: header.ttl_ms,
            observed_at_ms,
        }
    }

    fn duplicate(header: &Header, observed_at_ms: u64) -> Self {
        Self {
            reason: DropReason::Duplicate,
            trace_id: header.trace_id,
            msg_id: header.msg_id,
            opening_id: header.opening_id,
            created_at_ms: header.created_at_ms,
            ttl_ms: header.ttl_ms,
            observed_at_ms,
        }
    }
}

#[derive(Clone)]
pub struct Bus {
    drops: Arc<DropCounters>,
    dedupe: Arc<DedupeCache>,
}

impl Bus {
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_DEDUPE_CAPACITY)
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capped = capacity.max(1);
        let capacity = NonZeroUsize::new(capped).expect("NonZeroUsize guaranteed by max(1) guard");
        Self {
            drops: Arc::new(DropCounters::default()),
            dedupe: Arc::new(DedupeCache::new(capacity)),
        }
    }

    /// Returns current drop statistics (relaxed reads).
    #[must_use]
    pub fn drop_stats(&self) -> DropStats {
        self.drops.snapshot()
    }

    /// Validate an incoming header. Returns `Ok(())` if the frame should be delivered, or
    /// a `DropNotice` describing why it was rejected.
    ///
    /// This helper does not perform I/O; higher layers remain responsible for emitting the
    /// notice on `rlp/sys/drops`.
    pub fn check_delivery(&self, header: &Header, now_ms: u64) -> Result<(), DropNotice> {
        if header.ttl_ms != 0 {
            let expires_at = header
                .created_at_ms
                .saturating_add(u64::from(header.ttl_ms));
            if now_ms > expires_at {
                self.drops.inc_ttl();
                return Err(DropNotice::ttl_expired(header, now_ms));
            }
        }
        if self
            .dedupe
            .contains_or_insert(header.trace_id, header.msg_id)
        {
            self.drops.inc_duplicate();
            return Err(DropNotice::duplicate(header, now_ms));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64
    }

    #[test]
    fn allows_fresh_message() {
        let bus = Bus::new();
        let mut header = Header::default();
        header.trace_id = 1;
        header.msg_id = 10;
        let stamp = now_ms();
        header.created_at_ms = stamp;
        header.ttl_ms = 1_000;
        assert!(bus.check_delivery(&header, stamp).is_ok());
        assert_eq!(
            bus.drop_stats(),
            DropStats {
                ttl_expired: 0,
                duplicate: 0
            }
        );
    }

    #[test]
    fn drops_expired_message() {
        let bus = Bus::new();
        let mut header = Header::default();
        header.trace_id = 2;
        header.msg_id = 20;
        header.created_at_ms = 100;
        header.ttl_ms = 100;
        let notice = bus
            .check_delivery(&header, 250)
            .expect_err("should drop expired");
        assert_eq!(notice.reason, DropReason::TtlExpired);
        assert_eq!(bus.drop_stats().ttl_expired, 1);
        assert_eq!(bus.drop_stats().duplicate, 0);
    }

    #[test]
    fn drops_duplicates() {
        let bus = Bus::new();
        let mut header = Header::default();
        header.trace_id = 3;
        header.msg_id = 30;
        let stamp = now_ms();
        header.created_at_ms = stamp;
        header.ttl_ms = 1_000;
        assert!(bus.check_delivery(&header, stamp).is_ok());
        let dup = bus
            .check_delivery(&header, stamp)
            .expect_err("duplicate must be dropped");
        assert_eq!(dup.reason, DropReason::Duplicate);
        let stats = bus.drop_stats();
        assert_eq!(stats.duplicate, 1);
        assert_eq!(stats.ttl_expired, 0);
    }
}
