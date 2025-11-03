use crate::ids::{AgentId, OpeningId, TraceId};
use tracing::{Level, Span};

/// Shared trace metadata propagated across bus hops and agent spans.
#[derive(Clone, Debug)]
pub struct TraceContext {
    pub trace_id: TraceId,
    pub opening_id: OpeningId,
    pub agent_id: AgentId,
}

impl TraceContext {
    /// Spawn a child span that preserves the trace identifiers.
    #[must_use]
    pub fn child_span(&self, name: &str) -> Span {
        tracing::span!(
            Level::INFO,
            "runloop.child",
            trace_id = %self.trace_id,
            opening_id = %self.opening_id,
            agent_id = %self.agent_id,
            span_name = name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    #[test]
    fn child_span_carries_fields() {
        let context = TraceContext {
            trace_id: TraceId::new(),
            opening_id: OpeningId::new(),
            agent_id: AgentId::new(),
        };
        let subscriber = tracing_subscriber::registry();
        let _guard = tracing::subscriber::set_default(subscriber);
        let span = context.child_span("test_span");
        let metadata = span.metadata().expect("span metadata");
        assert_eq!(metadata.name(), "runloop.child");
        assert_eq!(metadata.level(), &Level::INFO);
    }
}
