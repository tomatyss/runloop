use crate::Error;
use dirs::{config_dir, home_dir};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Reverse;
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
    pub observability: ObservabilityConfig,
    #[serde(default)]
    pub openings: SearchDirsConfig,
    #[serde(default)]
    pub agents: SearchDirsConfig,
    #[serde(default)]
    pub bus: BusConfig,
}

/// Ordered list of configuration layers applied to build the final config.
#[derive(Clone, Debug, Serialize)]
pub struct ConfigLayer {
    pub source: ConfigSource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<ConfigOverride>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConfigSource {
    Defaults,
    File {
        path: PathBuf,
        exists: bool,
    },
    Env {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<String>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigOverride {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<Value>,
    pub new_value: Value,
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
            observability: ObservabilityConfig::default(),
            openings: SearchDirsConfig {
                search_dirs: vec![
                    "~/.runloop/openings".into(),
                    "./examples/openings".into(),
                    "/etc/runloop/openings".into(),
                ],
            },
            agents: SearchDirsConfig {
                search_dirs: vec![
                    "~/.runloop/agents".into(),
                    "./agents".into(),
                    "/usr/lib/runloop/agents".into(),
                ],
            },
            bus: BusConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from defaults + files + environment overrides.
    pub fn load() -> Result<Self, Error> {
        let env_pairs: Vec<(String, String)> = env::vars().collect();
        Self::load_from_sources(config_candidate_paths(), env_pairs)
    }

    /// Load configuration along with provenance layers.
    pub fn load_with_layers() -> Result<(Self, Vec<ConfigLayer>), Error> {
        let env_pairs: Vec<(String, String)> = env::vars().collect();
        Self::load_from_sources_with_layers(config_candidate_paths(), env_pairs)
    }

    fn load_from_sources(
        paths: Vec<PathBuf>,
        env_pairs: Vec<(String, String)>,
    ) -> Result<Self, Error> {
        let (config, _) = Self::load_from_sources_with_layers(paths, env_pairs)?;
        Ok(config)
    }

    fn load_from_sources_with_layers(
        paths: Vec<PathBuf>,
        env_pairs: Vec<(String, String)>,
    ) -> Result<(Self, Vec<ConfigLayer>), Error> {
        let mut base = serde_json::to_value(Config::default()).expect("serialize default config");
        let mut layers = Vec::new();
        layers.push(ConfigLayer {
            source: ConfigSource::Defaults,
            overrides: Vec::new(),
        });

        for path in paths {
            let mut layer = ConfigLayer {
                source: ConfigSource::File {
                    path: path.clone(),
                    exists: path.exists(),
                },
                overrides: Vec::new(),
            };
            match load_yaml_value(&path) {
                Ok(Some(mut value)) => {
                    normalize_router_aliases(&mut value);
                    normalize_kb_aliases(&mut value);
                    warn_unknown_keys(&value, Path::new(""));
                    let before = base.clone();
                    merge_value(&mut base, value);
                    layer.overrides = diff_values(&before, &base);
                }
                Ok(None) => {}
                Err(err) => return Err(err),
            }
            layers.push(layer);
        }

        if let Some(obj) = base.as_object_mut() {
            let before = serde_json::Value::Object(obj.clone());
            let mut keys = Vec::new();
            apply_env_overrides(obj, env_pairs, Some(&mut keys));
            let after = serde_json::Value::Object(obj.clone());
            layers.push(ConfigLayer {
                source: ConfigSource::Env { keys },
                overrides: diff_values(&before, &after),
            });
        } else {
            layers.push(ConfigLayer {
                source: ConfigSource::Env { keys: Vec::new() },
                overrides: Vec::new(),
            });
        }

        normalize_router_aliases(&mut base);
        normalize_kb_aliases(&mut base);

        let mut config: Config = serde_json::from_value(base)
            .map_err(|err| Error::Config(format!("config deserialization failed: {err}")))?;
        config.expand_paths();
        config.validate()?;
        Ok((config, layers))
    }

    /// Validate required invariants.
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(Error::Config(format!(
                "unsupported config version {}; expected 1",
                self.version
            )));
        }
        if self.runtime.agent_container != "wasm32-wasip1" {
            return Err(Error::Config(format!(
                "runtime.agent_container must be \"wasm32-wasip1\" for MVP (found {})",
                self.runtime.agent_container
            )));
        }
        if self.security.confirm_external_actions && self.router.denylist.is_empty() {
            warn!(
                "router denylist empty while confirmations required; consider keeping protective entries"
            );
        }
        if self.observability.metrics_interval_ms < 100
            || self.observability.metrics_interval_ms > 60_000
        {
            return Err(Error::Config(format!(
                "observability.metrics_interval_ms must be between 100 and 60000 (found {})",
                self.observability.metrics_interval_ms
            )));
        }
        warn_missing_search_dirs("openings.search_dirs", &self.openings.search_dirs);
        warn_missing_search_dirs("agents.search_dirs", &self.agents.search_dirs);

        if self
            .runtime
            .socket_path
            .as_ref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(Error::Config(
                "runtime.socket_path cannot be empty when specified".into(),
            ));
        }
        if self.runtime.sockets_dir.trim().is_empty() && self.runtime.socket_path.is_none() {
            return Err(Error::Config(
                "runtime.sockets_dir cannot be empty when runtime.socket_path is unset".into(),
            ));
        }

        for kind in &self.bus.auth.publishers.action_decision.allowed_kinds {
            let normalized = kind.trim();
            if normalized.is_empty() {
                return Err(Error::Config(
                    "empty publisher kind entry in bus.auth.publishers.action_decision.allowed_kinds"
                        .into(),
                ));
            }
            match normalized.to_ascii_lowercase().as_str() {
                "ui" | "tui" | "agent" => {}
                other => {
                    return Err(Error::Config(format!(
                        "unknown publisher kind '{other}' in bus.auth.publishers.action_decision.allowed_kinds"
                    )));
                }
            }
        }

        Ok(())
    }

