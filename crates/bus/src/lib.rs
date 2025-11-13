use bytes::Bytes;
use futures_core::Stream;
use lru::LruCache;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use runloop_core::{
    Error as CoreError,
    content::{CT_ACTION_DECISION, CT_BUS_DROP_NOTICE},
    ids::AgentId,
};
use runloop_rmp::header::DEFAULT_TTL_MS;
use runloop_rmp::{Error as RmpError, Header, encode_payload};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::warn;

mod ipc;

/// Topic reserved for bus drop notifications.
pub const DROP_TOPIC: &str = "rlp/sys/drops";
const DIRECT_TOPIC_PREFIX: &str = "agent/";

static REGISTRY: Lazy<Mutex<HashMap<PathBuf, Arc<Server>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Bus-level errors.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum BusError {
    #[error("bus already bound at {0}")]
    AlreadyBound(PathBuf),
    #[error("bus not found at {0}")]
    NotFound(PathBuf),
    #[error("bus closed")]
    Closed,
    #[error("message expired prior to publish")]
    MessageExpired,
    #[error("invalid ttl {0} ms")]
    InvalidTtl(u64),
    #[error("body length mismatch (header {expected}, body {actual})")]
    BodyLengthMismatch { expected: u32, actual: usize },
    #[error("backpressure timeout for topic {topic}")]
    BackpressureTimeout { topic: String },
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("body type mismatch: expected schema {expected:#06x}, got '{actual}'")]
    BodyTypeMismatch { expected: u16, actual: String },
    #[error("invalid expiry (created_at_ms={created_at_ms}, ttl_ms={ttl_ms})")]
    InvalidExpiry { created_at_ms: u64, ttl_ms: u64 },
    #[error("unknown schema id {0:#06x}")]
    UnknownSchema(u16),
}

impl From<BusError> for CoreError {
    fn from(err: BusError) -> Self {
        CoreError::Bus(err.to_string())
    }
}

/// High-level Runloop message (header + MsgPack envelope bytes).
#[derive(Debug, Clone)]
pub struct Message {
    pub header: Header,
    pub body: Bytes,
}

impl Message {
    /// Construct a message ensuring header/body invariants.
    pub fn new(mut header: Header, body: Bytes) -> Result<Self, BusError> {
        let actual = body.len() as u32;
        if header.body_len != 0 && header.body_len != actual {
            return Err(BusError::BodyLengthMismatch {
                expected: header.body_len,
                actual: body.len(),
            });
        }
        header.body_len = actual;
        header.expires_at_ms().map_err(map_rmp_error)?;
        Ok(Self { header, body })
    }

    fn expires_at(&self) -> Result<u64, BusError> {
        self.header.expires_at_ms().map_err(map_rmp_error)
    }

    fn is_expired(&self, now_ms: u64) -> Result<bool, BusError> {
        self.header.is_expired(now_ms).map_err(map_rmp_error)
    }
}

fn map_rmp_error(err: RmpError) -> BusError {
    match err {
        RmpError::InvalidTtl(ttl) => BusError::InvalidTtl(ttl),
        RmpError::InvalidExpiry {
            created_at_ms,
            ttl_ms,
        } => BusError::InvalidExpiry {
            created_at_ms,
            ttl_ms,
        },
        RmpError::BodyTypeMismatch { expected, actual } => {
            BusError::BodyTypeMismatch { expected, actual }
        }
        RmpError::UnknownSchema(schema_id) => BusError::UnknownSchema(schema_id),
        _ => BusError::Closed,
    }
}

/// Active bus instance exposed to clients.
#[derive(Clone)]
pub struct Bus {
    backend: BusBackend,
    kind: PublisherKind,
}

#[derive(Clone)]
enum BusBackend {
    Local(Arc<Server>),
    Remote(Arc<ipc::IpcClient>),
}

/// Publisher identity kinds for ACL checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublisherKind {
    Ui,
    Tui,
    Agent,
}

