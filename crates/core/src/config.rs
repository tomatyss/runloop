use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use tracing::warn;

/// Canonical Runloop configuration (version 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub kb: KbConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub router: RouterConfig,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub observability: ObservabilityConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: default_version(),
            kb: KbConfig::default(),
            logging: LoggingConfig::default(),
            security: SecurityConfig::default(),
            router: RouterConfig::default(),
            models: ModelsConfig::default(),
            observability: ObservabilityConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<ConfigLoadOutcome, ConfigError> {
        ConfigLoader::default().load(None::<&Path>)
    }

    pub fn validate(&self) -> Result<Vec<ConfigWarning>, ConfigError> {
        if self.version != 1 {
            return Err(ConfigError::UnsupportedVersion(self.version));
        }

        let mut warnings = Vec::new();

        if self.logging.format == LoggingFormat::DeprecatedJson {
            warnings.push(ConfigWarning::DeprecatedKey {
                key: "logging.format=deprecated-json".to_string(),
                note: "use logging.format = \"json\"".into(),
            });
        }

        Ok(warnings)
    }
}

pub struct ConfigLoader {
    env_prefix: String,
    path_env_var: Option<String>,
}

impl ConfigLoader {
    pub fn new(env_prefix: impl Into<String>, path_env_var: Option<String>) -> Self {
        Self {
            env_prefix: env_prefix.into(),
            path_env_var,
        }
    }

