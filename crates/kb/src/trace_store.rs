use crate::{KnowledgeBase, Materializer, Provenance, StateDelta};
use runloop_core::config::KbConfig;
use runloop_core::ids::{OpeningId, TraceId};
use runloop_openings::RunTrace;
use serde::Serialize;
use serde_json::json;

/// Owned summary of a node's terminal state used to persist `node.finished`.
#[derive(Clone, Debug, Serialize)]
pub struct NodeFinishedRecord {
    pub node_id: String,
    pub status: String,
    pub attempt: u32,
    pub duration_ms: u64,
    pub outputs_hash: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct TraceStore {
    kb: KnowledgeBase,
    actor: String,
    scope: Option<String>,
}

impl TraceStore {
    /// Open a knowledge base from configuration and wrap it for trace persistence.
    pub fn open(
        config: &KbConfig,
        actor: impl Into<String>,
        scope: Option<String>,
    ) -> Result<Self, crate::Error> {
        let kb = KnowledgeBase::open(config)?;
        kb.migrate()?;
        Ok(Self::from_kb(kb, actor, scope))
    }

    /// Wrap an existing knowledge base handle.
    pub fn from_kb(kb: KnowledgeBase, actor: impl Into<String>, scope: Option<String>) -> Self {
        Self {
            kb,
            actor: actor.into(),
            scope,
        }
    }

    fn provenance(&self, trace_id: &TraceId, opening_id: &OpeningId) -> Provenance {
        Provenance {
            trace_id: trace_id.to_string(),
            opening_id: opening_id.to_string(),
            agent_id: self.actor.clone(),
            inputs_hash: None,
            rationale: None,
        }
    }

    fn record_status(
        &self,
        kind: &str,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        status: &str,
    ) -> Result<(), crate::Error> {
        let payload = json!({
            "opening_id": opening_id.to_string(),
            "status": status,
        });
        let provenance = self.provenance(trace_id, opening_id);
        self.kb.propose(StateDelta::new(
            kind,
            self.actor.as_str(),
            self.scope.clone(),
            payload,
            provenance,
        ))?;
        Ok(())
    }

    pub fn record_run_started(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
    ) -> Result<(), crate::Error> {
        self.record_status("run.started", trace_id, opening_id, "started")
    }

    pub fn record_run_finished(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        status: &str,
    ) -> Result<(), crate::Error> {
        self.record_status("run.finished", trace_id, opening_id, status)
    }

    pub fn record_node_finished(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        record: &NodeFinishedRecord,
    ) -> Result<(), crate::Error> {
        let payload = json!({
            "trace_id": trace_id.to_string(),
            "opening_id": opening_id.to_string(),
            "node_id": record.node_id,
            "status": record.status,
            "attempt": record.attempt,
            "duration_ms": record.duration_ms,
            "outputs_hash": record.outputs_hash,
            "error": record.error,
        });
        let provenance = self.provenance(trace_id, opening_id);
        self.kb.propose(StateDelta::new(
            "node.finished",
            self.actor.as_str(),
            self.scope.clone(),
            payload,
            provenance,
        ))?;
        Ok(())
    }

    pub fn record_nodes(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        records: &[NodeFinishedRecord],
    ) -> Result<(), crate::Error> {
        for record in records {
            self.record_node_finished(trace_id, opening_id, record)?;
        }
        Ok(())
    }

    pub fn record_run_trace(&self, trace: &RunTrace) -> Result<(), crate::Error> {
        let payload =
            serde_json::to_value(trace).map_err(|err| crate::Error::Config(err.to_string()))?;
        let provenance = Provenance {
            trace_id: trace.trace_id.to_string(),
            opening_id: trace.opening_id.to_string(),
            agent_id: self.actor.clone(),
            inputs_hash: None,
            rationale: None,
        };
        self.kb.propose(StateDelta::new(
            "run.trace",
            self.actor.as_str(),
            self.scope.clone(),
            payload,
            provenance,
        ))?;
        Ok(())
    }

    pub fn flush(&self) -> Result<(), crate::Error> {
        let materializer = Materializer::new(self.kb.clone());
        while materializer.sync()? {}
        Ok(())
    }

    pub fn kb(&self) -> &KnowledgeBase {
        &self.kb
    }
}