impl Bus {
    /// Bind a new in-process bus at `path`.
    #[allow(clippy::unused_async)]
    pub async fn bind<P: AsRef<Path>>(path: P) -> Result<BusServerHandle, BusError> {
        let path = path.as_ref().to_path_buf();
        let mut guard = REGISTRY.lock().expect("registry poisoned");
        if guard.contains_key(&path) {
            return Err(BusError::AlreadyBound(path));
        }
        let server = Arc::new(Server::new());
        guard.insert(path.clone(), server.clone());
        drop(guard);
        let ipc = match ipc::spawn_ipc_server(&path, server.clone()) {
            Ok(opt) => opt,
            Err(err) => {
                warn!(?err, ?path, "failed to start ipc bus listener");
                None
            }
        };
        Ok(BusServerHandle {
            path,
            inner: server,
            ipc,
        })
    }

    /// Connect to the bus bound at `path`.
    pub async fn connect<P: AsRef<Path>>(path: P) -> Result<Self, BusError> {
        Self::connect_with_kind(path, PublisherKind::Agent).await
    }

    async fn connect_with_kind<P: AsRef<Path>>(
        path: P,
        kind: PublisherKind,
    ) -> Result<Self, BusError> {
        let path = path.as_ref().to_path_buf();
        let server = {
            let guard = REGISTRY.lock().expect("registry poisoned");
            guard.get(&path).cloned()
        };
        if let Some(server) = server {
            if server.closed.load(Ordering::SeqCst) {
                return Err(BusError::Closed);
            }
            return Ok(Self {
                backend: BusBackend::Local(server),
                kind,
            });
        }
        if kind != PublisherKind::Agent {
            return Err(BusError::Forbidden(
                "remote publishers limited to agent kind".into(),
            ));
        }
        let client = ipc::connect_ipc_client(&path, PublisherKind::Agent).await?;
        Ok(Self {
            backend: BusBackend::Remote(Arc::new(client)),
            kind,
        })
    }

    /// Connect with an explicit publisher kind (for ACL decisions).
    pub async fn connect_as<P: AsRef<Path>>(
        path: P,
        kind: PublisherKind,
    ) -> Result<Self, BusError> {
        Self::connect_with_kind(path, kind).await
    }

    /// Publish a message to a topic (fan-out to subscribers).
    pub async fn publish(&self, topic: &str, message: Message) -> Result<(), BusError> {
        match &self.backend {
            BusBackend::Local(server) => server.publish(topic, message, true, self.kind).await,
            BusBackend::Remote(client) => client.publish(topic, message).await,
        }
    }

    /// Send a direct message to an agent.
    pub async fn send(&self, dest: AgentId, message: Message) -> Result<(), BusError> {
        let topic = format_direct_topic(&dest);
        self.publish(&topic, message).await
    }

    /// Subscribe to a topic.
    #[allow(clippy::unused_async)]
    pub async fn subscribe(&self, topic: &str) -> Result<Subscription, BusError> {
        match &self.backend {
            BusBackend::Local(server) => server.subscribe(topic).await,
            BusBackend::Remote(client) => {
                let (rx, dropper) = client.subscribe(topic).await?;
                Ok(Subscription {
                    rx,
                    guard: None,
                    on_drop: Some(dropper),
                })
            }
        }
    }

    /// Utility to derive the direct-message topic for `agent_id`.
    pub fn direct_topic(agent_id: &AgentId) -> String {
        format_direct_topic(agent_id)
    }
}

/// Handle to a bound server (for stats + shutdown).
pub struct BusServerHandle {
    path: PathBuf,
    inner: Arc<Server>,
    ipc: Option<ipc::IpcServer>,
}

impl BusServerHandle {
    /// Shut down the server and remove it from the registry.
    pub fn close(&mut self) {
        if !self.inner.shutdown() {
            return;
        }
        let mut guard = REGISTRY.lock().expect("registry poisoned");
        guard.remove(&self.path);
        if let Some(ipc) = self.ipc.take() {
            drop(ipc);
        }
    }

    /// Retrieve a snapshot of bus metrics.
    pub fn stats(&self) -> BusStats {
        self.inner.metrics.snapshot()
    }
}

impl Drop for BusServerHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// Live subscription stream.
pub struct Subscription {
    rx: mpsc::Receiver<Message>,
    guard: Option<SubscriptionGuard>,
    on_drop: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Stream for Subscription {
    type Item = Message;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = unsafe { self.get_unchecked_mut() };
        Pin::new(&mut this.rx).poll_recv(cx)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(guard) = self.guard.take() {
            guard.unregister();
        }
        if let Some(dropper) = self.on_drop.take() {
            dropper();
        }
    }
}

