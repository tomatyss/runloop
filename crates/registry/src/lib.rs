use dirs::home_dir;
use runloop_core::Config;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PathOverrides {
    #[serde(default)]
    pub agents_dir: Option<PathBuf>,
    #[serde(default)]
    pub openings_dir: Option<PathBuf>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RegistryPaths {
    pub agents: Vec<PathBuf>,
    pub openings: Vec<PathBuf>,
    /// Populated when no configured dirs exist so callers can emit a demo banner.
    pub demo_agents: Option<PathBuf>,
    pub info: Vec<String>,
    pub warnings: Vec<String>,
}

/// Resolve search paths for agents and openings using config defaults, optional workspace
/// detection, and CLI overrides.
pub fn resolve_paths(config: &Config, overrides: &PathOverrides) -> RegistryPaths {
    let cwd = overrides.cwd.as_deref();
    let workspace = overrides
        .workspace_root
        .clone()
        .or_else(|| detect_workspace_root(cwd));

    let mut agents = Vec::new();
    if let Some(dir) = overrides.agents_dir.as_ref() {
        push_unique(&mut agents, expand_path(dir));
    } else if let Some(ws) = &workspace {
        push_unique(&mut agents, ws.join("agents"));
    }
    for entry in &config.agents.search_dirs {
        push_unique(&mut agents, expand_str_path(entry));
    }

    let mut openings = Vec::new();
    if let Some(dir) = overrides.openings_dir.as_ref() {
        push_unique(&mut openings, expand_path(dir));
    } else if let Some(ws) = &workspace {
        push_unique(&mut openings, ws.join("examples").join("openings"));
    }
    for entry in &config.openings.search_dirs {
        push_unique(&mut openings, expand_str_path(entry));
    }

    let missing_agents = agents.iter().all(|dir| !dir.exists());
    let missing_openings = openings.iter().all(|dir| !dir.exists());

    let mut info = Vec::new();
    let mut warnings = Vec::new();
    let mut demo_agents = None;
    if missing_agents {
        let demo_dir = default_demo_agents_dir();
        info.push(
            "no agent search dirs found; using demo bundles until custom bundles are added".into(),
        );
        push_unique(&mut agents, demo_dir.clone());
        demo_agents = Some(demo_dir);
    }
    if missing_openings {
        warnings.push(
            "no opening search dirs exist; set --openings-dir or configure openings.search_dirs"
                .into(),
        );
    }

    RegistryPaths {
        agents,
        openings,
        demo_agents,
        info,
        warnings,
    }
}

#[derive(Debug, Error)]
pub enum OpeningLookupError {
    #[error("opening '{name}' not found (searched: {searched})")]
    NotFound { name: String, searched: String },
}

pub fn resolve_opening_path(
    name: &str,
    search_dirs: &[PathBuf],
) -> Result<PathBuf, OpeningLookupError> {
    let raw_path = PathBuf::from(name);
    if raw_path.is_file() {
        return Ok(raw_path);
    }

    let mut candidates = Vec::new();

    let try_names = if raw_path.extension().is_some() {
        vec![raw_path.clone()]
    } else {
        vec![raw_path.with_extension("yaml"), raw_path]
    };

    for dir in search_dirs {
        if !dir.exists() {
            continue;
        }
        for candidate in &try_names {
            let path = dir.join(candidate);
            if path.is_file() {
                return Ok(path);
            }
            candidates.push(path);
        }
    }

    Err(OpeningLookupError::NotFound {
        name: name.to_string(),
        searched: candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    })
}

fn push_unique(list: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !list.iter().any(|existing| existing == &candidate) {
        list.push(candidate);
    }
}

fn expand_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    expand_str_path(&raw)
}

fn expand_str_path(raw: &str) -> PathBuf {
    if let Some(home) = home_dir() {
        if raw == "~" {
            return home;
        }
        if let Some(stripped) = raw.strip_prefix("~/") {
            return home.join(stripped);
        }
    }
    if let Some(value) = raw
        .strip_prefix("env:")
        .and_then(|rest| env::var(rest).ok())
    {
        return PathBuf::from(value);
    }
    PathBuf::from(raw)
}

