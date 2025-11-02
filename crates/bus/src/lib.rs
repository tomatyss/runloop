use std::{
    collections::HashMap,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use hex::encode;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use runloop_rmp::{Frame, Header, registry};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast::{self, Receiver, Sender, error::SendError};
use uuid::Uuid;

static BUS_REGISTRY: Lazy<Mutex<HashMap<PathBuf, Arc<BusInner>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const DEFAULT_LRU_SIZE: usize = 2048;
const DROPS_TOPIC: &str = "rlp/sys/drops";

#[derive(Clone)]
pub struct Bus {
    inner: Arc<BusInner>,
}

impl Bus {
    pub fn bind(path: impl AsRef<Path>) -> Result<Self, BusError> {
        let path = path.as_ref().to_path_buf();
        let mut registry = BUS_REGISTRY.lock();
        let inner = registry
            .entry(path.clone())
            .or_insert_with(|| Arc::new(BusInner::new()))
            .clone();
        Ok(Self { inner })
    }

    pub fn connect(path: impl AsRef<Path>) -> Result<Self, BusError> {
        let path_buf = path.as_ref().to_path_buf();
        let registry = BUS_REGISTRY.lock();
        let inner = registry
            .get(&path_buf)
            .cloned()
            .ok_or_else(|| BusError::NotBound(path_buf.clone()))?;
        Ok(Self { inner })
    }

    pub fn publish(
        &self,
        publisher: PublisherKind,
        topic: impl Into<String>,
        frame: Frame,
    ) -> Result<(), BusError> {
        let topic = topic.into();
        acl::enforce(&publisher, &topic)?;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        if frame.header.is_expired(now_ms) {
            self.inner
                .record_drop(&topic, &frame, DropReason::TtlExpired)?;
            return Ok(());
        }
        if !self.inner.track_dedup(&frame.header) {
            self.inner
                .record_drop(&topic, &frame, DropReason::Duplicate)?;
            return Ok(());
        }

        let sender = self.inner.topic_sender(&topic);
        let message = BusMessage {
            topic: topic.clone(),
            frame,
        };
        match sender.send(message) {
            Ok(_) => Ok(()),
            Err(SendError(_)) => Ok(()),
        }
    }

    pub fn subscribe(&self, topic: impl Into<String>) -> Receiver<BusMessage> {
        let topic = topic.into();
        let sender = self.inner.topic_sender(&topic);
        sender.subscribe()
    }

    pub fn metrics(&self) -> BusMetrics {
        self.inner.metrics()
    }
}

#[derive(Debug, Clone)]
pub struct BusMetrics {
    pub ttl_dropped: u64,
    pub duplicates: u64,
}

#[derive(Debug, Clone)]
pub struct BusMessage {
    pub topic: String,
    pub frame: Frame,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    TtlExpired,
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DropNotice {
    pub topic: String,
    pub reason: DropReason,
    pub trace_id_hex: String,
    pub msg_id_hex: String,
    pub observed_at_ms: u64,
}

struct BusInner {
    topics: Mutex<HashMap<String, Sender<BusMessage>>>,
    dedupe: Mutex<lru::LruCache<[u8; 32], u64>>,
    drop_ttl: AtomicU64,
    drop_duplicate: AtomicU64,
}

impl BusInner {
    fn new() -> Self {
        Self {
            topics: Mutex::new(HashMap::new()),
            dedupe: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(DEFAULT_LRU_SIZE).unwrap(),
            )),
            drop_ttl: AtomicU64::new(0),
            drop_duplicate: AtomicU64::new(0),
        }
    }

    fn topic_sender(&self, topic: &str) -> Sender<BusMessage> {
        let mut guard = self.topics.lock();
        guard
            .entry(topic.to_string())
            .or_insert_with(|| {
                let (sender, _) = broadcast::channel(1024);
                sender
            })
            .clone()
    }

    fn track_dedup(&self, header: &Header) -> bool {
        let key = dedupe_key(header);
        let mut cache = self.dedupe.lock();
        if cache.contains(&key) {
            false
        } else {
            cache.put(key, header.created_at_ms);
            true
        }
    }

    fn record_drop(&self, topic: &str, frame: &Frame, reason: DropReason) -> Result<(), BusError> {
        match reason {
            DropReason::TtlExpired => {
                self.drop_ttl.fetch_add(1, Ordering::Relaxed);
            }
            DropReason::Duplicate => {
                self.drop_duplicate.fetch_add(1, Ordering::Relaxed);
            }
        }
        let notice = DropNotice {
            topic: topic.to_string(),
            reason,
            trace_id_hex: encode(frame.header.trace_id),
            msg_id_hex: encode(frame.header.msg_id),
            observed_at_ms: chrono::Utc::now().timestamp_millis() as u64,
        };
        self.emit_drop_notice(notice)?;
        Ok(())
    }

    fn emit_drop_notice(&self, notice: DropNotice) -> Result<(), BusError> {
        let frame = Frame::with_payload(
            registry::CT_CONTROL_ERROR,
            0,
            Uuid::now_v7().into_bytes(),
            Uuid::now_v7().into_bytes(),
            &notice,
        )
        .map_err(|err| BusError::Encode(err.to_string()))?;
        let sender = self.topic_sender(DROPS_TOPIC);
        let message = BusMessage {
            topic: DROPS_TOPIC.to_string(),
            frame,
        };
        match sender.send(message) {
            Ok(_) => Ok(()),
            Err(SendError(_)) => Ok(()),
        }
    }

    fn metrics(&self) -> BusMetrics {
        BusMetrics {
            ttl_dropped: self.drop_ttl.load(Ordering::Relaxed),
            duplicates: self.drop_duplicate.load(Ordering::Relaxed),
        }
    }
}