    fn expand_paths(&mut self) {
        self.runtime.workdir = expand_path(&self.runtime.workdir);
        self.runtime.sockets_dir = expand_path(&self.runtime.sockets_dir);
        // Deprecation rewrite (MVP): prefer '~/.runloop/sock' over legacy '~/.runloop/run'.
        {
            let p = std::path::Path::new(&self.runtime.sockets_dir);
            let last = p.file_name().and_then(|s| s.to_str()) == Some("run");
            let parent_is_runloop = p
                .parent()
                .and_then(|pp| pp.file_name().and_then(|s| s.to_str()))
                == Some(".runloop");
            if last && parent_is_runloop {
                warn!(
                    "runtime.sockets_dir '~/.runloop/run' is deprecated; using '~/.runloop/sock' instead"
                );
                if let Some(parent) = p.parent() {
                    self.runtime.sockets_dir = parent.join("sock").to_string_lossy().into_owned();
                }
            }
        }
        if let Some(path) = &self.runtime.socket_path {
            self.runtime.socket_path = Some(expand_path(path));
        }

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
    #[serde(default)]
    pub socket_path: Option<String>,
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
            socket_path: None,
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
    #[serde(
        default = "default_route_vec",
        alias = "routing",
        deserialize_with = "deserialize_route"
    )]
    pub route: Vec<ModelRoute>,
    #[serde(default)]
    pub cache: BrokerCacheConfig,
    #[serde(default)]
    pub budgets: ModelBudgets,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            route: default_route_vec(),
            cache: BrokerCacheConfig::default(),
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
    pub secret_id: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub schema: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelRoute {
    pub pattern: String,
    pub provider: String,
    #[serde(default)]
    pub target_model: Option<String>,
}

fn deserialize_route<'de, D>(deserializer: D) -> Result<Vec<ModelRoute>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RouteRepr {
        List(Vec<ModelRoute>),
        Map(BTreeMap<String, String>),
    }

    let maybe = Option::<RouteRepr>::deserialize(deserializer)?;
    let routes = match maybe {
        Some(RouteRepr::List(list)) => list,
        Some(RouteRepr::Map(map)) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| {
                route_pattern_key(left).cmp(&route_pattern_key(right))
            });
            entries
                .into_iter()
                .map(|(pattern, provider)| ModelRoute {
                    pattern,
                    provider,
                    target_model: None,
                })
                .collect()
        }
        None => default_route_vec(),
    };
    if routes.is_empty() {
        Ok(default_route_vec())
    } else {
        Ok(routes)
    }
}

