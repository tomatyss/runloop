mod error;
pub mod schema;

use blake3::hash;
pub use error::AgentRegistryError;
use runloop_core::{
    AgentArtifactDigest, AgentArtifacts, AgentDigest, AgentPorts, AgentRef, AgentSchemaBundle,
    DescribedAgent,
};
pub use schema::{SchemaValidationError, SchemaViolation, validate_instance};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
pub use tools::{Budget, Observability, ToolEntry, ToolsDoc, Transport, load_tools};

mod tools;

#[derive(Clone, Debug)]
pub struct AgentBinary {
    pub path: PathBuf,
    pub blake3: String,
}

impl AgentBinary {
    fn from_entry(base: &Path, entry: &EntrySpec) -> Self {
        Self {
            path: base.join(&entry.path),
            blake3: entry.blake3.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AgentBundle {
    pub described: DescribedAgent,
    pub manifest_dir: PathBuf,
    pub wasm_entry: Option<AgentBinary>,
    pub native_entry: Option<AgentBinary>,
    pub policy_path: Option<PathBuf>,
    pub tools: Option<AgentArtifact>,
}

/// Non-executable artifact bundled with an agent (e.g., tools.json).
#[derive(Clone, Debug)]
pub struct AgentArtifact {
    pub path: PathBuf,
    pub blake3: String,
    pub version: u32,
}

impl AgentArtifact {
    fn from_entry(base: &Path, entry: &ArtifactSpec) -> Self {
        Self {
            path: base.join(&entry.path),
            blake3: entry.blake3.clone(),
            version: entry.version.unwrap_or(1),
        }
    }
}

/// Registry that discovers agent manifests from configured search directories.
#[derive(Clone, Debug)]
pub struct AgentRegistry {
    search_dirs: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ListedAgent {
    pub described: DescribedAgent,
    pub manifest_path: PathBuf,
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
        let (_, described) = self.load_manifest(reference, &manifest_path)?;
        Ok(described)
    }

    /// Load the full bundle metadata (entries + policy path) for a reference.
    pub fn bundle(&self, reference: &AgentRef) -> Result<AgentBundle, AgentRegistryError> {
        let manifest_path = self.find_manifest(reference)?;
        let manifest_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let (doc, described) = self.load_manifest(reference, &manifest_path)?;
        let wasm_entry = doc
            .entry_wasm
            .as_ref()
            .or(doc.agent.entry_wasm.as_ref())
            .map(|entry| AgentBinary::from_entry(&manifest_dir, entry));
        let native_entry = doc
            .entry_native
            .as_ref()
            .or(doc.agent.entry_native.as_ref())
            .map(|entry| AgentBinary::from_entry(&manifest_dir, entry));
        let policy_path = doc.caps.as_ref().map(|caps| manifest_dir.join(&caps.file));
        let tools = doc
            .artifacts
            .as_ref()
            .and_then(|artifacts| artifacts.tools.as_ref())
            .map(|entry| AgentArtifact::from_entry(&manifest_dir, entry));
        Ok(AgentBundle {
            described,
            manifest_dir,
            wasm_entry,
            native_entry,
            policy_path,
            tools,
        })
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

    /// Discover all manifests across configured search directories.
    pub fn list(&self) -> Result<Vec<ListedAgent>, AgentRegistryError> {
        let mut seen = BTreeSet::new();
        let mut listed = Vec::new();
        for base in &self.search_dirs {
            if !base.is_dir() && !base.is_file() {
                continue;
            }
            for manifest_path in discover_manifest_paths(base) {
                let (raw, doc) = read_manifest(&manifest_path)?;
                let reference = AgentRef::new(doc.agent.name.clone(), doc.agent.variant.clone());
                if !seen.insert(reference.clone()) {
                    continue;
                }
                let (_, described) = build_described(&reference, doc, raw)?;
                listed.push(ListedAgent {
                    described,
                    manifest_path: manifest_path.clone(),
                });
            }
        }
        Ok(listed)
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
    ) -> Result<(ManifestDoc, DescribedAgent), AgentRegistryError> {
        let (raw, doc) = read_manifest(path)?;
        build_described(reference, doc, raw)
    }
}

/// Compute the BLAKE3 hex digest for a file (convenience helper shared with tooling).
pub fn digest_file_hex(path: &Path) -> Result<String, AgentRegistryError> {
    let bytes = fs::read(path).map_err(|source| AgentRegistryError::IoPath {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(hash(&bytes).to_hex().to_string())
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

fn read_manifest(path: &Path) -> Result<(String, ManifestDoc), AgentRegistryError> {
    let raw = fs::read_to_string(path).map_err(|source| AgentRegistryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let doc: ManifestDoc = toml::from_str(&raw).map_err(|source| AgentRegistryError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok((raw, doc))
}

fn build_described(
    reference: &AgentRef,
    doc: ManifestDoc,
    raw: String,
) -> Result<(ManifestDoc, DescribedAgent), AgentRegistryError> {
    let agent_section = doc.agent.clone();
    let ports_section = doc.ports.clone();
    let schemas_section = doc.schemas.clone();
    let artifacts_section = doc.artifacts.clone().unwrap_or_default();
    let AgentSection {
        name,
        version,
        variant: agent_variant,
        ..
    } = agent_section;
    if name != reference.name {
        return Err(AgentRegistryError::Mismatch {
            reference: reference.clone(),
            detail: format!("manifest declares agent '{name}'"),
        });
    }
    if let Some(ref_variant) = &reference.variant
        && let Some(man_variant) = &agent_variant
        && man_variant != ref_variant
    {
        return Err(AgentRegistryError::Mismatch {
            reference: reference.clone(),
            detail: format!("variant mismatch (manifest {man_variant}, requested {ref_variant})"),
        });
    }
    let variant = reference.variant.clone().or_else(|| agent_variant.clone());
    let descriptor_ref = AgentRef::new(name, variant);
    let digest = hash(raw.as_bytes()).to_hex().to_string();
    let schema = schemas_section
        .map(|section| section.into_bundle())
        .unwrap_or_default();
    let tools_digest = artifacts_section
        .tools
        .as_ref()
        .map(|spec| {
            let version = spec.version.unwrap_or(1);
            if version != 1 {
                return Err(AgentRegistryError::Artifact {
                    reference: reference.clone(),
                    detail: format!("unsupported tools.json version {version} (supported: 1)"),
                });
            }
            Ok(AgentArtifactDigest {
                path: spec.path.clone(),
                blake3: spec.blake3.clone(),
                version,
            })
        })
        .transpose()?;
    let artifacts = AgentArtifacts {
        tools: tools_digest,
    };
    let ports = AgentPorts {
        inputs: ports_section.inputs,
        outputs: ports_section.outputs,
    };
    let described = DescribedAgent {
        reference: descriptor_ref,
        version,
        digest,
        schema,
        ports,
        artifacts,
    };
    Ok((doc, described))
}

fn discover_manifest_paths(base: &Path) -> Vec<PathBuf> {
    if base.is_file() {
        if base.file_name().is_some_and(|name| name == "manifest.toml") {
            return vec![base.to_path_buf()];
        }
        return Vec::new();
    }
    if !base.is_dir() {
        return Vec::new();
    }

    let mut manifests = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back((base.to_path_buf(), 0usize));

    while let Some((dir, depth)) = queue.pop_front() {
        let manifest = dir.join("manifest.toml");
        if manifest.is_file() {
            manifests.push(manifest);
            continue;
        }
        if depth >= 3 {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut dirs = entries
            .flatten()
            .filter_map(|entry| {
                let Ok(ft) = entry.file_type() else {
                    return None;
                };
                if ft.is_dir() {
                    Some(entry.path())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        dirs.sort();
        for entry in dirs {
            queue.push_back((entry, depth + 1));
        }
    }

    manifests
}

#[derive(Clone, Debug, Deserialize)]
struct ManifestDoc {
    agent: AgentSection,
    #[serde(default)]
    ports: PortsSection,
    #[serde(default)]
    schemas: Option<SchemasSection>,
    #[serde(default)]
    entry_native: Option<EntrySpec>,
    #[serde(default)]
    entry_wasm: Option<EntrySpec>,
    #[serde(default)]
    caps: Option<CapsSection>,
    #[serde(default)]
    artifacts: Option<ArtifactsSection>,
}

#[derive(Clone, Debug, Deserialize)]
struct AgentSection {
    name: String,
    version: String,
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    entry_native: Option<EntrySpec>,
    #[serde(default)]
    entry_wasm: Option<EntrySpec>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct PortsSection {
    #[serde(default, rename = "in")]
    inputs: Vec<String>,
    #[serde(default, rename = "out")]
    outputs: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct SchemasSection {
    #[serde(default)]
    with: Option<JsonValue>,
}

impl SchemasSection {
    fn into_bundle(self) -> AgentSchemaBundle {
        AgentSchemaBundle { with: self.with }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct EntrySpec {
    path: String,
    blake3: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CapsSection {
    file: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ArtifactsSection {
    #[serde(default)]
    tools: Option<ArtifactSpec>,
}

#[derive(Clone, Debug, Deserialize)]
struct ArtifactSpec {
    path: String,
    blake3: String,
    #[serde(default)]
    version: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::AgentRef;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tempfile::{NamedTempFile, tempdir};

    fn write_manifest(root: &Path, manifest: &str) -> PathBuf {
        let agent_dir = root.join("writer");
        std::fs::create_dir_all(&agent_dir).expect("create agent dir");
        let manifest_path = agent_dir.join("manifest.toml");
        std::fs::write(&manifest_path, manifest).expect("write manifest");
        manifest_path
    }

    fn basic_manifest() -> &'static str {
        r#"[agent]
name = "writer"
version = "1.0.0"

[ports]
in = []
out = []
"#
    }

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

    #[test]
    fn describe_many_deduplicates_references() {
        let tmp = tempdir().expect("tmp dir");
        write_manifest(tmp.path(), basic_manifest());
        let registry = AgentRegistry::new([tmp.path().to_path_buf()]);
        let reference = AgentRef::new("writer", None);
        let refs = vec![reference.clone(), reference];
        let described = registry.describe_many(refs.iter()).expect("describe");
        assert_eq!(
            described.len(),
            1,
            "duplicate references should be collapsed"
        );
    }

    #[test]
    fn describe_rejects_variant_mismatch() {
        let tmp = tempdir().expect("tmp dir");
        write_manifest(
            tmp.path(),
            r#"[agent]
name = "writer"
version = "1.0.0"
variant = "pro"

[ports]
in = []
out = []
"#,
        );
        let registry = AgentRegistry::new([tmp.path().to_path_buf()]);
        let err = registry
            .describe(&AgentRef::new("writer", Some("basic".into())))
            .expect_err("variant mismatch should fail");
        let msg = format!("{err}");
        assert!(
            msg.contains("variant mismatch"),
            "expected mismatch detail, got {msg}"
        );
    }

    #[test]
    fn list_discovers_manifests() {
        let tmp = tempdir().expect("tmp dir");
        let manifest = write_manifest(tmp.path(), basic_manifest());
        let registry = AgentRegistry::new([tmp.path()]);
        let listed = registry.list().expect("list agents");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].described.reference.name, "writer");
        assert_eq!(listed[0].described.version, "1.0.0");
        assert_eq!(listed[0].manifest_path, manifest);
    }

    #[test]
    fn list_prefers_first_search_dir() {
        let first = tempdir().expect("first dir");
        let second = tempdir().expect("second dir");
        write_manifest(
            first.path(),
            r#"[agent]
name = "writer"
version = "1.0.0"

[ports]
in = []
out = []
"#,
        );
        write_manifest(
            second.path(),
            r#"[agent]
name = "writer"
version = "2.0.0"

[ports]
in = []
out = []
"#,
        );
        let registry =
            AgentRegistry::new([first.path().to_path_buf(), second.path().to_path_buf()]);
        let listed = registry.list().expect("list agents");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].described.version, "1.0.0");
    }

    #[test]
    fn list_discovers_variant_layout() {
        let tmp = tempdir().expect("variant dir");
        let variant_dir = tmp.path().join("writer").join("variants").join("pro");
        std::fs::create_dir_all(&variant_dir).expect("variant dir");
        let manifest_path = variant_dir.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            r#"[agent]
name = "writer"
version = "1.1.0"
variant = "pro"

[ports]
in = []
out = []
"#,
        )
        .expect("write manifest");

        let registry = AgentRegistry::new([tmp.path()]);
        let listed = registry.list().expect("list agents");
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].described.reference.variant.as_deref(),
            Some("pro")
        );
        assert_eq!(listed[0].manifest_path, manifest_path);
    }

    #[test]
    fn bundle_exposes_tools_attachment() {
        let tmp = tempdir().expect("tmp dir");
        write_manifest(
            tmp.path(),
            r#"[agent]
name = "writer"
version = "1.0.0"

[ports]
in = []
out = []

[artifacts.tools]
path = "tools.json"
blake3 = "abcdef"
version = 1
"#,
        );
        let registry = AgentRegistry::new([tmp.path().to_path_buf()]);
        let bundle = registry
            .bundle(&AgentRef::new("writer", None))
            .expect("bundle with tools");
        let tools = bundle.tools.expect("tools attachment present");
        assert!(tools.path.ends_with("tools.json"));
        assert_eq!(tools.blake3, "abcdef");
        assert_eq!(tools.version, 1);
        assert_eq!(
            bundle
                .described
                .artifacts
                .tools
                .as_ref()
                .expect("digest")
                .blake3,
            "abcdef"
        );
    }

    #[test]
    fn unsupported_tools_version_rejected() {
        let tmp = tempdir().expect("tmp dir");
        write_manifest(
            tmp.path(),
            r#"[agent]
name = "writer"
version = "1.0.0"

[arts]
noop = true

[ports]
in = []
out = []

[artifacts.tools]
path = "tools.json"
blake3 = "abcdef"
version = 99
"#,
        );
        let registry = AgentRegistry::new([tmp.path().to_path_buf()]);
        let err = registry
            .describe(&AgentRef::new("writer", None))
            .expect_err("unsupported tools version");
        let msg = format!("{err}");
        assert!(
            msg.contains("unsupported tools.json version"),
            "expected tools version error, got {msg}"
        );
    }

    #[test]
    fn schema_property_names_extracts_keys() {
        let schema = serde_json::json!({
            "properties": {
                "alpha": {},
                "beta": {}
            }
        });
        let props = schema_property_names(&schema);
        assert!(props.contains("alpha"));
        assert!(props.contains("beta"));
        assert_eq!(props.len(), 2);
    }

    #[test]
    fn digest_file_hex_computes_hash() {
        let mut tmp = NamedTempFile::new().expect("tmp file");
        tmp.write_all(b"abc123").expect("write content");
        let digest = digest_file_hex(tmp.path()).expect("digest");
        assert_eq!(digest.len(), 64);
    }
}