fn detect_workspace_root(cwd: Option<&Path>) -> Option<PathBuf> {
    if let Some(env_root) = env::var_os("RUNLOOP_WORKSPACE_ROOT") {
        let candidate = expand_str_path(&env_root.to_string_lossy());
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    walk_for_workspace_root(cwd)
}

fn walk_for_workspace_root(cwd: Option<&Path>) -> Option<PathBuf> {
    let mut dir = cwd.map(PathBuf::from).or_else(|| env::current_dir().ok())?;
    let mut first_manifest_dir = None;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            if let Some(root) = manifest_workspace_root(&manifest) {
                return Some(root);
            }
            if first_manifest_dir.is_none() {
                first_manifest_dir = Some(dir.clone());
            }
        }
        if !dir.pop() {
            break;
        }
    }
    first_manifest_dir
}

fn manifest_workspace_root(manifest: &Path) -> Option<PathBuf> {
    let raw = fs::read_to_string(manifest).ok()?;
    let doc: toml::Value = toml::from_str(&raw).ok()?;
    let ws = doc.get("workspace")?;

    if ws.is_table() {
        return manifest.parent().map(PathBuf::from);
    }

    if let Some(rel) = ws.as_str() {
        let target = manifest
            .parent()
            .map(|p| p.join(rel))
            .unwrap_or_else(|| PathBuf::from(rel));
        let target_manifest = if target.is_dir() {
            target.join("Cargo.toml")
        } else {
            target.clone()
        };
        if target_manifest.is_file()
            && fs::read_to_string(&target_manifest)
                .ok()
                .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
                .and_then(|doc| doc.get("workspace").map(|val| val.is_table()))
                == Some(true)
        {
            return target_manifest.parent().map(PathBuf::from);
        }
    }
    None
}

fn default_demo_agents_dir() -> PathBuf {
    PathBuf::from("/usr/lib/runloop/agents")
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::Config;
    use tempfile::tempdir;

    #[test]
    fn override_precedes_config() {
        let mut config = Config::default();
        config.agents.search_dirs = vec!["/tmp/config-agents".into()];
        let overrides = PathOverrides {
            agents_dir: Some(PathBuf::from("/tmp/override-agents")),
            ..Default::default()
        };
        let resolved = resolve_paths(&config, &overrides);
        assert_eq!(
            resolved.agents.first().unwrap(),
            &PathBuf::from("/tmp/override-agents")
        );
        assert!(
            resolved
                .agents
                .contains(&PathBuf::from("/tmp/config-agents"))
        );
    }

    #[test]
    fn workspace_is_used_when_override_missing() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let config = Config::default();
        let overrides = PathOverrides {
            cwd: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let resolved = resolve_paths(&config, &overrides);
        assert!(resolved.agents.contains(&tmp.path().join("agents")));
        assert!(
            resolved
                .openings
                .contains(&tmp.path().join("examples").join("openings"))
        );
    }

    #[test]
    fn resolve_opening_uses_search_dirs() {
        let tmp = tempdir().unwrap();
        let openings_dir = tmp.path().join("openings");
        fs::create_dir_all(&openings_dir).unwrap();
        let target = openings_dir.join("demo.yaml");
        fs::write(&target, "version: 0\nname: demo\nnodes: []\n").unwrap();
        let resolved = resolve_opening_path("demo", &[openings_dir]).unwrap();
        assert_eq!(resolved, target);
    }

    #[test]
    fn missing_dirs_emit_fallback_info() {
        let tmp = tempdir().unwrap();
        let missing_agents = tmp.path().join("agents-missing");
        let missing_openings = tmp.path().join("openings-missing");
        let mut config = Config::default();
        config.agents.search_dirs = vec![missing_agents.display().to_string()];
        config.openings.search_dirs = vec![missing_openings.display().to_string()];
        let overrides = PathOverrides {
            cwd: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let resolved = resolve_paths(&config, &overrides);
        assert!(
            resolved
                .agents
                .iter()
                .any(|p| p == &default_demo_agents_dir()),
            "expected demo fallback in agent search dirs"
        );
        assert!(
            resolved.demo_agents.is_some(),
            "expected demo fallback marker"
        );
        assert!(
            resolved.info.iter().any(|msg| msg.contains("demo bundles")),
            "expected info banner when no search dirs exist"
        );
        assert!(
            resolved
                .warnings
                .iter()
                .any(|msg| msg.contains("opening search dirs")),
            "expected warning for missing openings"
        );
    }

    #[test]
    fn expands_tilde_in_config_paths() {
        let mut config = Config::default();
        config.agents.search_dirs = vec!["~/.runloop/agents".into()];
        let resolved = resolve_paths(&config, &PathOverrides::default());
        let home = home_dir().expect("home dir");
        assert!(
            resolved
                .agents
                .iter()
                .any(|p| p.starts_with(home.join(".runloop/agents"))),
            "expected ~ expansion in agents.search_dirs"
        );
    }
}
