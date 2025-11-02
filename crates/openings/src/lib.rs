use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpeningSpec {
    pub version: u8,
    pub nodes: IndexMap<String, NodeSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeSpec {
    #[serde(rename = "use")]
    pub uses: String,
    #[serde(default)]
    pub with: Option<serde_yaml::Value>,
}

#[derive(Debug, Error)]
pub enum OpeningsError {
    #[error("failed to parse opening: {0}")]
    Parse(#[from] serde_yaml::Error),
}

pub fn parse_opening(input: &str) -> Result<OpeningSpec, OpeningsError> {
    serde_yaml::from_str(input).map_err(OpeningsError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_example() {
        let yaml = r#"
version: 1
nodes:
  gather:
    use: context_gatherer
  write:
    use: writer
    with:
      style: concise
"#;
        let spec = parse_opening(yaml).unwrap();
        assert_eq!(spec.version, 1);
        assert!(spec.nodes.contains_key("gather"));
    }
}
