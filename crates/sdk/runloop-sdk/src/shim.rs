//! Shim configuration helpers and high-level bus client utilities.
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use runloop_bus::{Bus, Message, PublisherKind, Subscription};
use runloop_core::ids::{AgentId, OpeningId, TraceId};
use runloop_rmp::Header;
use serde::Serialize;
use uuid::Uuid;

use crate::caps::EffectiveCaps;
use crate::error::{Error, Result};
use crate::handshake::{AgentHello, CapabilitySummary};

/// Environment variable providing the runtime socket path.
pub const ENV_SOCKET: &str = "RUNLOOP_SOCKET";
/// Environment variable providing the agent identifier.
pub const ENV_AGENT_ID: &str = "RUNLOOP_AGENT_ID";
/// Environment variable providing JSON-encoded capabilities.
pub const ENV_CAPS_JSON: &str = "RUNLOOP_CAPS_JSON";
/// Optional environment variable overriding the shim version string.
pub const ENV_SHIM_VERSION: &str = "RUNLOOP_SHIM_VERSION";

/// Static shim version string (defaults to the crate version).
pub const SHIM_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Configuration for establishing a shim connection to the bus/runtime.
#[derive(Debug, Clone)]
pub struct ShimConfig {
    agent_id: AgentId,
    bus_path: PathBuf,
    caps: EffectiveCaps,
    shim_version: String,
}

impl ShimConfig {
    /// Construct a config from explicit parts (primarily for tests).
    pub fn new(
        agent_id: AgentId,
        bus_path: PathBuf,
        caps: EffectiveCaps,
        shim_version: impl Into<String>,
    ) -> Self {
        Self {
            agent_id,
            bus_path,
            caps,
            shim_version: shim_version.into(),
        }
    }

    /// Construct a config from the well-known environment variables.
    pub fn from_env() -> Result<Self> {
        let socket = env::var(ENV_SOCKET).map_err(|err| Error::from_var(ENV_SOCKET, err))?;
        let agent_id_raw =
            env::var(ENV_AGENT_ID).map_err(|err| Error::from_var(ENV_AGENT_ID, err))?;
        let caps_raw =
            env::var(ENV_CAPS_JSON).map_err(|err| Error::from_var(ENV_CAPS_JSON, err))?;
        let caps = EffectiveCaps::from_json(&caps_raw)?;
        let shim_version = env::var(ENV_SHIM_VERSION).unwrap_or_else(|_| SHIM_VERSION.to_string());
        let agent_uuid = Uuid::parse_str(agent_id_raw.trim())
            .map_err(|err| Error::invalid(ENV_AGENT_ID, err.to_string()))?;
        Ok(Self::new(
            AgentId(agent_uuid),
            PathBuf::from(socket),
            caps,
            shim_version,
        ))
    }

    /// Return the logical agent identifier.
    pub fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    /// Return effective capabilities granted to this shim.
    pub fn caps(&self) -> &EffectiveCaps {
        &self.caps
    }

    /// Return the bus socket path.
    pub fn bus_path(&self) -> &std::path::Path {
        &self.bus_path
    }

    fn capability_labels(&self) -> Vec<String> {
        let mut labels = Vec::new();
        if !self.caps.fs.is_empty() {
            labels.push("fs".into());
        }
        if !self.caps.net.is_empty() {
            labels.push("net".into());
        }
        if self.caps.time {
            labels.push("time".into());
        }
        if self.caps.model {
            labels.push("model".into());
        }
        if self.caps.exec {
            labels.push("exec".into());
        }
        if self.caps.kb_read.allow_all {
            labels.push("kb.read.*".into());
        } else {
            for domain in &self.caps.kb_read.domains {
                labels.push(format!("kb.read.{domain}"));
            }
        }
        if self.caps.kb_write.allow_all {
            labels.push("kb.write.*".into());
        } else {
            for domain in &self.caps.kb_write.domains {
                labels.push(format!("kb.write.{domain}"));
            }
        }
        labels
    }