fn route_pattern_key(pattern: &str) -> (bool, bool, Reverse<usize>, &str) {
    let is_catch_all = pattern == "*";
    let has_wildcard = pattern.contains('*');
    let specificity = pattern.chars().filter(|c| *c != '*').count();
    (is_catch_all, has_wildcard, Reverse(specificity), pattern)
}

fn default_route_vec() -> Vec<ModelRoute> {
    vec![ModelRoute {
        pattern: "*".into(),
        provider: "local".into(),
        target_model: None,
    }]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrokerCacheConfig {
    #[serde(default = "default_broker_cache_ttl_ms")]
    pub ttl_ms: u64,
    #[serde(default = "default_broker_cache_capacity")]
    pub capacity: usize,
}

impl Default for BrokerCacheConfig {
    fn default() -> Self {
        Self {
            ttl_ms: default_broker_cache_ttl_ms(),
            capacity: default_broker_cache_capacity(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Local,
    Http,
    #[serde(rename = "http_gemini")]
    HttpGemini,
    #[serde(rename = "http_openai_chat")]
    HttpOpenAiChat,
    #[serde(rename = "http_anthropic")]
    HttpAnthropic,
    #[serde(rename = "http_ollama")]
    HttpOllama,
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
    pub fastpath_shell: bool,
    #[serde(default = "default_router_opening")]
    pub default_opening: String,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default = "default_router_denylist")]
    pub denylist: Vec<String>,
    #[serde(default)]
    pub known_commands: Vec<String>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            fastpath_shell: true,
            default_opening: default_router_opening(),
            allowlist: Vec::new(),
            denylist: default_router_denylist(),
            known_commands: Vec::new(),
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    #[serde(default)]
    pub traces: TracesConfig,
    #[serde(default = "default_metrics_interval_ms")]
    pub metrics_interval_ms: u32,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            traces: TracesConfig::default(),
            metrics_interval_ms: default_metrics_interval_ms(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TracesConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    #[serde(default = "default_trace_sampling")]
    pub sampling: String,
}

impl Default for TracesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: None,
            sampling: default_trace_sampling(),
        }
    }
}