struct SubscriptionGuard {
    server: Server,
    topic: String,
    subscriber_id: u64,
}

impl SubscriptionGuard {
    fn unregister(&self) {
        self.server
            .remove_subscriber(&self.topic, self.subscriber_id);
    }
}

#[derive(Clone)]
struct Server {
    topics: Arc<RwLock<HashMap<String, TopicState>>>,
    metrics: Arc<Metrics>,
    config: BusConfig,
    closed: Arc<AtomicBool>,
}

impl Server {
    fn new() -> Self {
        Self {
            topics: Arc::new(RwLock::new(HashMap::new())),
            metrics: Arc::new(Metrics::default()),
            config: BusConfig::default(),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn shutdown(&self) -> bool {
        if self.closed.swap(true, Ordering::SeqCst) {
            return false;
        }
        let states = {
            let mut topics = self.topics.write();
            std::mem::take(&mut *topics)
        };
        drop(states);
        true
    }

    async fn publish(
        &self,
        topic: &str,
        message: Message,
        emit_notifications: bool,
        publisher: PublisherKind,
    ) -> Result<(), BusError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BusError::Closed);
        }
        // ACL: Only UI/TUI may publish action decisions
        if message.header.schema_id == CT_ACTION_DECISION
            && !self.allowed_action_decision_publisher(publisher)
        {
            return Err(BusError::Forbidden(
                "publisher kind not permitted to publish action.decision".into(),
            ));
        }
        let now_ms = current_millis();
        if message.is_expired(now_ms)? {
            self.metrics.inc_drop(DropReason::TtlExpired);
            if emit_notifications {
                let observed_at = current_millis();
                self.emit_drop(topic, &message, DropReason::TtlExpired, observed_at)
                    .await;
            }
            return Err(BusError::MessageExpired);
        }
        self.metrics.inc_published();
        let subs = { self.topics.read().get(topic).cloned() };
        let mut pending_reservations: Vec<DedupeReservation> = Vec::new();
        let mut primary_error: Option<(FailureSeverity, BusError)> = None;
        if let Some(state) = subs {
            for subscriber in state.subscribers.iter() {
                match self
                    .deliver(subscriber, topic, &message, now_ms, emit_notifications)
                    .await
                {
                    DeliveryResult::Delivered => {}
                    DeliveryResult::ChannelClosed { reservation } => {
                        pending_reservations.push(reservation);
                        let err = BusError::Closed;
                        update_primary_error(
                            &mut primary_error,
                            FailureSeverity::ChannelClosed,
                            err,
                        );
                    }
                    DeliveryResult::BackpressureTimedOut { reservation } => {
                        pending_reservations.push(reservation);
                        let err = BusError::BackpressureTimeout {
                            topic: topic.to_string(),
                        };
                        update_primary_error(
                            &mut primary_error,
                            FailureSeverity::BackpressureTimeout,
                            err,
                        );
                    }
                }
            }
        }
        for mut reservation in pending_reservations {
            reservation.rollback();
        }
        if let Some((_, err)) = primary_error {
            return Err(err);
        }
        Ok(())
    }

    async fn deliver(
        &self,
        subscriber: &Subscriber,
        topic: &str,
        message: &Message,
        now_ms: u64,
        emit_notifications: bool,
    ) -> DeliveryResult {
        let mut reservation = match subscriber.reserve(&message.header, now_ms) {
            Ok(reservation) => reservation,
            Err(DedupeReservationError::Duplicate) => {
                self.metrics.inc_drop(DropReason::Duplicate);
                if emit_notifications {
                    let observed_at = current_millis();
                    self.emit_drop(topic, message, DropReason::Duplicate, observed_at)
                        .await;
                }
                return DeliveryResult::Delivered;
            }
        };

        match self.push_message(subscriber, message).await {
            Ok(()) => {
                reservation.commit();
                DeliveryResult::Delivered
            }
            Err(PushError::ChannelClosed) => {
                self.remove_subscriber(topic, subscriber.id);
                DeliveryResult::ChannelClosed { reservation }
            }
            Err(PushError::TimedOut) => {
                self.metrics.inc_drop(DropReason::BackpressureTimeout);
                if emit_notifications {
                    let observed_at = current_millis();
                    self.emit_drop(topic, message, DropReason::BackpressureTimeout, observed_at)
                        .await;
                }
                DeliveryResult::BackpressureTimedOut { reservation }
            }
        }
    }

    async fn push_message(
        &self,
        subscriber: &Subscriber,
        message: &Message,
    ) -> Result<(), PushError> {
        match subscriber.tx.try_send(message.clone()) {
            Ok(_) => {
                self.metrics.inc_delivered();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                match timeout(
                    self.config.send_timeout,
                    subscriber.tx.send(message.clone()),
                )
                .await
                {
                    Ok(Ok(())) => {
                        self.metrics.inc_delivered();
                        Ok(())
                    }
                    Ok(Err(_)) => Err(PushError::ChannelClosed),
                    Err(_) => Err(PushError::TimedOut),
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(PushError::ChannelClosed),
        }
    }

    #[allow(clippy::unused_async)]
    async fn subscribe(&self, topic: &str) -> Result<Subscription, BusError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BusError::Closed);
        }
        let (tx, rx) = mpsc::channel(self.config.queue_capacity);
        let subscriber = Subscriber::new(
            tx,
            self.config.dedupe_capacity,
            self.config.dedupe_max_age_ms,
        );
        let guard = SubscriptionGuard {
            server: self.clone(),
            topic: topic.to_string(),
            subscriber_id: subscriber.id,
        };
        {
            let mut topics = self.topics.write();
            topics
                .entry(topic.to_string())
                .or_default()
                .subscribers
                .push(subscriber);
        }
        Ok(Subscription {
            rx,
            guard: Some(guard),
            on_drop: None,
        })
    }

    fn remove_subscriber(&self, topic: &str, subscriber_id: u64) {
        let mut topics = self.topics.write();
        if let Some(state) = topics.get_mut(topic) {
            state.subscribers.retain(|sub| sub.id != subscriber_id);
            if state.subscribers.is_empty() {
                topics.remove(topic);
            }
        }
    }

    async fn emit_drop(
        &self,
        topic: &str,
        message: &Message,
        reason: DropReason,
        observed_at_ms: u64,
    ) {
        let expires_at = message.expires_at().ok();
        let notice = DropNotice {
            v: 1,
            reason,
            topic: topic.to_string(),
            trace_id: message.header.trace_id,
            msg_id: message.header.msg_id,
            expires_at_ms: expires_at,
            observed_at_ms,
        };
        let payload = match encode_payload(CT_BUS_DROP_NOTICE, &notice, None) {
            Ok(buf) => buf,
            Err(err) => {
                warn!(?err, "unable to encode drop notice");
                return;
            }
        };
        let drop_msg = Message::new(
            Header {
                schema_id: CT_BUS_DROP_NOTICE,
                created_at_ms: observed_at_ms,
                ttl_ms: 5_000,
                trace_id: message.header.trace_id,
                msg_id: self.metrics.next_drop_msg_id(),
                ..Header::default()
            },
            Bytes::from(payload),
        );
        let drop_msg = match drop_msg {
            Ok(msg) => msg,
            Err(err) => {
                warn!(?err, "drop notice failed ttl validation");
                return;
            }
        };
        let subs = { self.topics.read().get(DROP_TOPIC).cloned() };
        if let Some(state) = subs {
            for subscriber in state.subscribers.iter() {
                match self.push_message(subscriber, &drop_msg).await {
                    Ok(()) => {}
                    Err(PushError::ChannelClosed) => {
                        self.remove_subscriber(DROP_TOPIC, subscriber.id);
                    }
                    Err(PushError::TimedOut) => {
                        warn!(topic = DROP_TOPIC, "drop notice backpressure timeout");
                    }
                }
            }
        }
    }
}

