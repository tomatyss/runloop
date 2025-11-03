use std::fs;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use toml::Value;

use crate::caps::{Caps, CapsParse};
use crate::error::Error;
use crate::spec::AgentIdentity;

fn load_caps_parse(path: &Path) -> Result<CapsParse, Error> {
    let contents = fs::read_to_string(path)?;
    let value: Value = toml::from_str(&contents)?;
    Caps::from_policy(&value)
}

pub fn effective_caps(identity: &AgentIdentity, policy_path: &Path) -> Result<Caps, Error> {
    let base = load_caps_parse(policy_path)?;
    let mut effective = base.caps;
    for override_path in override_paths(identity) {
        if !override_path.exists() {
            continue;
        }
        let override_caps = load_caps_parse(&override_path).map_err(|err| {
            Error::Override(format!(
                "failed to apply override {}: {err}",
                override_path.display()
            ))
        })?;
        effective = effective.intersect_with(&override_caps.caps, &override_caps.presence);
    }
    Ok(effective)
}

fn override_paths(identity: &AgentIdentity) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let key = sanitized_identity(identity);

    // System override.
    let system_path = PathBuf::from(format!("/etc/runloop/policy-overrides/{key}.caps"));
    paths.push(system_path);

    // User override.
    if let Some(base) = BaseDirs::new() {
        let mut user_path = base.home_dir().to_path_buf();
        user_path.push(".runloop");
        user_path.push("policy-overrides");
        user_path.push(&key);
        user_path.push("policy.caps");
        paths.push(user_path);
    }

    paths
}

fn sanitized_identity(identity: &AgentIdentity) -> String {
    let mut key = identity
        .name()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "_");
    if let Some(variant) = identity.variant() {
        key.push('_');
        key.push_str(&variant.replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
    }
    key
}
