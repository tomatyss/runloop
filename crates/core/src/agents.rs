use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Reference to an agent bundle (name + optional variant).
#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl AgentRef {
    /// Create a new reference.
    pub fn new(name: impl Into<String>, variant: Option<String>) -> Self {
        Self {
            name: name.into(),
            variant,
        }
    }

    /// Whether this reference includes a variant qualifier.
    pub fn has_variant(&self) -> bool {
        self.variant.is_some()
    }

    /// Canonical spec form `name[@variant]`.
    pub fn spec(&self) -> String {
        match &self.variant {
            Some(variant) if !variant.is_empty() => format!("{}@{}", self.name, variant),
            _ => self.name.clone(),
        }
    }
}

impl PartialEq for AgentRef {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.variant == other.variant
    }
}

impl Hash for AgentRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.variant.hash(state);
    }
}

impl Ord for AgentRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.name.cmp(&other.name) {
            std::cmp::Ordering::Equal => self.variant.cmp(&other.variant),
            ordering => ordering,
        }
    }
}

impl PartialOrd for AgentRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for AgentRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.variant {
            Some(variant) if !variant.is_empty() => write!(f, "{}@{}", self.name, variant),
            _ => f.write_str(&self.name),
        }
    }
}

/// Digest assertion for manifests.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDigest {
    pub reference: AgentRef,
    pub digest: String,
}

/// Port declarations exposed via manifests.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPorts {
    #[serde(default, rename = "in", skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
    #[serde(default, rename = "out", skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
}

/// JSON Schema bundle exported by agent manifests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentSchemaBundle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<JsonValue>,
}

/// Optional artifacts declared by agent manifests (digests are required for signing).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<AgentArtifactDigest>,
}

/// Digest assertion for an attachment artifact (e.g., tools.json).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentArtifactDigest {
    pub path: String,
    pub blake3: String,
    pub version: u32,
}

/// Summary provided by the daemon for a resolved agent manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DescribedAgent {
    pub reference: AgentRef,
    pub version: String,
    pub digest: String,
    #[serde(default)]
    pub schema: AgentSchemaBundle,
    #[serde(default)]
    pub ports: AgentPorts,
    #[serde(default)]
    pub artifacts: AgentArtifacts,
}
