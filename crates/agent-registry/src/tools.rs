use crate::AgentRegistryError;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Top-level tools.json document (versioned).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolsDoc {
    pub version: u32,
    #[serde(default)]
    pub tools: Vec<ToolEntry>,
}

impl ToolsDoc {
    /// Validate structural invariants for version 1 documents.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "unsupported tools.json version {} (expected 1)",
                self.version
            ));
        }
        for (i, tool) in self.tools.iter().enumerate() {
            tool.validate()
                .map_err(|detail| format!("tool #{i}: {detail}"))?;
        }
        Ok(())
    }
}

/// Single tool entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolEntry {
    pub id: String,
    pub description: String,
    pub transport: Transport,
    #[serde(default)]
    pub input_schema: JsonValue,
    #[serde(default)]
    pub result_schema: JsonValue,
    #[serde(default)]
    pub schema_refs: BTreeMap<String, JsonValue>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub budget: Option<Budget>,
    #[serde(default)]
    pub requires_confirmation: bool,
    #[serde(default)]
    pub observability: Option<Observability>,
}

impl ToolEntry {
    fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("tool id cannot be empty".into());
        }
        if self.description.trim().is_empty() {
            return Err("description cannot be empty".into());
        }
        self.transport.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Transport {
    Exec {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        method: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

impl Transport {
    fn validate(&self) -> Result<(), String> {
        match self {
            Transport::Exec { command, .. } => {
                if command.trim().is_empty() {
                    return Err("exec transport requires a command".into());
                }
            }
            Transport::Http { method, url, .. } => {
                if method.trim().is_empty() {
                    return Err("http transport requires a method".into());
                }
                if url.trim().is_empty() {
                    return Err("http transport requires a url".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Budget {
    #[serde(default)]
    pub tokens: Option<u64>,
    #[serde(default)]
    pub usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Observability {
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Load and validate a tools.json document from disk.
pub fn load_tools(path: &Path) -> Result<ToolsDoc, AgentRegistryError> {
    let raw = fs::read_to_string(path).map_err(|source| AgentRegistryError::IoPath {
        path: path.to_path_buf(),
        source,
    })?;
    let doc: ToolsDoc = serde_json::from_str(&raw).map_err(|source| AgentRegistryError::Tools {
        reference: None,
        detail: format!("invalid JSON: {source}"),
    })?;
    doc.validate().map_err(|detail| AgentRegistryError::Tools {
        reference: None,
        detail,
    })?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn validates_version_and_entries() {
        let doc = ToolsDoc {
            version: 1,
            tools: vec![ToolEntry {
                id: "mail.smtp_send".into(),
                description: "send mail".into(),
                transport: Transport::Http {
                    method: "POST".into(),
                    url: "https://example.com".into(),
                    headers: BTreeMap::new(),
                },
                input_schema: JsonValue::Null,
                result_schema: JsonValue::Null,
                schema_refs: BTreeMap::new(),
                capabilities: vec!["net:example.com".into()],
                secrets: vec![],
                budget: Some(Budget {
                    tokens: None,
                    usd: Some(1.0),
                }),
                requires_confirmation: true,
                observability: Some(Observability { tags: vec![] }),
            }],
        };
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn load_tools_reads_file() {
        let mut tmp = NamedTempFile::new().expect("tmp file");
        let doc = ToolsDoc {
            version: 1,
            tools: Vec::new(),
        };
        let json = serde_json::to_string(&doc).expect("serialize");
        std::io::Write::write_all(&mut tmp, json.as_bytes()).expect("write");
        let loaded = load_tools(tmp.path()).expect("load tools");
        assert_eq!(loaded, doc);
    }
}