#[derive(Default, Clone)]
struct TopicState {
    subscribers: Vec<Subscriber>,
}

#[derive(Clone)]
struct Subscriber {
    id: u64,
    tx: mpsc::Sender<Message>,
    dedupe: Arc<Mutex<DedupeCache>>,
}

impl Subscriber {
    fn new(tx: mpsc::Sender<Message>, capacity: usize, max_age_ms: u64) -> Self {
        Self {
            id: next_id(),
            tx,
            dedupe: Arc::new(Mutex::new(DedupeCache::new(capacity, max_age_ms))),
        }
    }

    fn reserve(
        &self,
        header: &Header,
        now_ms: u64,
    ) -> Result<DedupeReservation, DedupeReservationError> {
        let key = header.dedupe_key();
        let mut cache = self.dedupe.lock().expect("dedupe poisoned");
        if cache.contains(key, now_ms) {
            return Err(DedupeReservationError::Duplicate);
        }
        cache.insert(key, now_ms);
        Ok(DedupeReservation::new(self.dedupe.clone(), key))
    }
}

#[derive(Clone)]
struct BusConfig {
    queue_capacity: usize,
    send_timeout: Duration,
    dedupe_capacity: usize,
    dedupe_max_age_ms: u64,
    allow_action_decision_ui: bool,
    allow_action_decision_tui: bool,
}

