//! Capability manifest structures shared by the shim and native agents.
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};

/// Error surfaced while parsing or validating capability manifests.
#[derive(Debug)]
pub enum CapsParseError {
    /// Generic validation failure.
    Invalid(String),
    /// JSON (de)serialization failed.
    Json(serde_json::Error),
}

impl From<serde_json::Error> for CapsParseError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl fmt::Display for CapsParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(msg) => write!(f, "{msg}"),
            Self::Json(err) => write!(f, "{err}"),
        }
    }
}

/// Filesystem capability entry describing a preopened root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsAccess {
    /// Absolute host path.
    pub root: Utf8PathBuf,
    /// Whether writes are permitted within the root.
    #[serde(default)]
    pub write: bool,
}

impl FsAccess {
    /// Returns `true` if the provided path is inside this root.
    pub fn contains(&self, path: &Utf8Path) -> bool {
        let root_clean = clean_path(&self.root);
        let path_clean = clean_path(path);
        path_clean.starts_with(&root_clean)
    }
}

/// Network capability entry describing a host (and optional port) allow-list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetAccess {
    /// Hostname or literal address.
    pub host: String,
    /// Optional TCP port. When missing, any port is permitted.
    #[serde(default)]
    pub port: Option<u16>,
}

impl NetAccess {
    /// Returns `true` if the host/port tuple is allowed by this entry.
    pub fn matches(&self, host: &str, port: Option<u16>) -> bool {
        if !host.eq_ignore_ascii_case(&self.host) {
            return false;
        }
        match (self.port, port) {
            (Some(expected), Some(actual)) => expected == actual,
            (Some(_), None) => false,
            _ => true,
        }
    }
}

/// Capability scope for KB read/write namespaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NamespaceCaps {
    /// Whether all namespaces are permitted.
    #[serde(default)]
    pub allow_all: bool,
    /// Explicit namespace allow-list when `allow_all` is false.
    #[serde(default)]
    pub domains: BTreeSet<String>,
}
impl NamespaceCaps {
    /// Returns `true` if the provided namespace is allowed.
    pub fn permits(&self, namespace: &str) -> bool {
        self.allow_all || self.domains.contains(namespace)
    }
}

/// Effective capabilities granted to a shim/agent process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EffectiveCaps {
    /// Filesystem roots available to the agent.
    #[serde(default)]
    pub fs: Vec<FsAccess>,
    /// Network destinations permitted for HTTP(S) calls.
    #[serde(default)]
    pub net: Vec<NetAccess>,
    /// Whether raw HTTP (port 80) is allowed.
    #[serde(default)]
    pub net_allow_http: bool,
    /// Whether wall/monotonic clocks may be read.
    #[serde(default)]
    pub time: bool,
    /// Knowledge base namespaces the agent can read.
    #[serde(default)]
    pub kb_read: NamespaceCaps,
    /// Knowledge base namespaces the agent can write.
    #[serde(default)]
    pub kb_write: NamespaceCaps,
    /// Whether model completions are permitted.
    #[serde(default)]
    pub model: bool,
    /// Secrets identifiers available to the agent.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Whether host exec is allowed (disabled for MVP).
    #[serde(default)]
    pub exec: bool,
}

impl EffectiveCaps {
    /// Deserialize an `EffectiveCaps` from the JSON string provided by the runtime.
    pub fn from_json(raw: &str) -> Result<Self, CapsParseError> {
        let mut caps: Self = serde_json::from_str(raw)?;
        caps.sanitize();
        Ok(caps)
    }

    /// Returns `true` if the provided filesystem path is readable.
    pub fn allows_read(&self, path: &Utf8Path) -> bool {
        self.fs.iter().any(|entry| entry.contains(path))
    }

    /// Returns `true` if the provided filesystem path is writable.
    pub fn allows_write(&self, path: &Utf8Path) -> bool {
        self.fs
            .iter()
            .any(|entry| entry.write && entry.contains(path))
    }

    /// Returns `true` if the requested network target is allowed.
    pub fn allows_host(&self, host: &str, port: Option<u16>) -> bool {
        if matches!(port, Some(80)) && !self.net_allow_http {
            return false;
        }
        self.net.iter().any(|entry| entry.matches(host, port))
    }

    fn sanitize(&mut self) {
        self.fs.retain(|entry| !entry.root.as_str().is_empty());
        self.net.retain(|entry| !entry.host.trim().is_empty());
        self.secrets.retain(|secret| !secret.trim().is_empty());
    }
}

fn clean_path(path: &Utf8Path) -> Utf8PathBuf {
    let mut segments: Vec<String> = Vec::new();
    let mut absolute = false;
    for comp in Path::new(path.as_str()).components() {
        match comp {
            Component::Prefix(_) => {}
            Component::RootDir => {
                absolute = true;
                segments.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                segments.pop();
            }
            Component::Normal(seg) => segments.push(seg.to_string_lossy().into_owned()),
        }
    }
    let mut result = Utf8PathBuf::new();
    if absolute {
        result.push("/");
    }
    for segment in segments {
        result.push(segment);
    }
    if result.as_str().is_empty() {
        result.push(".");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_caps_from_json() {
        let raw = r#"{
            "fs":[{"root":"/tmp/a","write":true},{"root":"/opt/ro","write":false}],
            "net":[{"host":"example.com","port":443}],
            "kb_read":{"allow_all":false,"domains":["contacts"]},
            "kb_write":{"allow_all":false,"domains":["contacts"]},
            "model":true
        }"#;
        let caps = EffectiveCaps::from_json(raw).expect("parse caps");
        assert!(caps.allows_read(Utf8Path::new("/tmp/a/file")));
        assert!(caps.allows_write(Utf8Path::new("/tmp/a/file")));
        assert!(!caps.allows_write(Utf8Path::new("/opt/ro/file")));
        assert!(caps.allows_host("example.com", Some(443)));
        assert!(!caps.allows_host("example.com", Some(80)));
        assert!(caps.kb_read.permits("contacts"));
        assert!(!caps.kb_read.permits("artifacts"));
        assert!(caps.model);
    }

    #[test]
    fn namespace_permits_all_when_flag_set() {
        let caps = NamespaceCaps {
            allow_all: true,
            domains: BTreeSet::new(),
        };
        assert!(caps.permits("anything"));
    }

    #[test]
    fn fs_contains_rejects_siblings_and_parent() {
        let access = FsAccess {
            root: Utf8PathBuf::from("/tmp/a"),
            write: true,
        };
        assert!(access.contains(Utf8Path::new("/tmp/a/file.txt")));
        assert!(!access.contains(Utf8Path::new("/tmp/ab/file.txt")));
        assert!(!access.contains(Utf8Path::new("/tmp/a/../etc/passwd")));
    }

    #[test]
    fn http_denied_when_flag_false() {
        let mut caps = EffectiveCaps::default();
        caps.net.push(NetAccess {
            host: "example.com".into(),
            port: Some(80),
        });
        caps.net_allow_http = false;
        assert!(!caps.allows_host("example.com", Some(80)));
        caps.net_allow_http = true;
        assert!(caps.allows_host("example.com", Some(80)));
    }
}
