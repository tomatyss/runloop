use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use runloop_core::ids::{AgentId, EventId, OpeningId, TraceId};
use runloop_kb::{KnowledgeBase, Materializer, Provenance, StateDelta};
use runloop_model_broker::{Broker, BrokerError, ModelOutput, ModelRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

pub type AgentResult<T> = Result<T, AgentError>;

/// Shared error type surfaced by canonical agents.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("knowledge base error: {0}")]
    KnowledgeBase(#[from] runloop_kb::Error),
    #[error("model broker error: {0}")]
    Model(#[from] BrokerError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("confirmation provider unavailable")]
    ConfirmationUnavailable,
    #[error("action was declined by the operator")]
    ConfirmationDeclined,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("{0}")]
    Other(String),
}

/// Canonical runtime context handed to each agent invocation.
#[derive(Clone)]
pub struct AgentContext {
    kb: KnowledgeBase,
    materializer: Materializer,
    broker: Option<Arc<Broker>>,
    workdir: PathBuf,
    trace_id: TraceId,
    opening_id: OpeningId,
    agent_id: AgentId,
    confirmation: Option<Arc<dyn ConfirmationProvider>>,
}

impl AgentContext {
    #[must_use]
    pub fn new(
        kb: KnowledgeBase,
        broker: Option<Arc<Broker>>,
        workdir: PathBuf,
        trace_id: TraceId,
        opening_id: OpeningId,
        agent_id: AgentId,
        confirmation: Option<Arc<dyn ConfirmationProvider>>,
    ) -> Self {
        let materializer = Materializer::new(kb.clone());
        Self {
            kb,
            materializer,
            broker,
            workdir,
            trace_id,
            opening_id,
            agent_id,
            confirmation,
        }
    }

    pub fn kb(&self) -> &KnowledgeBase {
        &self.kb
    }

    pub fn broker(&self) -> Option<&Arc<Broker>> {
        self.broker.as_ref()
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    pub fn opening_id(&self) -> OpeningId {
        self.opening_id
    }

    pub fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    pub fn confirmation(&self) -> Option<Arc<dyn ConfirmationProvider>> {
        self.confirmation.as_ref().map(Arc::clone)
    }

    pub fn ensure_views(&self) -> AgentResult<()> {
        loop {
            if !self.materializer.sync()? {
                break;
            }
        }
        Ok(())
    }

    pub fn propose_event(
        &self,
        kind: &str,
        payload: Value,
        rationale: Option<String>,
    ) -> AgentResult<EventId> {
        let provenance = Provenance {
            trace_id: self.trace_id.to_string(),
            opening_id: self.opening_id.to_string(),
            agent_id: self.agent_id.to_string(),
            inputs_hash: None,
            rationale,
        };
        let delta = StateDelta::new(
            kind,
            self.agent_id.to_string(),
            Some("user".to_string()),
            payload,
            provenance,
        );
        let id = self.kb.propose(delta)?;
        self.ensure_views()?;
        Ok(id)
    }

    pub async fn complete_model(&self, request: ModelRequest) -> AgentResult<ModelOutput> {
        match self.broker.as_ref() {
            Some(broker) => broker.complete(&request).await.map_err(AgentError::from),
            None => Err(AgentError::Other(
                "model broker unavailable for this invocation".into(),
            )),
        }
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.workdir.join("artifacts")
    }
}

/// Summary of a resolved contact record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedContact {
    pub contact_id: String,
    pub name: String,
    pub email: String,
    pub org: Option<String>,
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<EventId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSnippet {
    pub title: String,
    pub excerpt: String,
    pub event_id: EventId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextBundle {
    pub topic: String,
    pub snippets: Vec<ContextSnippet>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DraftArtifact {
    pub artifact_id: EventId,
    pub path: PathBuf,
    pub sha256: String,
    pub body_md: String,
    pub rationale: String,
    pub citations: Vec<EventId>,
    pub word_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewNotes {
    pub summary: String,
    #[serde(default)]
    pub suggestions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    pub ok: bool,
    pub notes: ReviewNotes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MailResult {
    pub status: String,
    pub recipients: Vec<String>,
    pub artifact_id: EventId,
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub delivered_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionProposal {
    pub id: String,
    pub trace_id: TraceId,
    pub opening_id: OpeningId,
    pub agent: AgentId,
    pub summary: String,
    pub recipients: Vec<String>,
    pub artifact_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionDecision {
    pub approved: bool,
    #[serde(default)]
    pub rationale: Option<String>,
}

impl ActionDecision {
    #[must_use]
    pub fn approved(rationale: Option<String>) -> Self {
        Self {
            approved: true,
            rationale,
        }
    }

    #[must_use]
    pub fn rejected(rationale: Option<String>) -> Self {
        Self {
            approved: false,
            rationale,
        }
    }
}

#[async_trait]
pub trait ConfirmationProvider: Send + Sync {
    async fn confirm(&self, proposal: ActionProposal) -> AgentResult<ActionDecision>;
}

/// Helper used by agents to build canonical contact payloads.
#[must_use]
pub fn contact_payload(contact: &ResolvedContact, trust: f32) -> Value {
    let mut payload = json!({
        "name": contact.name,
        "email": contact.email,
        "trust": trust,
        "evidence": []
    });
    if let Some(org) = &contact.org
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("org".to_string(), json!(org));
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_payload_omits_org_when_none() {
        let contact = ResolvedContact {
            contact_id: "test".into(),
            name: "Test User".into(),
            email: "test@example.com".into(),
            org: None,
            confidence: 1.0,
            last_event_id: None,
        };
        let payload = contact_payload(&contact, 0.5);
        assert!(payload.get("org").is_none());
        assert_eq!(payload["name"], "Test User");
    }

    #[test]
    fn contact_payload_includes_org_when_some() {
        let contact = ResolvedContact {
            contact_id: "test".into(),
            name: "Test User".into(),
            email: "test@example.com".into(),
            org: Some("Acme Corp".into()),
            confidence: 1.0,
            last_event_id: None,
        };
        let payload = contact_payload(&contact, 0.5);
        assert_eq!(payload["org"], "Acme Corp");
    }
}