impl Default for BusConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 1_024,
            send_timeout: Duration::from_millis(50),
            dedupe_capacity: 65_536,
            dedupe_max_age_ms: DEFAULT_TTL_MS,
            allow_action_decision_ui: true,
            allow_action_decision_tui: true,
        }
    }
}

impl Server {
    fn allowed_action_decision_publisher(&self, kind: PublisherKind) -> bool {
        match kind {
            PublisherKind::Ui => self.config.allow_action_decision_ui,
            PublisherKind::Tui => self.config.allow_action_decision_tui,
            _ => false,
        }
    }
}

//

enum PushError {
    ChannelClosed,
    TimedOut,
}

#[derive(Default)]
struct Metrics {
    published: AtomicU64,
    delivered: AtomicU64,
    drops_ttl: AtomicU64,
    drops_duplicate: AtomicU64,
    drops_backpressure: AtomicU64,
    drop_msg_seq: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> BusStats {
        BusStats {
            published: self.published.load(Ordering::Relaxed),
            delivered: self.delivered.load(Ordering::Relaxed),
            drops_ttl: self.drops_ttl.load(Ordering::Relaxed),
            drops_duplicate: self.drops_duplicate.load(Ordering::Relaxed),
            drops_backpressure: self.drops_backpressure.load(Ordering::Relaxed),
        }
    }

    fn inc_published(&self) {
        self.published.fetch_add(1, Ordering::Relaxed);
    }

    fn inc_delivered(&self) {
        self.delivered.fetch_add(1, Ordering::Relaxed);
    }

