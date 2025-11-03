use crate::Error;
use dirs::{config_dir, home_dir};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Canonical Runloop configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub kb: KbConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub router: RouterConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub openings: SearchDirsConfig,
    #[serde(default)]
    pub agents: SearchDirsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            runtime: RuntimeConfig::default(),
            models: ModelsConfig::default(),
            kb: KbConfig::default(),
            security: SecurityConfig::default(),
            router: RouterConfig::default(),
            ui: UiConfig::default(),
            logging: LoggingConfig::default(),
            openings: SearchDirsConfig {
                search_dirs: vec!["~/.runloop/openings".into(), "/etc/runloop/openings".into()],
            },
            agents: SearchDirsConfig {
                search_dirs: vec!["~/.runloop/agents".into(), "/usr/lib/runloop/agents".into()],
            },
        }
    }
}

impl Config {
    /// Load configuration from defaults + files + environment overrides.
    pub fn load() -> Result<Self, Error> {
        Self::load_from_sources(config_candidate_paths(), env::vars())
    }

    fn load_from_sources(
        paths: Vec<PathBuf>,
        env_pairs: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, Error> {
        let mut base = serde_json::to_value(Config::default()).expect("serialize default config");

        for path in paths {
            match load_yaml_value(&path) {
                Ok(Some(value)) => {
                    warn_unknown_keys(&value, Path::new(""));
                    merge_value(&mut base, value);
                }
                Ok(None) => {}
                Err(err) => return Err(err),
            }
        }

        if let Some(obj) = base.as_object_mut() {
            apply_env_overrides(obj, env_pairs);
        }

        let mut config: Config = serde_json::from_value(base)
            .map_err(|err| Error::Config(format!("config deserialization failed: {err}")))?;
        config.expand_paths();
        config.validate()?;
        Ok(config)
    }

    /// Validate required invariants.
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(Error::Config(format!(
                "unsupported config version {}; expected 1",
                self.version
            )));
        }
        if self.runtime.agent_container != "wasm32-wasi" {
            return Err(Error::Config(format!(
                "runtime.agent_container must be \"wasm32-wasi\" for MVP (found {})",
                self.runtime.agent_container
            )));
        }
        if self.security.confirm_external_actions && self.router.denylist.is_empty() {
            warn!(
                "router denylist empty while confirmations required; consider keeping protective entries"
            );
        }
        Ok(())
    }

    fn expand_paths(&mut self) {
        self.runtime.workdir = expand_path(&self.runtime.workdir);
        self.runtime.sockets_dir = expand_path(&self.runtime.sockets_dir);

        for provider in &mut self.models.broker.providers {
            if let Some(dir) = &provider.model_dir {
                provider.model_dir = Some(expand_path(dir));
            }
        }

        self.kb.root_dir = expand_path(&self.kb.root_dir);
        if let Some(root) = &self.logging.file {
            self.logging.file = Some(expand_path(root));
        }
        if let Some(root) = &self.security.secrets.root {
            self.security.secrets.root = Some(expand_path(root));
        }

        for dir in &mut self.openings.search_dirs {
            *dir = expand_path(dir);
        }
        for dir in &mut self.agents.search_dirs {
            *dir = expand_path(dir);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_runtime_base")]
    pub base: String,
    #[serde(default = "default_agent_container")]
    pub agent_container: String,
    #[serde(default = "default_runtime_workdir")]
    pub workdir: String,
    #[serde(default = "default_runtime_sockets_dir")]
    pub sockets_dir: String,
    #[serde(default)]
    pub max_agents: u32,
    #[serde(default)]
    pub pressure_threshold: PressureThreshold,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            base: default_runtime_base(),
            agent_container: default_agent_container(),
            workdir: default_runtime_workdir(),
            sockets_dir: default_runtime_sockets_dir(),
            max_agents: 0,
            pressure_threshold: PressureThreshold::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PressureThreshold {
    #[serde(default = "default_cpu_pct")]
    pub cpu_pct: u8,
    #[serde(default = "default_mem_pct")]
    pub mem_pct: u8,
    #[serde(default = "default_io_wait_pct")]
    pub io_wait_pct: u8,
}

impl Default for PressureThreshold {
    fn default() -> Self {
        Self {
            cpu_pct: default_cpu_pct(),
            mem_pct: default_mem_pct(),
            io_wait_pct: default_io_wait_pct(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelsConfig {
    #[serde(default = "default_models_default")]
    pub default: String,
    #[serde(default)]
    pub broker: BrokerConfig,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            default: default_models_default(),
            broker: BrokerConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrokerConfig {
    #[serde(default)]
    pub providers: Vec<ModelProvider>,
    #[serde(default)]
    pub routing: BTreeMap<String, String>,
    #[serde(default)]
    pub budgets: ModelBudgets,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        let mut routing = BTreeMap::new();
        routing.insert("*".into(), "local".into());
        Self {
            providers: Vec::new(),
            routing,
            budgets: ModelBudgets::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelProvider {
    pub id: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub model_dir: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub auth: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Local,
    Http,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelBudgets {
    #[serde(default = "default_broker_default_tokens")]
    pub default_tokens: u32,
    #[serde(default = "default_per_request_cap")]
    pub per_request_tokens_cap: u32,
    #[serde(default)]
    pub hard_cap_usd: Option<f32>,
}

impl Default for ModelBudgets {
    fn default() -> Self {
        Self {
            default_tokens: default_broker_default_tokens(),
            per_request_tokens_cap: default_per_request_cap(),
            hard_cap_usd: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KbConfig {
    #[serde(default = "default_kb_root")]
    pub root_dir: String,
    #[serde(default = "default_events_db")]
    pub events_db: String,
    #[serde(default = "default_view_db")]
    pub view_db: String,
    #[serde(default = "default_true")]
    pub wal: bool,
    #[serde(default)]
    pub fts: bool,
    #[serde(default)]
    pub redaction: KbRedactionConfig,
}

impl Default for KbConfig {
    fn default() -> Self {
        Self {
            root_dir: default_kb_root(),
            events_db: default_events_db(),
            view_db: default_view_db(),
            wal: true,
            fts: false,
            redaction: KbRedactionConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KbRedactionConfig {
    #[serde(default = "default_true")]
    pub mask_email: bool,
    #[serde(default = "default_redaction_level")]
    pub level: String,
    #[serde(default = "default_true")]
    pub at_query_time: bool,
    #[serde(default = "default_true")]
    pub materialize_masked_columns: bool,
    #[serde(default = "default_url_param_denylist")]
    pub url_param_denylist: Vec<String>,
}

impl Default for KbRedactionConfig {
    fn default() -> Self {
        Self {
            mask_email: true,
            level: default_redaction_level(),
            at_query_time: true,
            materialize_masked_columns: true,
            url_param_denylist: default_url_param_denylist(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default = "default_true")]
    pub confirm_external_actions: bool,
    #[serde(default = "default_true")]
    pub allow_unsigned_agents: bool,
    #[serde(default)]
    pub allowed_agent_signers: Vec<String>,
    #[serde(default)]
    pub caps: SecurityCapsConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
    #[serde(default)]
    pub testing: Option<TestingConfig>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            confirm_external_actions: true,
            allow_unsigned_agents: true,
            allowed_agent_signers: Vec::new(),
            caps: SecurityCapsConfig::default(),
            secrets: SecretsConfig::default(),
            testing: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityCapsConfig {
    #[serde(default)]
    pub audit_on_allow: bool,
    #[serde(default = "default_true")]
    pub audit_on_deny: bool,
}

impl Default for SecurityCapsConfig {
    fn default() -> Self {
        Self {
            audit_on_allow: false,
            audit_on_deny: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecretsConfig {
    #[serde(default = "default_secrets_provider")]
    pub provider: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default = "default_secrets_encryption")]
    pub encryption: String,
    #[serde(default)]
    pub master_key: Option<String>,
    #[serde(default)]
    pub allow_export: bool,
    #[serde(default)]
    pub allow_list: bool,
    #[serde(default)]
    pub default_ttl: u64,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            provider: default_secrets_provider(),
            root: Some("~/.runloop/secrets".into()),
            encryption: default_secrets_encryption(),
            master_key: None,
            allow_export: false,
            allow_list: false,
            default_ttl: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestingConfig {
    #[serde(default)]
    pub broker_mode: Option<String>,
    #[serde(default)]
    pub broker_seed: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouterConfig {
    #[serde(default = "default_true")]
    pub shell_fastpath: bool,
    #[serde(default = "default_router_opening")]
    pub default_opening: String,
    #[serde(default = "default_router_denylist")]
    pub denylist: Vec<String>,
    #[serde(default)]
    pub allowlist: Vec<String>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            shell_fastpath: true,
            default_opening: default_router_opening(),
            denylist: default_router_denylist(),
            allowlist: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_ui_theme")]
    pub theme: String,
    #[serde(default = "default_ui_confirm_prompts")]
    pub confirm_prompts: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: default_ui_theme(),
            confirm_prompts: default_ui_confirm_prompts(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_logging_level")]
    pub level: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default = "default_logging_format")]
    pub format: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_logging_level(),
            file: None,
            format: default_logging_format(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchDirsConfig {
    #[serde(default)]
    pub search_dirs: Vec<String>,
}

// Defaults helpers
fn default_runtime_base() -> String {
    "debian".into()
}
fn default_agent_container() -> String {
    "wasm32-wasi".into()
}
fn default_runtime_workdir() -> String {
    "~/.runloop".into()
}
fn default_runtime_sockets_dir() -> String {
    "~/.runloop/sock".into()
}
fn default_cpu_pct() -> u8 {
    90
}
fn default_mem_pct() -> u8 {
    90
}
fn default_io_wait_pct() -> u8 {
    50
}
fn default_models_default() -> String {
    "null:echo".into()
}
fn default_broker_default_tokens() -> u32 {
    8_000
}
fn default_per_request_cap() -> u32 {
    2_000
}
fn default_kb_root() -> String {
    "~/.runloop/pog".into()
}
fn default_events_db() -> String {
    "events.sqlite".into()
}
fn default_view_db() -> String {
    "pog.sqlite".into()
}
fn default_true() -> bool {
    true
}
fn default_redaction_level() -> String {
    "strict".into()
}
fn default_url_param_denylist() -> Vec<String> {
    vec![
        "token".into(),
        "key".into(),
        "password".into(),
        "signature".into(),
        "auth".into(),
    ]
}
fn default_secrets_provider() -> String {
    "stub".into()
}
fn default_secrets_encryption() -> String {
    "none".into()
}
fn default_router_opening() -> String {
    "compose_email".into()
}
fn default_router_denylist() -> Vec<String> {
    vec!["rm -rf /".into()]
}
fn default_ui_theme() -> String {
    "mono".into()
}
fn default_ui_confirm_prompts() -> String {
    "inline".into()
}
fn default_logging_level() -> String {
    "info".into()
}
fn default_logging_format() -> String {
    "plain".into()
}

fn config_candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(path) = env::var("RUNLOOP_CONFIG") {
        paths.push(PathBuf::from(path));
    } else {
        if let Some(xdg) = config_dir() {
            paths.push(xdg.join("runloop").join("config.yaml"));
        }
        if let Some(home) = home_dir() {
            paths.push(home.join(".runloop").join("config.yaml"));
        }
    }
    paths
}

fn load_yaml_value(path: &Path) -> Result<Option<Value>, Error> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read(path).map_err(|err| {
        Error::Config(format!(
            "failed reading config file {}: {err}",
            path.display()
        ))
    })?;
    let value: serde_yaml::Value = serde_yaml::from_slice(&content)
        .map_err(|err| Error::Config(format!("invalid YAML in {}: {err}", path.display())))?;
    let value = serde_json::to_value(value).map_err(|err| {
        Error::Config(format!(
            "config conversion error in {}: {err}",
            path.display()
        ))
    })?;
    Ok(Some(value))
}

fn merge_value(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (key, value) in patch_map {
                match base_map.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        base_map.insert(key, value);
                    }
                }
            }
        }
        (base_slot, patch_value) => {
            *base_slot = patch_value;
        }
    }
}

fn apply_env_overrides(
    root: &mut serde_json::Map<String, Value>,
    env_pairs: impl IntoIterator<Item = (String, String)>,
) {
    for (key, value) in env_pairs {
        if let Some(stripped) = key.strip_prefix("RUNLOOP__") {
            let path = stripped
                .split("__")
                .map(|segment| segment.to_ascii_lowercase())
                .collect::<Vec<_>>();
            set_path(root, &path, parse_env_value(&value));
        } else if let Some((path, parsed)) = parse_alias(&key, &value) {
            set_path(root, &path, parsed);
        }
    }
}

fn parse_env_value(raw: &str) -> Value {
    match raw {
        "true" | "TRUE" | "1" => Value::Bool(true),
        "false" | "FALSE" | "0" => Value::Bool(false),
        other => {
            if let Ok(int) = other.parse::<i64>() {
                Value::Number(int.into())
            } else if let Ok(float) = other.parse::<f64>() {
                serde_json::Number::from_f64(float)
                    .map(Value::Number)
                    .unwrap_or_else(|| Value::String(other.to_string()))
            } else if (other.starts_with('{') && other.ends_with('}'))
                || (other.starts_with('[') && other.ends_with(']'))
            {
                serde_json::from_str(other).unwrap_or_else(|_| Value::String(other.to_string()))
            } else {
                Value::String(other.to_string())
            }
        }
    }
}

fn set_path(root: &mut serde_json::Map<String, Value>, path: &[String], value: Value) {
    if path.is_empty() {
        return;
    }
    let mut cursor = root;
    for key in &path[..path.len() - 1] {
        cursor = cursor
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Default::default()))
            .as_object_mut()
            .expect("intermediate config node must be object");
    }
    if let Some(last) = path.last() {
        cursor.insert(last.clone(), value);
    }
}

fn parse_alias(key: &str, value: &str) -> Option<(Vec<String>, Value)> {
    let aliases: [(&str, &[&str]); 6] = [
        ("RUNLOOP_LOG_LEVEL", &["logging", "level"]),
        (
            "RUNLOOP_RUNTIME_AGENT_CONTAINER",
            &["runtime", "agent_container"],
        ),
        (
            "RUNLOOP_ROUTER_DEFAULT_OPENING",
            &["router", "default_opening"],
        ),
        ("RUNLOOP_MODELS_DEFAULT", &["models", "default"]),
        ("RUNLOOP_KB_ROOT_DIR", &["kb", "root_dir"]),
        (
            "RUNLOOP_SECURITY_CONFIRM_EXTERNAL_ACTIONS",
            &["security", "confirm_external_actions"],
        ),
    ];
    for (alias, path) in aliases {
        if key == alias {
            return Some((
                path.iter().map(|s| s.to_string()).collect(),
                parse_env_value(value),
            ));
        }
    }
    None
}

fn expand_path(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = home_dir() {
            return home.join(&path[2..]).display().to_string();
        }
    }
    if let Some(stripped) = path.strip_prefix("env:") {
        if let Ok(val) = env::var(stripped) {
            return val;
        }
    }
    shellexpand::full(path)
        .map(|cow| cow.to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn warn_unknown_keys(document: &Value, path: &Path) {
    if let Value::Object(map) = document {
        let known = known_keys_for_path(path);
        if !known.is_empty() {
            for key in map.keys() {
                if !known.contains(key.as_str()) {
                    warn!(
                        "unknown configuration key '{}' under '{}'",
                        key,
                        path_string(path)
                    );
                }
            }
        }
        for key in map.keys() {
            let mut new_path = path.to_path_buf();
            new_path.push(key);
            warn_unknown_keys(&map[key], &new_path);
        }
    }
}

fn known_keys_for_path(path: &Path) -> BTreeSet<&'static str> {
    match path_string(path).as_str() {
        "" => BTreeSet::from([
            "version", "runtime", "models", "kb", "security", "router", "ui", "logging",
            "openings", "agents",
        ]),
        "runtime" => BTreeSet::from([
            "base",
            "agent_container",
            "workdir",
            "sockets_dir",
            "max_agents",
            "pressure_threshold",
        ]),
        "runtime/pressure_threshold" => BTreeSet::from(["cpu_pct", "mem_pct", "io_wait_pct"]),
        "models" => BTreeSet::from(["default", "broker"]),
        "models/broker" => BTreeSet::from(["providers", "routing", "budgets"]),
        "kb" => BTreeSet::from([
            "root_dir",
            "events_db",
            "view_db",
            "wal",
            "fts",
            "redaction",
        ]),
        "kb/redaction" => BTreeSet::from([
            "mask_email",
            "level",
            "at_query_time",
            "materialize_masked_columns",
            "url_param_denylist",
        ]),
        "security" => BTreeSet::from([
            "confirm_external_actions",
            "allow_unsigned_agents",
            "allowed_agent_signers",
            "caps",
            "secrets",
            "testing",
        ]),
        "security/caps" => BTreeSet::from(["audit_on_allow", "audit_on_deny"]),
        "security/secrets" => BTreeSet::from([
            "provider",
            "root",
            "encryption",
            "master_key",
            "allow_export",
            "allow_list",
            "default_ttl",
        ]),
        "security/testing" => BTreeSet::from(["broker_mode", "broker_seed"]),
        "router" => BTreeSet::from(["shell_fastpath", "default_opening", "denylist", "allowlist"]),
        "ui" => BTreeSet::from(["theme", "confirm_prompts"]),
        "logging" => BTreeSet::from(["level", "file", "format"]),
        "openings" | "agents" => BTreeSet::from(["search_dirs"]),
        _ => BTreeSet::new(),
    }
}

fn path_string(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::Config;
    use std::io::Write;

    #[test]
    fn defaults_validate() {
        let config = Config::default();
        config.validate().expect("default config should validate");
    }

    #[test]
    fn load_with_file_and_env_overrides() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("config.yaml");
        let mut file = std::fs::File::create(&config_path).expect("create config file");
        write!(
            file,
            r#"
version: 1
runtime:
  agent_container: "wasm32-wasi"
kb:
  root_dir: "~/.custom-pog"
security:
  confirm_external_actions: false
"#
        )
        .expect("write config");
        let env_pairs = vec![
            ("RUNLOOP__MODELS__DEFAULT".into(), "local:test".into()),
            (
                "RUNLOOP__ROUTER__DEFAULT_OPENING".into(),
                "compose_support_ticket".into(),
            ),
        ];
        let config = Config::load_from_sources(vec![config_path], env_pairs).expect("config load");
        assert_eq!(config.models.default, "local:test");
        assert_eq!(config.router.default_opening, "compose_support_ticket");
        assert_eq!(config.security.confirm_external_actions, false);
        assert!(config.kb.root_dir.ends_with(".custom-pog"));
    }
}
