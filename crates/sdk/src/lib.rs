//! Minimal agent SDK placeholder.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Error)]
pub enum SdkError {
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
}

pub fn validate_manifest(manifest: &AgentManifest) -> Result<(), SdkError> {
    if manifest.name.is_empty() {
        return Err(SdkError::InvalidManifest("name cannot be empty".into()));
    }
    if manifest.version.is_empty() {
        return Err(SdkError::InvalidManifest("version cannot be empty".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_validation() {
        let manifest = AgentManifest {
            name: "writer".into(),
            version: "0.0.1".into(),
            description: None,
        };
        validate_manifest(&manifest).unwrap();
    }
}