    fn inc_drop(&self, reason: DropReason) {
        match reason {
            DropReason::TtlExpired => {
                self.drops_ttl.fetch_add(1, Ordering::Relaxed);
            }
            DropReason::Duplicate => {
                self.drops_duplicate.fetch_add(1, Ordering::Relaxed);
            }
            DropReason::BackpressureTimeout => {
                self.drops_backpressure.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn next_drop_msg_id(&self) -> u64 {
        self.drop_msg_seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Snapshot view of key bus counters.
#[derive(Debug, Clone, Copy)]
pub struct BusStats {
    pub published: u64,
    pub delivered: u64,
    pub drops_ttl: u64,
    pub drops_duplicate: u64,
    pub drops_backpressure: u64,
}

/// Drop notification payload sent on `DROP_TOPIC`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropNotice {
    pub v: u16,
    pub reason: DropReason,
    pub topic: String,
    pub trace_id: u128,
    pub msg_id: u64,
    pub expires_at_ms: Option<u64>,
    pub observed_at_ms: u64,
}

/// Reasons for a message drop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    TtlExpired,
    Duplicate,
    BackpressureTimeout,
}

struct DedupeCache {
    cache: LruCache<(u128, u64), u64>,
    max_age_ms: u64,
}

impl DedupeCache {
    fn new(capacity: usize, max_age_ms: u64) -> Self {
        let capped = capacity.max(1);
        let capacity = NonZeroUsize::new(capped).expect("non-zero enforced");
        Self {
            cache: LruCache::new(capacity),
            max_age_ms,
        }
    }

    fn contains(&mut self, key: (u128, u64), now_ms: u64) -> bool {
        self.prune(now_ms);
        self.cache.contains(&key)
    }

    fn insert(&mut self, key: (u128, u64), now_ms: u64) {
        self.prune(now_ms);
        self.cache.put(key, now_ms);
    }

    fn remove(&mut self, key: (u128, u64)) {
        self.cache.pop(&key);
    }

    fn prune(&mut self, now_ms: u64) {
        while let Some((_, &ts)) = self.cache.peek_lru() {
            if now_ms.saturating_sub(ts) > self.max_age_ms {
                self.cache.pop_lru();
            } else {
                break;
            }
        }
    }
}

#[derive(Debug)]
enum DedupeReservationError {
    Duplicate,
}

struct DedupeReservation {
    cache: Arc<Mutex<DedupeCache>>,
    key: (u128, u64),
    committed: bool,
}

impl DedupeReservation {
    fn new(cache: Arc<Mutex<DedupeCache>>, key: (u128, u64)) -> Self {
        Self {
            cache,
            key,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }

    fn rollback(&mut self) {
        if self.committed {
            return;
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(self.key);
        }
        self.committed = true;
    }
}

impl Drop for DedupeReservation {
    fn drop(&mut self) {
        if !self.committed {
            if let Ok(mut cache) = self.cache.lock() {
                cache.remove(self.key);
            }
            self.committed = true;
        }
    }
}

enum FailureSeverity {
    ChannelClosed,
    BackpressureTimeout,
}

impl FailureSeverity {
    fn rank(&self) -> u8 {
        match self {
            FailureSeverity::ChannelClosed => 1,
            FailureSeverity::BackpressureTimeout => 2,
        }
    }
}

fn update_primary_error(
    target: &mut Option<(FailureSeverity, BusError)>,
    severity: FailureSeverity,
    error: BusError,
) {
    let replace = target
        .as_ref()
        .map(|(existing, _)| severity.rank() > existing.rank())
        .unwrap_or(true);
    if replace {
        *target = Some((severity, error));
    }
}

enum DeliveryResult {
    Delivered,
    ChannelClosed { reservation: DedupeReservation },
    BackpressureTimedOut { reservation: DedupeReservation },
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn format_direct_topic(agent_id: &AgentId) -> String {
    format!("{DIRECT_TOPIC_PREFIX}{}", agent_id.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::ipc;
    use futures_util::StreamExt;
    use runloop_core::{content::CT_TRACE_LINE, ids::AgentId};
    use runloop_rmp::decode_payload;
    use serde_json::json;
    use std::time::Duration;
    use tokio::{task, time};

    fn test_message(seq: u64) -> Message {
        let mut header = Header::default();
        header.schema_id = CT_TRACE_LINE;
        header.created_at_ms = current_millis();
        header.ttl_ms = 1_000;
        header.trace_id = 1;
        header.msg_id = seq;
        let payload = encode_payload(CT_TRACE_LINE, &json!({"seq": seq}), None).unwrap();
        Message::new(header, Bytes::from(payload)).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn throughput_exceeds_target() {
        let path = PathBuf::from("/tmp/runloop-test-bus-throughput");
        let server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();
        let mut sub = client.subscribe("test/topic").await.unwrap();

        let publish_task = task::spawn({
            let client = client.clone();
            async move {
                for seq in 0..1_200u64 {
                    client
                        .publish("test/topic", test_message(seq))
                        .await
                        .unwrap();
                }
            }
        });

        let start = time::Instant::now();
        let mut received = 0u64;
        while let Some(_msg) = sub.next().await {
            received += 1;
            if received == 1_200 {
                break;
            }
        }
        publish_task.await.unwrap();
        let elapsed = start.elapsed();
        let rate = received as f64 / elapsed.as_secs_f64();
        assert!(rate >= 600.0, "rate {rate} msgs/s below target");
        drop(server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ttl_enforced_and_notified() {
        let path = PathBuf::from("/tmp/runloop-test-bus-ttl");
        let server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();
        let mut drops = client.subscribe(DROP_TOPIC).await.unwrap();

        let mut header = Header::default();
        header.schema_id = CT_TRACE_LINE;
        header.created_at_ms = current_millis() - 2_000;
        header.ttl_ms = 1;
        header.trace_id = 9;
        header.msg_id = 1;
        let payload = encode_payload(CT_TRACE_LINE, &json!({"expired": true}), None).unwrap();
        let msg = Message::new(header, Bytes::from(payload)).unwrap();
        let res = client.publish("ttl/topic", msg).await;
        assert!(matches!(res, Err(BusError::MessageExpired)));
        let drop_notice = drops.next().await.expect("drop notice");
        assert_eq!(drop_notice.header.schema_id, CT_BUS_DROP_NOTICE);
        let payload: DropNotice = decode_payload(CT_BUS_DROP_NOTICE, drop_notice.body.as_ref())
            .unwrap()
            .payload;
        assert_eq!(payload.v, 1);
        assert!(matches!(payload.reason, DropReason::TtlExpired));
        let stats = server.stats();
        assert_eq!(stats.drops_ttl, 1);
        drop(server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_ttl_is_rejected() {
        let path = PathBuf::from("/tmp/runloop-test-bus-ttl-zero");
        let server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();

        let mut header = Header::default();
        header.schema_id = CT_TRACE_LINE;
        header.created_at_ms = current_millis() - 86_400_000; // one day ago
        header.ttl_ms = 0;
        header.trace_id = 42;
        header.msg_id = 123;
        let payload = encode_payload(CT_TRACE_LINE, &json!({"seq": 1}), None).unwrap();
        let message = Message::new(header, Bytes::from(payload)).unwrap_err();
        assert!(matches!(message, BusError::InvalidTtl(0)));

        drop(server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicates_dropped() {
        let path = PathBuf::from("/tmp/runloop-test-bus-dup");
        let server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();
        let mut sub = client.subscribe("dup/topic").await.unwrap();

        let msg = test_message(10);
        client.publish("dup/topic", msg.clone()).await.unwrap();
        client.publish("dup/topic", msg.clone()).await.unwrap();

        assert!(sub.next().await.is_some());
        assert!(
            time::timeout(Duration::from_millis(50), sub.next())
                .await
                .is_err()
        );
        let stats = server.stats();
        assert_eq!(stats.drops_duplicate, 1);
        drop(server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn backpressure_times_out_and_notifies() {
        let path = PathBuf::from("/tmp/runloop-test-bus-backpressure");
        let server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();
        let mut drops = client.subscribe(DROP_TOPIC).await.unwrap();
        let _slow_sub = client.subscribe("bp/topic").await.unwrap();

        let mut timed_out = false;
        for seq in 0..2_048u64 {
            match client.publish("bp/topic", test_message(seq)).await {
                Ok(()) => continue,
                Err(BusError::BackpressureTimeout { .. }) => {
                    timed_out = true;
                    break;
                }
                Err(err) => panic!("unexpected error: {err:?}"),
            }
        }
        assert!(timed_out, "expected a backpressure timeout");
        let drop_message = drops.next().await.expect("drop message");
        assert_eq!(drop_message.header.schema_id, CT_BUS_DROP_NOTICE);
        let notice: DropNotice = decode_payload(CT_BUS_DROP_NOTICE, drop_message.body.as_ref())
            .unwrap()
            .payload;
        assert_eq!(notice.v, 1);
        assert!(matches!(notice.reason, DropReason::BackpressureTimeout));
        let stats = server.stats();
        assert!(stats.drops_backpressure >= 1);
        drop(server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn direct_send_delivers() {
        let path = PathBuf::from("/tmp/runloop-test-bus-send");
        let server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();
        let agent = AgentId::new();
        let topic = Bus::direct_topic(&agent);
        let mut inbox = client.subscribe(&topic).await.unwrap();
        client.send(agent, test_message(99)).await.unwrap();
        let msg = inbox.next().await.expect("direct message");
        assert_eq!(msg.header.msg_id, 99);
        assert_eq!(msg.header.trace_id, 1);
        drop(server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retry_after_backpressure_succeeds() {
        let path = PathBuf::from("/tmp/runloop-test-bus-retry");
        let server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();
        let mut drops = client.subscribe(DROP_TOPIC).await.unwrap();
        let slow_sub = client.subscribe("bp/retry").await.unwrap();

        let mut failed_message = None;
        for seq in 0..5_000u64 {
            let msg = test_message(seq + 10_000);
            match client.publish("bp/retry", msg.clone()).await {
                Ok(()) => continue,
                Err(BusError::BackpressureTimeout { .. }) => {
                    failed_message = Some(msg);
                    break;
                }
                Err(err) => panic!("unexpected error: {err:?}"),
            }
        }

        let retry_msg = failed_message.expect("expected a publish timeout");
        drop(slow_sub);

        task::yield_now().await;

        let mut inbox = client.subscribe("bp/retry").await.unwrap();
        client.publish("bp/retry", retry_msg.clone()).await.unwrap();
        let delivered = inbox.next().await.expect("message delivered");
        assert_eq!(delivered.header.msg_id, retry_msg.header.msg_id);

        let drop_notice = drops.next().await.expect("drop notice");
        assert_eq!(drop_notice.header.schema_id, CT_BUS_DROP_NOTICE);

        drop(server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_shuts_down_subscribers() {
        let path = PathBuf::from("/tmp/runloop-test-bus-close");
        let mut server = Bus::bind(&path).await.unwrap();
        let client = Bus::connect(&path).await.unwrap();
        let mut sub = client.subscribe("close/topic").await.unwrap();

        server.close();

        assert!(sub.next().await.is_none(), "subscriber did not terminate");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn action_decision_acl_rejects_agent_and_allows_ui() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bus-acl");
        let server = Bus::bind(&path).await.unwrap();

        // Agent-kind publisher should be forbidden for action.decision
        let agent = Bus::connect(&path).await.unwrap();
        let mut header = Header::default();
        header.schema_id = CT_ACTION_DECISION;
        header.created_at_ms = current_millis();
        header.ttl_ms = 5_000;
        header.trace_id = 0xabc;
        header.msg_id = 1;
        let msg = Message::new(header, Bytes::from_static(b"{}")).expect("message");
        let res = agent.publish("actions/decision", msg);
        assert!(matches!(res.await, Err(BusError::Forbidden(_))));

        // UI-kind publisher is allowed
        let ui = Bus::connect_as(&path, PublisherKind::Ui).await.unwrap();
        let mut inbox = ui.subscribe("actions/decision").await.unwrap();
        let mut header = Header::default();
        header.schema_id = CT_ACTION_DECISION;
        header.created_at_ms = current_millis();
        header.ttl_ms = 5_000;
        header.trace_id = 0xabc;
        header.msg_id = 2;
        let ok_msg = Message::new(header, Bytes::from_static(b"{}")).expect("ok message");
        ui.publish("actions/decision", ok_msg).await.unwrap();
        let delivered = inbox.next().await.expect("delivered");
        assert_eq!(delivered.header.msg_id, 2);

        drop(server);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ipc_publish_propagates_errors() {
        let path = PathBuf::from("/tmp/runloop-ipc-publish");
        let _ = std::fs::remove_file(&path);
        let server = Bus::bind(&path).await.unwrap();
        for _ in 0..10 {
            if path.exists() {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        if !path.exists() {
            eprintln!(
                "skipping ipc publish test; socket unavailable at {:?}",
                path
            );
            return;
        }
        let client = {
            let mut attempt = 0;
            loop {
                match ipc::connect_ipc_client(&path, PublisherKind::Agent).await {
                    Ok(client) => break client,
                    Err(err) => {
                        attempt += 1;
                        if attempt > 10 {
                            panic!("ipc client: {err:?}");
                        }
                        time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        };

        let mut header = Header::default();
        header.schema_id = CT_ACTION_DECISION;
        header.created_at_ms = current_millis();
        header.trace_id = 1;
        header.msg_id = 7;
        let message = Message::new(header, Bytes::from_static(b"{}")).unwrap();

        let err = client
            .publish("action.decision", message)
            .await
            .expect_err("publish should fail");
        assert!(matches!(err, BusError::Forbidden(_)));

        drop(server);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ipc_subscribe_error_surfaces() {
        let path = PathBuf::from("/tmp/runloop-ipc-subscribe");
        let _ = std::fs::remove_file(&path);
        let mut server = Bus::bind(&path).await.unwrap();
        for _ in 0..10 {
            if path.exists() {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        if !path.exists() {
            eprintln!(
                "skipping ipc subscribe test; socket unavailable at {:?}",
                path
            );
            return;
        }
        let client = {
            let mut attempt = 0;
            loop {
                match ipc::connect_ipc_client(&path, PublisherKind::Agent).await {
                    Ok(client) => break client,
                    Err(err) => {
                        attempt += 1;
                        if attempt > 10 {
                            panic!("ipc client: {err:?}");
                        }
                        time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        };
        server.close();

        match client.subscribe("offline/topic").await {
            Err(err) => assert!(matches!(err, BusError::Closed)),
            Ok(_) => panic!("subscribe should not succeed"),
        }
    }
}