    pub fn load<P: AsRef<Path>>(
        &self,
        explicit_path: Option<P>,
    ) -> Result<ConfigLoadOutcome, ConfigError> {
        let root_path = explicit_path
            .map(|p| p.as_ref().to_path_buf())
            .or_else(|| {
                self.path_env_var
                    .as_ref()
                    .and_then(|key| env::var_os(key).map(PathBuf::from))
            })
            .unwrap_or_else(default_config_path);

        let mut warnings = Vec::new();
        let mut root_value = match fs::read_to_string(&root_path) {
            Ok(content) => serde_yaml::from_str::<Value>(&content)
                .map_err(|err| ConfigError::Parse(root_path.clone(), err.to_string()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
            Err(err) => return Err(ConfigError::Io(root_path.clone(), err)),
        };

        ensure_object(&mut root_value);
        apply_aliases(&mut root_value, &mut warnings);
        apply_env_overrides(&mut root_value, &self.env_prefix, &mut warnings)?;

        let mut config: Config = serde_json::from_value(root_value.clone())
            .map_err(|err| ConfigError::Parse(root_path.clone(), err.to_string()))?;

        config.kb.finalise_paths();

        let mut validation_warnings = config.validate()?;
        warnings.append(&mut validation_warnings);

        Ok(ConfigLoadOutcome { config, warnings })
    }
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self {
            env_prefix: "RUNLOOP__".to_string(),
            path_env_var: Some("RUNLOOP_CONFIG".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KbConfig {
    #[serde(default = "default_kb_root")]
    pub root_dir: PathBuf,
    #[serde(default = "default_events_db")]
    pub events_db: PathBuf,
    #[serde(default = "default_view_db")]
    pub view_db: PathBuf,
}

impl Default for KbConfig {
    fn default() -> Self {
        Self {
            root_dir: default_kb_root(),
            events_db: default_events_db(),
            view_db: default_view_db(),
        }
    }
}

impl KbConfig {
    pub fn events_db_path(&self) -> PathBuf {
        if self.events_db.is_absolute() {
            self.events_db.clone()
        } else {
            self.root_dir.join(&self.events_db)
        }
    }

    pub fn view_db_path(&self) -> PathBuf {
        if self.view_db.is_absolute() {
            self.view_db.clone()
        } else {
            self.root_dir.join(&self.view_db)
        }
    }

    fn finalise_paths(&mut self) {
        if !self.events_db.is_absolute() && self.events_db.components().next().is_none() {
            self.events_db = PathBuf::from("events.sqlite");
        }
        if !self.view_db.is_absolute() && self.view_db.components().next().is_none() {
            self.view_db = PathBuf::from("views.sqlite");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "LoggingFormat::default")]
    pub format: LoggingFormat,
    #[serde(default)]
    pub file: Option<PathBuf>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: LoggingFormat::default(),
            file: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoggingFormat {
    Text,
    Json,
    #[serde(rename = "deprecated-json")]
    DeprecatedJson,
}

impl Default for LoggingFormat {
    fn default() -> Self {
        LoggingFormat::Text
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouterConfig {
    #[serde(default = "default_router_opening")]
    pub default_opening: String,
    #[serde(default)]
    pub confirm_external: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            default_opening: default_router_opening(),
            confirm_external: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelsConfig {
    #[serde(default)]
    pub broker: BrokerConfig,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            broker: BrokerConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerConfig {
    #[serde(default)]
    pub endpoint: Option<url::Url>,
    #[serde(default)]
    pub cache_ttl_sec: Option<u64>,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            cache_ttl_sec: Some(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    #[serde(default)]
    pub otlp_endpoint: Option<url::Url>,
    #[serde(default = "default_sampling_ratio")]
    pub traces_sampling_ratio: f32,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: None,
            traces_sampling_ratio: default_sampling_ratio(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default)]
    pub secrets: SecretsConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            secrets: SecretsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretsConfig {
    #[serde(default = "SecretsProvider::default")]
    pub provider: SecretsProvider,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            provider: SecretsProvider::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretsProvider {
    Stub,
    OsKeyring,
    Age,
    Auto,
}

impl SecretsProvider {
    fn default() -> Self {
        SecretsProvider::Stub
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLoadOutcome {
    pub config: Config,
    pub warnings: Vec<ConfigWarning>,
}

impl ConfigLoadOutcome {
    pub fn log_warnings(&self) {
        for warning in &self.warnings {
            warn!("{warning}");
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigWarning {
    AliasUsed { alias: String, canonical: String },
    DeprecatedKey { key: String, note: String },
    EnvOverride { key: String },
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigWarning::AliasUsed { alias, canonical } => {
                write!(
                    f,
                    "config key `{alias}` is deprecated; mapped to `{canonical}`"
                )
            }
            ConfigWarning::DeprecatedKey { key, note } => {
                write!(f, "config `{key}` is deprecated: {note}")
            }
            ConfigWarning::EnvOverride { key } => {
                write!(f, "environment override applied for `{key}`")
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("failed to parse config {0}: {1}")]
    Parse(PathBuf, String),
    #[error("config version {0} is not supported; expected 1")]
    UnsupportedVersion(u32),
    #[error("invalid environment override `{0}`: {1}")]
    InvalidEnv(String, String),
}

fn default_version() -> u32 {
    1
}

fn default_kb_root() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".runloop")
        .join("kb")
}

fn default_events_db() -> PathBuf {
    PathBuf::from("events.sqlite")
}

fn default_view_db() -> PathBuf {
    PathBuf::from("views.sqlite")
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_router_opening() -> String {
    "compose_email".to_string()
}

fn default_sampling_ratio() -> f32 {
    0.1
}

fn default_config_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".runloop")
        .join("config.yaml")
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("RUNLOOP_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
}

fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
}

fn apply_aliases(root: &mut Value, warnings: &mut Vec<ConfigWarning>) {
    if let Some(alias_value) = take_path(root, &["kb", "ledger"]) {
        if let Some(path) = alias_value.as_str() {
            warnings.push(ConfigWarning::AliasUsed {
                alias: "kb.ledger".into(),
                canonical: "kb.events_db".into(),
            });
            let canonical_path = PathBuf::from(path);
            if let Some(parent) = canonical_path.parent() {
                set_path(
                    root,
                    &["kb", "root_dir"],
                    Value::String(parent.to_string_lossy().into()),
                );
            }
            set_path(
                root,
                &["kb", "events_db"],
                Value::String(
                    canonical_path
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| canonical_path.to_string_lossy().into()),
                ),
            );
        }
    }

    if let Some(alias_value) = take_path(root, &["kb", "materialized"]) {
        if let Some(path) = alias_value.as_str() {
            warnings.push(ConfigWarning::AliasUsed {
                alias: "kb.materialized".into(),
                canonical: "kb.view_db".into(),
            });
            let canonical_path = PathBuf::from(path);
            if root_path_missing(root, &["kb", "root_dir"]) {
                if let Some(parent) = canonical_path.parent() {
                    set_path(
                        root,
                        &["kb", "root_dir"],
                        Value::String(parent.to_string_lossy().into()),
                    );
                }
            }
            set_path(
                root,
                &["kb", "view_db"],
                Value::String(
                    canonical_path
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| canonical_path.to_string_lossy().into()),
                ),
            );
        }
    }

    if let Some(alias_value) = take_path(root, &["security", "secrets_backend"]) {
        if let Some(provider) = alias_value.as_str() {
            warnings.push(ConfigWarning::AliasUsed {
                alias: "security.secrets_backend".into(),
                canonical: "security.secrets.provider".into(),
            });
            set_path(
                root,
                &["security", "secrets", "provider"],
                Value::String(provider.to_string()),
            );
        }
    }

    if let Some(alias_value) = take_path(root, &["observability", "logs_format"]) {
        if let Some(format) = alias_value.as_str() {
            warnings.push(ConfigWarning::AliasUsed {
                alias: "observability.logs_format".into(),
                canonical: "logging.format".into(),
            });
            set_path(
                root,
                &["logging", "format"],
                Value::String(format.to_string()),
            );
        }
    }
}

fn apply_env_overrides(
    root: &mut Value,
    prefix: &str,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<(), ConfigError> {
    for (key, value) in env::vars() {
        if !key.starts_with(prefix) {
            continue;
        }
        let tail = &key[prefix.len()..];
        if tail.is_empty() {
            return Err(ConfigError::InvalidEnv(
                key.clone(),
                "missing key segments".into(),
            ));
        }
        let segments: Vec<String> = tail
            .split("__")
            .map(|segment| segment.to_ascii_lowercase().replace('-', "_"))
            .collect();
        if segments.iter().any(|s| s.is_empty()) {
            return Err(ConfigError::InvalidEnv(
                key.clone(),
                "empty key segment".into(),
            ));
        }
        let yaml_value: serde_yaml::Value = match serde_yaml::from_str(&value) {
            Ok(v) => v,
            Err(_) => serde_yaml::Value::String(value.clone()),
        };
        let json_value = serde_json::to_value(yaml_value)
            .map_err(|err| ConfigError::InvalidEnv(key.clone(), err.to_string()))?;
        set_path_owned(root, &segments, json_value);
        warnings.push(ConfigWarning::EnvOverride { key: key.clone() });
    }
    Ok(())
}

fn set_path(root: &mut Value, path: &[&str], value: Value) {
    let mut current = root;
    for segment in &path[..path.len().saturating_sub(1)] {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let map = current.as_object_mut().unwrap();
        current = map
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(last) = path.last() {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        current
            .as_object_mut()
            .unwrap()
            .insert((*last).to_string(), value);
    }
}

fn set_path_owned(root: &mut Value, path: &[String], value: Value) {
    let mut current = root;
    for segment in &path[..path.len().saturating_sub(1)] {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let map = current.as_object_mut().unwrap();
        current = map
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(last) = path.last() {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        current.as_object_mut().unwrap().insert(last.clone(), value);
    }
}

fn take_path(root: &mut Value, path: &[&str]) -> Option<Value> {
    if path.is_empty() {
        return None;
    }
    let mut current = root;
    for segment in &path[..path.len() - 1] {
        if let Value::Object(map) = current {
            if let Some(child) = map.get_mut(*segment) {
                current = child;
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    if let Value::Object(map) = current {
        map.remove(*path.last().unwrap())
    } else {
        None
    }
}

fn root_path_missing(root: &Value, path: &[&str]) -> bool {
    let mut current = root;
    for segment in path {
        match current {
            Value::Object(map) => match map.get(*segment) {
                Some(child) => current = child,
                None => return true,
            },
            _ => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs::File, io::Write};
    use tempfile::TempDir;

    #[test]
    fn load_defaults_when_missing() {
        let loader = ConfigLoader::default();
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("config.yaml");
        let outcome = loader.load(Some(path.clone())).unwrap();
        assert_eq!(outcome.config.version, 1);
        assert!(
            !outcome
                .config
                .kb
                .events_db_path()
                .to_string_lossy()
                .is_empty()
        );
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn honours_aliases_and_env() {
        let loader = ConfigLoader::default();
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("config.yaml");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "version: 1\nkb:\n  ledger: {}\nsecurity:\n  secrets_backend: os-keyring\n",
            temp.path().join("events.sqlite").display()
        )
        .unwrap();

        unsafe {
            env::set_var("RUNLOOP__LOGGING__FORMAT", "\"json\"");
        }
        let outcome = loader.load(Some(path.clone())).unwrap();
        unsafe {
            env::remove_var("RUNLOOP__LOGGING__FORMAT");
        }

        assert_eq!(
            outcome.config.security.secrets.provider,
            SecretsProvider::OsKeyring
        );
        assert_eq!(outcome.config.logging.format, LoggingFormat::Json);
        assert!(
            outcome.warnings.iter().any(
                |w| matches!(w, ConfigWarning::AliasUsed { alias, .. } if alias == "kb.ledger")
            )
        );
        assert!(outcome.warnings.iter().any(
            |w| matches!(w, ConfigWarning::EnvOverride { key } if key == "RUNLOOP__LOGGING__FORMAT")
        ));
    }
}