fn default_metrics_interval_ms() -> u32 {
    1_000
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchDirsConfig {
    #[serde(default)]
    pub search_dirs: Vec<String>,
}

// Bus auth configuration
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BusConfig {
    #[serde(default)]
    pub auth: BusAuthConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BusAuthConfig {
    #[serde(default)]
    pub publishers: BusPublishersConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BusPublishersConfig {
    #[serde(default = "default_allowed_action_decision_kinds")]
    pub action_decision: BusPublisherRule,
}

impl Default for BusPublishersConfig {
    fn default() -> Self {
        Self {
            action_decision: default_allowed_action_decision_kinds(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BusPublisherRule {
    #[serde(default = "default_action_decision_kinds")]
    pub allowed_kinds: Vec<String>,
}

fn default_allowed_action_decision_kinds() -> BusPublisherRule {
    BusPublisherRule {
        allowed_kinds: default_action_decision_kinds(),
    }
}

fn default_action_decision_kinds() -> Vec<String> {
    vec!["ui".into(), "tui".into()]
}

// Defaults helpers
fn default_runtime_base() -> String {
    "debian".into()
}
fn default_agent_container() -> String {
    "wasm32-wasip1".into()
}
fn default_runtime_workdir() -> String {
    "~/.runloop".into()
}
fn default_runtime_sockets_dir() -> String {
    // Prefer XDG_RUNTIME_DIR for per-user runtime sockets; fallback to ~/.runloop/sock
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
    default_runtime_sockets_dir_with(xdg.as_deref())
}

fn default_runtime_sockets_dir_with(xdg: Option<&str>) -> String {
    if let Some(xdg) = xdg
        && !xdg.trim().is_empty()
    {
        return format!("{}/runloop", xdg.trim_end_matches('/'));
    }
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
fn default_broker_cache_ttl_ms() -> u64 {
    600_000
}
fn default_broker_cache_capacity() -> usize {
    1_024
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
    "auto".into()
}
fn default_trace_sampling() -> String {
    "parent".into()
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
        paths.push(PathBuf::from("/etc/runloop/config.yaml"));
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

fn normalize_router_aliases(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(router_map)) = map.get_mut("router") {
                if router_map.contains_key("allow_shell_fastpath") {
                    warn!(
                        "configuration key 'router.allow_shell_fastpath' is deprecated; use 'router.fastpath_shell'"
                    );
                    if !router_map.contains_key("fastpath_shell") {
                        if let Some(alias_value) = router_map.remove("allow_shell_fastpath") {
                            router_map.insert("fastpath_shell".into(), alias_value);
                        }
                    } else {
                        router_map.remove("allow_shell_fastpath");
                    }
                }
                if router_map.contains_key("shell_fastpath") {
                    warn!(
                        "configuration key 'router.shell_fastpath' is deprecated; use 'router.fastpath_shell'"
                    );
                    if !router_map.contains_key("fastpath_shell") {
                        if let Some(alias_value) = router_map.remove("shell_fastpath") {
                            router_map.insert("fastpath_shell".into(), alias_value);
                        }
                    } else {
                        router_map.remove("shell_fastpath");
                    }
                }
            }
            for value in map.values_mut() {
                normalize_router_aliases(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_router_aliases(item);
            }
        }
        _ => {}
    }
}

fn normalize_kb_aliases(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(kb)) = map.get_mut("kb") {
                if let Some(ledger) = kb.remove("ledger") {
                    warn!("configuration key 'kb.ledger' is deprecated; use 'kb.events_db'");
                    kb.entry("events_db").or_insert(ledger);
                }
                if let Some(materialized) = kb.remove("materialized") {
                    warn!("configuration key 'kb.materialized' is deprecated; use 'kb.view_db'");
                    kb.entry("view_db").or_insert(materialized);
                }
            }
            for value in map.values_mut() {
                normalize_kb_aliases(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_kb_aliases(item);
            }
        }
        _ => {}
    }
}

fn apply_env_overrides(
    root: &mut serde_json::Map<String, Value>,
    env_pairs: impl IntoIterator<Item = (String, String)>,
    mut recorded_keys: Option<&mut Vec<String>>,
) {
    for (key, value) in env_pairs {
        if let Some(stripped) = key.strip_prefix("RUNLOOP__") {
            let mut path = stripped
                .split("__")
                .map(|segment| segment.to_ascii_lowercase())
                .collect::<Vec<_>>();
            if path.as_slice() == ["router", "shell_fastpath"] {
                warn!(
                    "environment key 'RUNLOOP__ROUTER__SHELL_FASTPATH' is deprecated; use 'RUNLOOP__ROUTER__FASTPATH_SHELL'"
                );
                if let Some(key) = path.get_mut(1) {
                    *key = "fastpath_shell".into();
                }
            }
            if path.as_slice() == ["router", "allow_shell_fastpath"] {
                warn!(
                    "environment key 'RUNLOOP__ROUTER__ALLOW_SHELL_FASTPATH' is deprecated; use 'RUNLOOP__ROUTER__FASTPATH_SHELL'"
                );
                if let Some(key) = path.get_mut(1) {
                    *key = "fastpath_shell".into();
                }
            }
            if let Some(keys) = recorded_keys.as_mut() {
                (**keys).push(path.join("."));
            }
            set_path(root, &path, parse_env_value(&value));
        } else if let Some((path, parsed)) = parse_alias(&key, &value) {
            if let Some(keys) = recorded_keys.as_mut() {
                (**keys).push(path.join("."));
            }
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
    let aliases: [(&str, &[&str]); 8] = [
        ("RUNLOOP_LOG_LEVEL", &["logging", "level"]),
        (
            "RUNLOOP_RUNTIME_AGENT_CONTAINER",
            &["runtime", "agent_container"],
        ),
        (
            "RUNLOOP_ROUTER_DEFAULT_OPENING",
            &["router", "default_opening"],
        ),
        (
            "RUNLOOP_ROUTER_SHELL_FASTPATH",
            &["router", "fastpath_shell"],
        ),
        (
            "RUNLOOP_ROUTER_ALLOW_SHELL_FASTPATH",
            &["router", "fastpath_shell"],
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

fn diff_values(before: &Value, after: &Value) -> Vec<ConfigOverride> {
    let mut overrides = Vec::new();
    diff_values_recursive("", before, after, &mut overrides);
    overrides
}

fn diff_values_recursive(
    prefix: &str,
    before: &Value,
    after: &Value,
    overrides: &mut Vec<ConfigOverride>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(before_map), Value::Object(after_map)) => {
            let mut keys = BTreeSet::new();
            keys.extend(before_map.keys().cloned());
            keys.extend(after_map.keys().cloned());
            for key in keys {
                let next_prefix = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                let before_child = before_map.get(&key).unwrap_or(&Value::Null);
                let after_child = after_map.get(&key).unwrap_or(&Value::Null);
                diff_values_recursive(&next_prefix, before_child, after_child, overrides);
            }
        }
        _ => {
            overrides.push(ConfigOverride {
                key: prefix.to_string(),
                previous: value_to_option(before.clone()),
                new_value: after.clone(),
            });
        }
    }
}

fn value_to_option(value: Value) -> Option<Value> {
    if value.is_null() { None } else { Some(value) }
}

fn expand_path(path: &str) -> String {
    if let Some((home, stripped)) = home_dir().zip(path.strip_prefix("~/")) {
        return home.join(stripped).display().to_string();
    }
    if let Some(value) = path.strip_prefix("env:").and_then(|key| env::var(key).ok()) {
        return value;
    }
    shellexpand::full(path)
        .map(|cow| cow.to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn warn_missing_search_dirs(label: &str, dirs: &[String]) {
    for dir in dirs {
        let expanded = expand_path(dir);
        let path = Path::new(&expanded);
        if !path.exists() {
            warn!("configured {} entry '{}' does not exist", label, expanded);
        }
    }
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
            "version",
            "runtime",
            "models",
            "kb",
            "security",
            "router",
            "ui",
            "logging",
            "observability",
            "openings",
            "agents",
            "bus",
        ]),
        "runtime" => BTreeSet::from([
            "base",
            "agent_container",
            "socket_path",
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
        "observability" => BTreeSet::from(["traces", "metrics_interval_ms"]),
        "observability/traces" => BTreeSet::from(["enabled", "otlp_endpoint", "sampling"]),
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
        "router" => BTreeSet::from([
            "fastpath_shell",
            "default_opening",
            "allowlist",
            "denylist",
            "known_commands",
        ]),
        "ui" => BTreeSet::from(["theme", "confirm_prompts"]),
        "logging" => BTreeSet::from(["level", "file", "format"]),
        "openings" | "agents" => BTreeSet::from(["search_dirs"]),
        "bus" => BTreeSet::from(["auth"]),
        "bus/auth" => BTreeSet::from(["publishers"]),
        "bus/auth/publishers" => BTreeSet::from(["action_decision"]),
        "bus/auth/publishers/action_decision" => BTreeSet::from(["allowed_kinds"]),
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
    fn metrics_interval_bounds() {
        let mut config = Config::default();
        config.observability.metrics_interval_ms = 99;
        assert!(config.validate().is_err(), "should reject too-low interval");
        config.observability.metrics_interval_ms = 100;
        config.validate().expect("100ms should pass");
        config.observability.metrics_interval_ms = 60_000;
        config.validate().expect("60s should pass");
        config.observability.metrics_interval_ms = 60_001;
        assert!(
            config.validate().is_err(),
            "should reject too-high interval"
        );
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
  agent_container: "wasm32-wasip1"
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

    #[test]
    fn kb_aliases_normalized() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("config.yaml");
        let mut file = std::fs::File::create(&config_path).expect("create config file");
        write!(
            file,
            r#"
version: 1
kb:
  ledger: "events.sqlite"
  materialized: "pog.sqlite"
"#
        )
        .expect("write config");
        let env_pairs: Vec<(String, String)> = vec![];
        let config = Config::load_from_sources(vec![config_path], env_pairs).expect("config load");
        assert_eq!(config.kb.events_db, "events.sqlite");
        assert_eq!(config.kb.view_db, "pog.sqlite");
    }

    #[test]
    fn sockets_dir_prefers_xdg_runtime_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let xdg_dir = temp.path().join("xdg");
        std::fs::create_dir_all(&xdg_dir).expect("mkdir xdg");
        let xdg_str = xdg_dir.to_string_lossy().to_string();
        let resolved = super::default_runtime_sockets_dir_with(Some(&xdg_str));
        let expected_prefix = format!(
            "{}/runloop",
            xdg_dir.to_string_lossy().trim_end_matches('/')
        );
        assert!(
            resolved.starts_with(&expected_prefix),
            "sockets_dir={} expected prefix {}",
            resolved,
            expected_prefix
        );
    }

    #[test]
    fn deprecation_rewrite_for_sock_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("config.yaml");
        let mut file = std::fs::File::create(&config_path).expect("create config file");
        write!(
            file,
            r#"
version: 1
runtime:
  agent_container: "wasm32-wasip1"
  sockets_dir: "~/.runloop/run"
"#
        )
        .expect("write config");
        let config = Config::load_from_sources(vec![config_path], Vec::new()).expect("load");
        assert!(
            config.runtime.sockets_dir.ends_with("/.runloop/sock"),
            "sockets_dir rewrite failed: {}",
            config.runtime.sockets_dir
        );
    }

    #[test]
    fn broker_route_map_orders_catch_all_last() {
        let json = r#"{
            "version": 1,
            "models": {
                "broker": {
                    "route": {
                        "gpt4*": "openai",
                        "*": "local"
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(json).expect("config parse");
        let routes = &config.models.broker.route;
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].pattern, "gpt4*");
        assert_eq!(routes[1].pattern, "*");
    }

    #[test]
    fn legacy_routing_key_deserializes() {
        let json = r#"{
            "version": 1,
            "models": {
                "broker": {
                    "routing": {
                        "*": "local"
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(json).expect("config parse");
        assert_eq!(config.models.broker.route.len(), 1);
        assert_eq!(config.models.broker.route[0].provider, "local");
    }

    #[test]
    fn missing_route_defaults_to_local() {
        let json = r#"{
            "version": 1,
            "models": {
                "broker": {
                    "providers": []
                }
            }
        }"#;
        let config: Config = serde_json::from_str(json).expect("config parse");
        assert_eq!(config.models.broker.route.len(), 1);
        assert_eq!(config.models.broker.route[0].provider, "local");
        assert_eq!(config.models.broker.route[0].pattern, "*");
    }

    #[test]
    fn validate_rejects_empty_socket_path() {
        let mut config = Config::default();
        config.runtime.socket_path = Some("   ".into());
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("runtime.socket_path cannot be empty")
        );
    }

    #[test]
    fn validate_rejects_empty_sockets_dir_when_no_path() {
        let mut config = Config::default();
        config.runtime.socket_path = None;
        config.runtime.sockets_dir = "   ".into();
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("runtime.sockets_dir cannot be empty")
        );
    }

    #[test]
    fn validate_rejects_invalid_action_decision_kind() {
        let mut config = Config::default();
        config
            .bus
            .auth
            .publishers
            .action_decision
            .allowed_kinds
            .push("invalid_kind".into());
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown publisher kind 'invalid_kind'")
        );
    }

    #[test]
    fn validate_rejects_empty_action_decision_kind() {
        let mut config = Config::default();
        config
            .bus
            .auth
            .publishers
            .action_decision
            .allowed_kinds
            .push("   ".into());
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty publisher kind entry"));
    }
}