fn dedupe_key(header: &Header) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&header.trace_id);
    key[16..].copy_from_slice(&header.msg_id);
    key
}

#[derive(Debug, Clone)]
pub enum PublisherKind {
    Ui,
    Agent,
    Runtime,
    System,
}

mod acl {
    use super::{BusError, PublisherKind};

    const ACTION_DECISION: &str = "action.decision";

    pub fn enforce(publisher: &PublisherKind, topic: &str) -> Result<(), BusError> {
        if topic == ACTION_DECISION && !matches!(publisher, PublisherKind::Ui) {
            return Err(BusError::AclViolation(topic.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BusError {
    #[error("bus path not bound: {0}")]
    NotBound(PathBuf),
    #[error("broadcast error: {0}")]
    Broadcast(String),
    #[error("acl violation when publishing to {0}")]
    AclViolation(String),
    #[error("encode error: {0}")]
    Encode(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_rmp::Frame;
    use serde_json::json;
    use tokio::runtime::Runtime;

    #[test]
    fn ttl_drop_increments_metrics() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let bus = Bus::bind("/tmp/runloop-test.sock").unwrap();
            let trace = uuid::Uuid::now_v7().into_bytes();
            let msg = uuid::Uuid::now_v7().into_bytes();
            let frame = Frame::with_payload(0x0001, 1, trace, msg, &json!({"v":1})).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            bus.publish(PublisherKind::System, "test.topic", frame)
                .unwrap();
            let metrics = bus.metrics();
            assert_eq!(metrics.ttl_dropped, 1);
        });
    }

    #[test]
    fn duplicate_drop_tracked() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let bus = Bus::bind("/tmp/runloop-dupe.sock").unwrap();
            let trace = uuid::Uuid::now_v7().into_bytes();
            let msg = uuid::Uuid::now_v7().into_bytes();
            let frame = Frame::with_payload(0x0001, 1000, trace, msg, &json!({"v":1})).unwrap();
            bus.publish(PublisherKind::System, "test.topic", frame.clone())
                .unwrap();
            bus.publish(PublisherKind::System, "test.topic", frame)
                .unwrap();
            let metrics = bus.metrics();
            assert_eq!(metrics.duplicates, 1);
        });
    }

    #[test]
    fn acl_blocks_action_decision() {
        let bus = Bus::bind("/tmp/runloop-acl.sock").unwrap();
        let trace = uuid::Uuid::now_v7().into_bytes();
        let msg = uuid::Uuid::now_v7().into_bytes();
        let frame = Frame::with_payload(0x0001, 1000, trace, msg, &json!({"v":1})).unwrap();
        let err = bus
            .publish(PublisherKind::Agent, "action.decision", frame)
            .unwrap_err();
        assert!(matches!(err, BusError::AclViolation(_)));
    }
}
