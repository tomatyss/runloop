use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Identifier for an agent instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub Uuid);

impl AgentId {
    /// Create a new random agent id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self(Uuid::nil())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent:{}", self.0)
    }
}

/// Identifier for an opening (logical workflow run).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OpeningId(pub Uuid);

impl OpeningId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for OpeningId {
    fn default() -> Self {
        Self(Uuid::nil())
    }
}

impl fmt::Display for OpeningId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "opening:{}", self.0)
    }
}

/// Identifier for a distributed trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId(pub Uuid);

impl TraceId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self(Uuid::nil())
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trace:{}", self.0)
    }
}

impl std::str::FromStr for TraceId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let uuid_str = trimmed.strip_prefix("trace:").unwrap_or(trimmed);
        Uuid::parse_str(uuid_str).map(TraceId)
    }
}

/// Identifier for a persisted knowledge base event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub i64);

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event:{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ids_are_nil() {
        assert_eq!(AgentId::default().0, Uuid::nil());
        assert_eq!(OpeningId::default().0, Uuid::nil());
        assert_eq!(TraceId::default().0, Uuid::nil());
    }

    #[test]
    fn trace_id_parses_with_and_without_prefix() {
        let trace = TraceId::new();
        let raw = trace.to_string();
        let parsed_prefixed: TraceId = raw.parse().expect("prefixed trace parses");
        assert_eq!(parsed_prefixed.0, trace.0);

        let uuid_str = trace.0.to_string();
        let parsed_uuid: TraceId = uuid_str.parse().expect("uuid parses");
        assert_eq!(parsed_uuid.0, trace.0);
    }
}
