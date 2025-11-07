use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use camino::Utf8PathBuf;

use crate::caps::Caps;
use crate::error::Error;
use crate::policy;

/// Logical agent identity (used for overrides and audit tags).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentIdentity {
    name: String,
    variant: Option<String>,
}

impl AgentIdentity {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            variant: None,
        }
    }

    pub fn with_variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into());
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn variant(&self) -> Option<&str> {
        self.variant.as_deref()
    }
}

/// Specification for spawning an agent instance.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub identity: AgentIdentity,
    pub wasm_path: PathBuf,
    pub policy_path: PathBuf,
    pub caps: Caps,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub stdout_capacity: usize,
    pub stderr_capacity: usize,
    pub working_dir: Option<Utf8PathBuf>,
    pub spawn_ready_timeout_ms: Option<u64>,
}

impl AgentSpec {
    pub fn builder(identity: AgentIdentity, wasm_path: impl Into<PathBuf>) -> AgentSpecBuilder {
        AgentSpecBuilder {
            identity,
            wasm_path: wasm_path.into(),
            policy_path: None,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            stdout_capacity: 64 * 1024,
            stderr_capacity: 64 * 1024,
            working_dir: None,
            spawn_ready_timeout_ms: None,
        }
    }

    pub fn sanitize(&mut self) {
        for value in self.env.values_mut() {
            *value = value.trim().to_string();
        }
    }
}

pub struct AgentSpecBuilder {
    identity: AgentIdentity,
    wasm_path: PathBuf,
    policy_path: Option<PathBuf>,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    stdout_capacity: usize,
    stderr_capacity: usize,
    working_dir: Option<Utf8PathBuf>,
    spawn_ready_timeout_ms: Option<u64>,
}

impl AgentSpecBuilder {
    pub fn policy_path(mut self, path: impl AsRef<Path>) -> Self {
        self.policy_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn argv<I, S>(mut self, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.argv = argv.into_iter().map(Into::into).collect();
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    pub fn stdout_capacity(mut self, capacity: usize) -> Self {
        self.stdout_capacity = capacity;
        self
    }

    pub fn stderr_capacity(mut self, capacity: usize) -> Self {
        self.stderr_capacity = capacity;
        self
    }

    pub fn working_dir(mut self, path: impl Into<Utf8PathBuf>) -> Self {
        self.working_dir = Some(path.into());
        self
    }

    pub fn spawn_ready_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.spawn_ready_timeout_ms = Some(timeout_ms);
        self
    }

    pub fn build(self) -> Result<AgentSpec, Error> {
        let policy_path = self
            .policy_path
            .ok_or_else(|| Error::Config("policy path is required".into()))?;
        let caps = policy::effective_caps(&self.identity, &policy_path)?;

        let mut spec = AgentSpec {
            identity: self.identity,
            wasm_path: self.wasm_path,
            policy_path,
            caps,
            argv: self.argv,
            env: self.env,
            cwd: self.cwd,
            stdout_capacity: self.stdout_capacity,
            stderr_capacity: self.stderr_capacity,
            working_dir: self.working_dir,
            spawn_ready_timeout_ms: self.spawn_ready_timeout_ms,
        };
        spec.sanitize();
        Ok(spec)
    }
}