    /// Build the canonical agent hello payload for this configuration.
    pub fn hello(&self) -> AgentHello {
        let summary = CapabilitySummary::new(0, self.capability_labels());
        AgentHello {
            agent_id: self.agent_id.to_string(),
            shim_version: self.shim_version.clone(),
            capabilities: summary,
            trace: None,
        }
    }
}

struct ShimInner {
    config: ShimConfig,
    bus: Bus,
    msg_counter: AtomicU64,
}

impl ShimInner {
    fn next_msg_id(&self) -> u64 {
        self.msg_counter.fetch_add(1, Ordering::Relaxed)
    }
}

/// Publish metadata overrides.
#[derive(Debug, Clone, Default)]
pub struct PublishMeta {
    /// Trace identifier to stamp on the header.
    pub trace_id: Option<TraceId>,
    /// Opening identifier associated with the message.
    pub opening_id: Option<OpeningId>,
    /// TTL override (milliseconds). `None` keeps the default.
    pub ttl_ms: Option<u32>,
}

/// Client facade for interacting with the bus/runtime as an agent shim.
#[derive(Clone)]
pub struct ShimClient {
    inner: Arc<ShimInner>,
}

impl ShimClient {
    /// Establish a bus connection using the provided configuration.
    pub async fn connect(config: ShimConfig) -> Result<Self> {
        let bus = Bus::connect_as(config.bus_path(), PublisherKind::Agent).await?;
        Ok(Self {
            inner: Arc::new(ShimInner {
                config,
                bus,
                msg_counter: AtomicU64::new(1),
            }),
        })
    }

    /// Return the logical agent identifier for this shim.
    pub fn agent_id(&self) -> AgentId {
        self.inner.config.agent_id()
    }

    /// Access the effective capabilities associated with this shim.
    pub fn caps(&self) -> &EffectiveCaps {
        self.inner.config.caps()
    }

    /// Generate the canonical hello payload (useful for manual testing).
    pub fn hello(&self) -> AgentHello {
        self.inner.config.hello()
    }

    /// Publish a message with default metadata.
    pub async fn publish<T: Serialize>(
        &self,
        topic: &str,
        schema_id: u16,
        payload: &T,
    ) -> Result<()> {
        self.publish_with(topic, schema_id, payload, PublishMeta::default())
            .await
    }

    /// Publish a message with explicit metadata overrides.
    pub async fn publish_with<T: Serialize>(
        &self,
        topic: &str,
        schema_id: u16,
        payload: &T,
        meta: PublishMeta,
    ) -> Result<()> {
        let body = runloop_rmp::encode_payload(schema_id, payload)?;
        let defaults = Header::default();
        let header = Header {
            schema_id,
            trace_id: meta.trace_id.unwrap_or_default().0.as_u128(),
            opening_id: meta.opening_id.unwrap_or_default().0.as_u128(),
            msg_id: self.inner.next_msg_id(),
            created_at_ms: current_time_ms(),
            ttl_ms: meta.ttl_ms.unwrap_or(defaults.ttl_ms),
            ..defaults
        };
        let message = Message::new(header, Bytes::from(body))?;
        self.inner.bus.publish(topic, message).await?;
        Ok(())
    }

    /// Subscribe to a topic on the bus.
    pub async fn subscribe(&self, topic: &str) -> Result<Subscription> {
        Ok(self.inner.bus.subscribe(topic).await?)
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use runloop_core::content::CT_CONTROL;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn publish_reaches_bus() {
        let dir = tempdir().expect("temp dir");
        let bus_path = dir.path().join("bus.sock");
        let mut server = runloop_bus::Bus::bind(&bus_path).await.expect("bind bus");
        let caps = EffectiveCaps::default();
        let shim = ShimClient::connect(ShimConfig::new(AgentId::new(), bus_path, caps, "test"))
            .await
            .expect("connect shim");
        let mut sub = shim.subscribe("test/topic").await.expect("subscribe");
        shim.publish("test/topic", CT_CONTROL, &json!({"hello": true}))
            .await
            .expect("publish");
        let msg = sub.next().await.expect("message");
        assert_eq!(msg.header.schema_id, CT_CONTROL);
        server.close();
    }
}
