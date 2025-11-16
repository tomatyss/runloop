mod error;
pub mod schema;

use blake3::hash;
pub use error::AgentRegistryError;
use runloop_core::{AgentDigest, AgentPorts, AgentRef, AgentSchemaBundle, DescribedAgent};
pub use schema::{SchemaValidationError, SchemaViolation, validate_instance};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Registry that discovers agent manifests from configured search directories.
#[derive(Clone, Debug)]
pub struct AgentRegistry {
    search_dirs: Vec<PathBuf>,
}

impl AgentRegistry {
    /// Build a registry from ordered search directories (earlier entries win).
    pub fn new<I, P>(dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let mut seen = BTreeSet::new();
        let mut search_dirs = Vec::new();
        for dir in dirs {
            let path = dir.into();
            if path.as_os_str().is_empty() {
                continue;
            }
            if seen.insert(path.clone()) {
                search_dirs.push(path);
            }
        }
        Self { search_dirs }
    }

    /// Describe a single agent reference.
    pub fn describe(&self, reference: &AgentRef) -> Result<DescribedAgent, AgentRegistryError> {
        let manifest_path = self.find_manifest(reference)?;
        self.load_manifest(reference, &manifest_path)
    }

    /// Describe a set of references, deduplicating by canonical spec while preserving order.
    pub fn describe_many<'a, I>(
        &self,
        references: I,
    ) -> Result<Vec<DescribedAgent>, AgentRegistryError>
    where
        I: IntoIterator<Item = &'a AgentRef>,
    {
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for reference in references {
            if !seen.insert(reference.clone()) {
                continue;
            }
            result.push(self.describe(reference)?);
        }
        Ok(result)
    }

    fn find_manifest(&self, reference: &AgentRef) -> Result<PathBuf, AgentRegistryError> {
        let relative_roots = candidate_roots(reference);
        for base in &self.search_dirs {
            for rel in &relative_roots {
                let candidate = base.join(rel).join("manifest.toml");
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err(AgentRegistryError::NotFound {
            reference: reference.clone(),
        })
    }

    fn load_manifest(
        &self,
        reference: &AgentRef,
        path: &Path,
    ) -> Result<DescribedAgent, AgentRegistryError> {
        let raw = fs::read_to_string(path).map_err(|source| AgentRegistryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let doc: ManifestDoc =
            toml::from_str(&raw).map_err(|source| AgentRegistryError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        let ManifestDoc {
            agent,
            ports,
            schemas,
        } = doc;
        let AgentSection {
            name,
            version,
            variant: agent_variant,
        } = agent;
        if name != reference.name {
            return Err(AgentRegistryError::Mismatch {
                reference: reference.clone(),
                detail: format!("manifest declares agent '{}'", name),
            });
        }
        if let Some(ref_variant) = &reference.variant
            && let Some(man_variant) = &agent_variant
            && man_variant != ref_variant
        {
            return Err(AgentRegistryError::Mismatch {
                reference: reference.clone(),
                detail: format!(
                    "variant mismatch (manifest {man_variant}, requested {ref_variant})"
                ),
            });
        }
        let variant = reference.variant.clone().or_else(|| agent_variant.clone());
        let descriptor_ref = AgentRef::new(name, variant);
        let digest = hash(raw.as_bytes()).to_hex().to_string();
        let schema = schemas
            .map(|section| section.into_bundle())
            .unwrap_or_default();
        let ports = AgentPorts {
            inputs: ports.inputs,
            outputs: ports.outputs,
        };
        Ok(DescribedAgent {
            reference: descriptor_ref,
            version,
            digest,
            schema,
            ports,
        })
    }
}

/// Collect digest assertions for descriptors (used by control-plane submissions).
pub fn digests_from(described: &[DescribedAgent]) -> Vec<AgentDigest> {
    described
        .iter()
        .map(|agent| AgentDigest {
            reference: agent.reference.clone(),
            digest: agent.digest.clone(),
        })
        .collect()
}

/// Extract the set of property keys defined at the top level of a schema.
pub fn schema_property_names(schema: &JsonValue) -> BTreeSet<String> {
    schema
        .get("properties")
        .and_then(|props| props.as_object())
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn candidate_roots(reference: &AgentRef) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(variant) = &reference.variant
        && !variant.is_empty()
    {
        roots.push(PathBuf::from(format!("{}@{}", reference.name, variant)));
        roots.push(PathBuf::from(&reference.name).join(variant));
        roots.push(
            PathBuf::from(&reference.name)
                .join("variants")
                .join(variant),
        );
    }
    roots.push(PathBuf::from(&reference.name));
    roots
}

#[derive(Debug, Deserialize)]
struct ManifestDoc {
    agent: AgentSection,
    #[serde(default)]
    ports: PortsSection,
    #[serde(default)]
    schemas: Option<SchemasSection>,
}

#[derive(Debug, Deserialize)]
struct AgentSection {
    name: String,
    version: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PortsSection {
    #[serde(default, rename = "in")]
    inputs: Vec<String>,
    #[serde(default, rename = "out")]
    outputs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SchemasSection {
    #[serde(default)]
    with: Option<JsonValue>,
}

impl SchemasSection {
    fn into_bundle(self) -> AgentSchemaBundle {
        AgentSchemaBundle { with: self.with }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn describe_loads_manifest_schema() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("agents");
        let registry = AgentRegistry::new([root]);
        let descriptor = registry
            .describe(&AgentRef::new("writer", None))
            .expect("describe writer");
        assert_eq!(descriptor.reference.name, "writer");
        assert!(descriptor.schema.with.is_some());
    }
}
