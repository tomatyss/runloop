//! Model broker facade for completion providers with caching and budget enforcement.

use async_trait::async_trait;
use blake3::Hasher;
use lru::LruCache;
use reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use runloop_core::config::{BrokerConfig, ModelBudgets, ModelProvider, ProviderKind};
use runloop_core::ids::TraceId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time;

const METRIC_CALLS: &str = "runloop_broker_calls_total";
const METRIC_CACHE_HITS: &str = "runloop_broker_cache_hits_total";
const METRIC_ERRORS: &str = "runloop_broker_errors_total";

/// Resolve opaque secret identifiers into bearer/API keys.
pub trait SecretResolver: Send + Sync {
    /// Resolve the provided secret identifier.
    fn resolve(&self, secret_id: &str) -> Option<String>;
}

/// Result alias for broker completions.
pub type ModelResult = Result<ModelOutput, BrokerError>;

/// Execution request for the broker.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelRequest {
    #[serde(default)]
    pub trace_id: TraceId,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub params: Option<ModelParams>,
    #[serde(default)]
    pub budget_tokens: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
    #[serde(default)]
    pub cache_ttl_ms: Option<u32>,
    #[serde(default)]
    pub cache_key: Option<String>,
    #[serde(default)]
    pub stream: bool,
}

/// Optional model parameters forwarded to providers.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ModelParams {
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
}

/// Normalised broker response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelOutput {
    pub text: String,
    #[serde(default)]
    pub tokens_in: Option<u32>,
    #[serde(default)]
    pub tokens_out: Option<u32>,
    pub cached: bool,
    pub provider: String,
    pub provider_model: String,
    pub latency_ms: u32,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

impl ModelOutput {
    /// Returns the metadata payload (excluding response text) for optional hostcall side-channels.
    #[must_use]
    pub fn meta(&self) -> ModelOutputMeta {
        ModelOutputMeta {
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            cached: self.cached,
            provider: self.provider.clone(),
            provider_model: self.provider_model.clone(),
            latency_ms: self.latency_ms,
            finish_reason: self.finish_reason.clone(),
        }
    }
}

/// Metadata returned alongside the model output text.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelOutputMeta {
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
    pub cached: bool,
    pub provider: String,
    pub provider_model: String,
    pub latency_ms: u32,
    pub finish_reason: Option<String>,
}

/// Errors surfaced by the broker to callers.
#[derive(thiserror::Error, Serialize, Deserialize, Clone, Debug)]
pub enum BrokerError {
    #[error("streaming unsupported")]
    StreamingUnsupported,
    #[error("budget exceeded (limit={limit}, used={used})")]
    BudgetExceeded { limit: u32, used: u32 },
    #[error("timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u32 },
    #[error("provider fault: {code} {message}")]
    ProviderFault { code: String, message: String },
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },
    #[error("cancelled")]
    Cancelled,
    #[error("output too large (cap={cap})")]
    OutputTooLarge { cap: usize },
}

/// Errors that occur while constructing the broker.
#[derive(thiserror::Error, Debug)]
pub enum BrokerInitError {
    #[error("duplicate provider id '{0}'")]
    DuplicateProvider(String),
    #[error("http provider '{id}' missing base_url")]
    MissingBaseUrl { id: String },
    #[error("unsupported provider schema '{schema}' for '{id}'")]
    UnsupportedSchema { id: String, schema: String },
    #[error("invalid header name '{header}' for provider '{id}'")]
    InvalidHeaderName {
        id: String,
        header: String,
        #[source]
        source: reqwest::header::InvalidHeaderName,
    },
    #[error("invalid header value for '{header}' on provider '{id}'")]
    InvalidHeaderValue {
        id: String,
        header: String,
        #[source]
        source: reqwest::header::InvalidHeaderValue,
    },
    #[error("http client error for provider '{id}': {source}")]
    HttpClient {
        id: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("route '{pattern}' references unknown provider '{provider}'")]
    UnknownProvider { pattern: String, provider: String },
}

/// Async broker facade.
#[derive(Clone)]
pub struct Broker {
    inner: Arc<BrokerInner>,
}

struct BrokerInner {
    providers: HashMap<String, Arc<dyn Provider>>,
    routes: Vec<RouteRule>,
    cache: Option<Arc<BrokerCache>>,
    budgets: BudgetLimits,
}

struct BrokerCache {
    default_ttl: Duration,
    store: Mutex<LruCache<String, CacheEntry>>,
}

#[derive(Clone)]
struct CacheEntry {
    output: ModelOutput,
    expires_at: Instant,
}

struct BudgetLimits {
    default_tokens: Option<u32>,
    per_request_cap: Option<u32>,
    _hard_cap_usd: Option<f32>,
}

impl From<ModelBudgets> for BudgetLimits {
    fn from(value: ModelBudgets) -> Self {
        Self {
            default_tokens: Some(value.default_tokens).filter(|v| *v > 0),
            per_request_cap: Some(value.per_request_tokens_cap).filter(|v| *v > 0),
            _hard_cap_usd: value.hard_cap_usd,
        }
    }
}

impl BudgetLimits {
    fn effective_limit(&self, request: Option<u32>) -> Option<u32> {
        let mut limit = request.or(self.default_tokens);
        if let (Some(current), Some(cap)) = (limit, self.per_request_cap) {
            limit = Some(current.min(cap));
        }
        limit.filter(|v| *v > 0)
    }

    fn enforce(&self, request: &ModelRequest, total_tokens: u32) -> Result<(), BrokerError> {
        if let Some(limit) = self.effective_limit(request.budget_tokens)
            && total_tokens > limit
        {
            return Err(BrokerError::BudgetExceeded {
                limit,
                used: total_tokens,
            });
        }
        Ok(())
    }
}

impl Broker {
    /// Build a broker from configuration and a secret resolver.
    pub fn new(
        config: BrokerConfig,
        secrets: Arc<dyn SecretResolver>,
    ) -> Result<Self, BrokerInitError> {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();

        for provider_cfg in config.providers {
            if providers.contains_key(&provider_cfg.id) {
                return Err(BrokerInitError::DuplicateProvider(provider_cfg.id));
            }
            let provider = build_provider(provider_cfg, Arc::clone(&secrets))?;
            providers.insert(provider.id().to_string(), provider);
        }

        if !providers.contains_key("null") {
            providers.insert("null".into(), Arc::new(NullProvider::new("null")));
        }
        if !providers.contains_key("local") {
            providers.insert("local".into(), Arc::new(NullProvider::new("local")));
        }

        let routes = if config.route.is_empty() {
            vec![RouteRule {
                pattern: "*".into(),
                provider_id: "null".into(),
                target_model: None,
            }]
        } else {
            config
                .route
                .into_iter()
                .map(|route| {
                    if !providers.contains_key(&route.provider) {
                        return Err(BrokerInitError::UnknownProvider {
                            pattern: route.pattern,
                            provider: route.provider,
                        });
                    }
                    Ok(RouteRule {
                        pattern: route.pattern,
                        provider_id: route.provider,
                        target_model: route.target_model,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        let cache = cache_from_config(config.cache);

        Ok(Self {
            inner: Arc::new(BrokerInner {
                providers,
                routes,
                cache,
                budgets: config.budgets.into(),
            }),
        })
    }

    /// Execute a model completion request asynchronously.
    pub async fn complete(&self, request: &ModelRequest) -> ModelResult {
        if request.stream {
            record_error(&BrokerError::StreamingUnsupported);
            return Err(BrokerError::StreamingUnsupported);
        }

        let inner = &self.inner;
        let route = match inner
            .routes
            .iter()
            .find(|route| route.matches(&request.model))
        {
            Some(route) => route.clone(),
            None => {
                let err = BrokerError::InvalidRequest {
                    reason: format!("no provider configured for model '{}'", request.model),
                };
                record_error(&err);
                return Err(err);
            }
        };

        let provider = match inner.providers.get(&route.provider_id) {
            Some(provider) => Arc::clone(provider),
            None => {
                let err = BrokerError::InvalidRequest {
                    reason: format!(
                        "routing for model '{}' references unknown provider '{}'",
                        request.model, route.provider_id
                    ),
                };
                record_error(&err);
                return Err(err);
            }
        };

        metrics::counter!(METRIC_CALLS, "provider" => provider.id().to_owned()).increment(1);

        let provider_model_hint = route
            .target_model
            .clone()
            .unwrap_or_else(|| request.model.clone());
        let cache_ttl = effective_ttl(request.cache_ttl_ms, inner.cache.as_ref());
        let cache_key = compose_cache_key(
            provider.id(),
            &provider_model_hint,
            request,
            request.cache_key.as_deref(),
        );

        if let Some(cache) = inner.cache.as_ref()
            && let Some(hit) = check_cache(cache, &cache_key).await
        {
            metrics::counter!(METRIC_CACHE_HITS, "provider" => provider.id().to_owned())
                .increment(1);
            let mut output = hit;
            let (tokens_in, tokens_out, total_tokens) = usage_profile(&output, &request.prompt);
            if let Err(err) = inner.budgets.enforce(request, total_tokens) {
                record_error(&err);
                return Err(err);
            }
            output.tokens_in = tokens_in;
            output.tokens_out = tokens_out;
            return Ok(output);
        }

        let timeout = request.timeout_ms.and_then(|ms| {
            if ms == 0 {
                None
            } else {
                Some(Duration::from_millis(ms as u64))
            }
        });

        let start = Instant::now();
        let completion_fut = provider.complete(request, &provider_model_hint, timeout);
        let completion = if let Some(limit) = timeout {
            match time::timeout(limit, completion_fut).await {
                Ok(result) => result,
                Err(_) => {
                    let err = BrokerError::Timeout {
                        timeout_ms: saturating_duration_millis(limit),
                    };
                    record_error(&err);
                    return Err(err);
                }
            }
        } else {
            completion_fut.await
        };

        let completion = match completion {
            Ok(value) => value,
            Err(err) => {
                record_error(&err);
                return Err(err);
            }
        };

        let ProviderCompletion {
            text,
            tokens_in: completion_tokens_in,
            tokens_out: completion_tokens_out,
            finish_reason,
            provider_model,
        } = completion;

        let latency_ms = saturating_duration_millis(start.elapsed());
        let resolved_model = if provider_model.is_empty() {
            provider_model_hint
        } else {
            provider_model
        };

        let mut output = ModelOutput {
            text,
            tokens_in: completion_tokens_in,
            tokens_out: completion_tokens_out,
            cached: false,
            provider: provider.id().to_string(),
            provider_model: resolved_model,
            latency_ms,
            finish_reason,
        };

        let (tokens_in, tokens_out, total_tokens) = usage_profile(&output, &request.prompt);
        if let Err(err) = inner.budgets.enforce(request, total_tokens) {
            record_error(&err);
            return Err(err);
        }
        output.tokens_in = tokens_in;
        output.tokens_out = tokens_out;

        if let (Some(ttl), Some(cache)) = (cache_ttl, inner.cache.as_ref()) {
            store_cache(cache, cache_key, &output, ttl).await;
        }

        Ok(output)
    }
}

impl Default for Broker {
    fn default() -> Self {
        Self::new(BrokerConfig::default(), Arc::new(NoopSecretResolver)).expect("default broker")
    }
}

#[derive(Clone)]
struct RouteRule {
    pattern: String,
    provider_id: String,
    target_model: Option<String>,
}

impl RouteRule {
    fn matches(&self, model: &str) -> bool {
        if self.pattern == "*" {
            return true;
        }
        if let Some(prefix) = self.pattern.strip_suffix('*') {
            return model.starts_with(prefix);
        }
        self.pattern == model
    }
}

fn cache_from_config(config: runloop_core::config::BrokerCacheConfig) -> Option<Arc<BrokerCache>> {
    if config.capacity == 0 || config.ttl_ms == 0 {
        return None;
    }
    let capacity =
        NonZeroUsize::new(config.capacity).unwrap_or_else(|| NonZeroUsize::new(1).unwrap());
    Some(Arc::new(BrokerCache {
        default_ttl: Duration::from_millis(config.ttl_ms),
        store: Mutex::new(LruCache::new(capacity)),
    }))
}

fn effective_ttl(override_ms: Option<u32>, cache: Option<&Arc<BrokerCache>>) -> Option<Duration> {
    match override_ms {
        Some(0) => None,
        Some(value) => Some(Duration::from_millis(value as u64)),
        None => cache
            .map(|cache| cache.default_ttl)
            .filter(|ttl| !ttl.is_zero()),
    }
}

async fn check_cache(cache: &Arc<BrokerCache>, key: &str) -> Option<ModelOutput> {
    let mut guard = cache.store.lock().await;
    let mut stale = false;
    if let Some(entry) = guard.get(key) {
        if entry.expires_at > Instant::now() {
            let mut output = entry.output.clone();
            output.cached = true;
            return Some(output);
        }
        stale = true;
    }
    if stale {
        guard.pop(key);
    }
    None
}

async fn store_cache(cache: &Arc<BrokerCache>, key: String, output: &ModelOutput, ttl: Duration) {
    let expires_at = Instant::now() + ttl;
    let mut stored = output.clone();
    stored.cached = false;
    let mut guard = cache.store.lock().await;
    guard.put(
        key,
        CacheEntry {
            output: stored,
            expires_at,
        },
    );
}

fn compose_cache_key(
    provider_id: &str,
    provider_model: &str,
    request: &ModelRequest,
    extra: Option<&str>,
) -> String {
    let mut hasher = Hasher::new();
    hasher.update(provider_id.as_bytes());
    hasher.update(provider_model.as_bytes());
    hasher.update(request.prompt.as_bytes());
    if let Some(params) = &request.params
        && let Ok(encoded) = serde_json::to_vec(params)
    {
        hasher.update(&encoded);
    }
    if let Some(key) = extra {
        hasher.update(key.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn estimate_tokens(text: &str) -> u32 {
    // Simple heuristic: 4 characters ≈ 1 token.
    text.len().div_ceil(4) as u32
}

fn usage_profile(output: &ModelOutput, prompt: &str) -> (Option<u32>, Option<u32>, u32) {
    let tokens_in = output.tokens_in.or_else(|| Some(estimate_tokens(prompt)));
    let tokens_out = output
        .tokens_out
        .or_else(|| Some(estimate_tokens(&output.text)));
    let total = tokens_in
        .unwrap_or(0)
        .saturating_add(tokens_out.unwrap_or(0));
    (tokens_in, tokens_out, total)
}

fn saturating_duration_millis(duration: Duration) -> u32 {
    duration.as_millis().min(u32::MAX as u128) as u32
}

fn record_error(err: &BrokerError) {
    metrics::counter!(METRIC_ERRORS, "kind" => metric_label(err)).increment(1);
}

fn metric_label(err: &BrokerError) -> &'static str {
    match err {
        BrokerError::StreamingUnsupported => "streaming_unsupported",
        BrokerError::BudgetExceeded { .. } => "budget_exceeded",
        BrokerError::Timeout { .. } => "timeout",
        BrokerError::ProviderFault { .. } => "provider_fault",
        BrokerError::InvalidRequest { .. } => "invalid_request",
        BrokerError::Cancelled => "cancelled",
        BrokerError::OutputTooLarge { .. } => "output_too_large",
    }
}

#[async_trait]
trait Provider: Send + Sync {
    fn id(&self) -> &str;
    async fn complete(
        &self,
        request: &ModelRequest,
        provider_model: &str,
        timeout: Option<Duration>,
    ) -> Result<ProviderCompletion, BrokerError>;
}

struct ProviderCompletion {
    text: String,
    tokens_in: Option<u32>,
    tokens_out: Option<u32>,
    finish_reason: Option<String>,
    provider_model: String,
}

struct NullProvider {
    id: String,
}

impl NullProvider {
    fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

#[async_trait]
impl Provider for NullProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        provider_model: &str,
        _timeout: Option<Duration>,
    ) -> Result<ProviderCompletion, BrokerError> {
        Ok(ProviderCompletion {
            text: request.prompt.clone(),
            tokens_in: Some(estimate_tokens(&request.prompt)),
            tokens_out: Some(estimate_tokens(&request.prompt)),
            finish_reason: Some("stop".into()),
            provider_model: provider_model.to_string(),
        })
    }
}

struct HttpProvider {
    id: String,
    client: Client,
    base_url: String,
    secret_id: Option<String>,
    headers: HeaderMap,
    schema: HttpSchema,
    secrets: Arc<dyn SecretResolver>,
}

impl HttpProvider {
    fn new(cfg: ModelProvider, secrets: Arc<dyn SecretResolver>) -> Result<Self, BrokerInitError> {
        let base_url = cfg
            .base_url
            .ok_or_else(|| BrokerInitError::MissingBaseUrl { id: cfg.id.clone() })?;

        let schema = match cfg.schema.as_deref().unwrap_or("openai-completions") {
            "openai-completions" => HttpSchema::OpenAiCompletions,
            other => {
                return Err(BrokerInitError::UnsupportedSchema {
                    id: cfg.id,
                    schema: other.to_string(),
                });
            }
        };

        let mut headers = HeaderMap::new();
        for (key, value) in cfg.headers {
            let name = HeaderName::try_from(key.as_str()).map_err(|source| {
                BrokerInitError::InvalidHeaderName {
                    id: cfg.id.clone(),
                    header: key.clone(),
                    source,
                }
            })?;
            let header_value = HeaderValue::try_from(value.as_str()).map_err(|source| {
                BrokerInitError::InvalidHeaderValue {
                    id: cfg.id.clone(),
                    header: key.clone(),
                    source,
                }
            })?;
            headers.append(name, header_value);
        }

        let client = Client::builder()
            .build()
            .map_err(|source| BrokerInitError::HttpClient {
                id: cfg.id.clone(),
                source,
            })?;

        Ok(Self {
            id: cfg.id,
            client,
            base_url,
            secret_id: cfg.secret_id,
            headers,
            schema,
            secrets,
        })
    }
}

#[async_trait]
impl Provider for HttpProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        provider_model: &str,
        timeout: Option<Duration>,
    ) -> Result<ProviderCompletion, BrokerError> {
        let secret = match self.secret_id.as_ref() {
            Some(id) => self
                .secrets
                .resolve(id)
                .ok_or_else(|| BrokerError::ProviderFault {
                    code: "secret_missing".into(),
                    message: format!("secret '{id}' not found"),
                })?,
            None => String::new(),
        };

        let url = self.schema.endpoint(&self.base_url);
        let mut body = serde_json::json!({
            "model": provider_model,
            "prompt": request.prompt,
            "stream": false,
        });

        if let Some(params) = &request.params {
            if let Some(value) = params.max_tokens {
                body["max_tokens"] = serde_json::json!(value);
            }
            if let Some(value) = params.temperature {
                body["temperature"] = serde_json::json!(value);
            }
            if let Some(value) = params.top_p {
                body["top_p"] = serde_json::json!(value);
            }
            if let Some(stop) = &params.stop {
                body["stop"] = serde_json::json!(stop);
            }
        }

        let mut builder = self.client.post(url).json(&body);
        if let Some(duration) = timeout {
            builder = builder.timeout(duration);
        }
        if !secret.is_empty() {
            builder = builder.bearer_auth(secret);
        }
        for (key, value) in self.headers.iter() {
            builder = builder.header(key, value);
        }
        builder = builder.header("X-Runloop-Trace-Id", request.trace_id.to_string());

        let response = builder
            .send()
            .await
            .map_err(|err| BrokerError::ProviderFault {
                code: "http_send".into(),
                message: err.to_string(),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "no body".to_string());
            return Err(BrokerError::ProviderFault {
                code: format!("http_{}", status.as_u16()),
                message,
            });
        }

        let payload: OpenAiCompletionResponse =
            response
                .json()
                .await
                .map_err(|err| BrokerError::ProviderFault {
                    code: "decode".into(),
                    message: err.to_string(),
                })?;

        let choice =
            payload
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| BrokerError::ProviderFault {
                    code: "empty_response".into(),
                    message: "no completion choices returned".into(),
                })?;

        let resolved_model = payload.model.unwrap_or_else(|| provider_model.to_string());

        Ok(ProviderCompletion {
            text: choice.text,
            tokens_in: payload.usage.as_ref().and_then(|usage| usage.prompt_tokens),
            tokens_out: payload
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens),
            finish_reason: choice.finish_reason.or(payload.finish_reason),
            provider_model: resolved_model,
        })
    }
}

fn build_provider(
    cfg: ModelProvider,
    secrets: Arc<dyn SecretResolver>,
) -> Result<Arc<dyn Provider>, BrokerInitError> {
    match cfg.kind {
        ProviderKind::Local => Ok(Arc::new(NullProvider::new(cfg.id))),
        ProviderKind::Http => Ok(Arc::new(HttpProvider::new(cfg, secrets)?)),
    }
}

enum HttpSchema {
    OpenAiCompletions,
}

impl HttpSchema {
    fn endpoint(&self, base: &str) -> String {
        match self {
            Self::OpenAiCompletions => format!("{}/v1/completions", base.trim_end_matches('/')),
        }
    }
}

#[derive(Deserialize)]
struct OpenAiCompletionResponse {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    #[serde(default)]
    text: String,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

struct NoopSecretResolver;

impl SecretResolver for NoopSecretResolver {
    fn resolve(&self, _secret_id: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::config::{
        BrokerCacheConfig, BrokerConfig, ModelBudgets, ModelProvider, ModelRoute,
    };

    struct TestSecrets;

    impl SecretResolver for TestSecrets {
        fn resolve(&self, _secret_id: &str) -> Option<String> {
            None
        }
    }

    fn broker_config() -> BrokerConfig {
        BrokerConfig {
            providers: vec![],
            route: vec![ModelRoute {
                pattern: "*".into(),
                provider: "null".into(),
                target_model: None,
            }],
            cache: BrokerCacheConfig {
                ttl_ms: 60_000,
                capacity: 8,
            },
            budgets: ModelBudgets::default(),
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            trace_id: TraceId::default(),
            model: "local:echo".into(),
            prompt: "hello".into(),
            params: None,
            budget_tokens: None,
            timeout_ms: None,
            cache_ttl_ms: None,
            cache_key: None,
            stream: false,
        }
    }

    #[tokio::test]
    async fn null_provider_echoes_prompt() {
        let broker = Broker::new(broker_config(), Arc::new(TestSecrets)).expect("broker init");
        let resp = broker.complete(&request()).await.expect("complete");
        assert_eq!(resp.text, "hello");
        assert!(!resp.cached);
    }

    #[tokio::test]
    async fn cache_hit_sets_flag() {
        let broker = Broker::new(broker_config(), Arc::new(TestSecrets)).expect("broker init");
        let req = ModelRequest {
            cache_ttl_ms: Some(30_000),
            ..request()
        };
        let first = broker.complete(&req).await.expect("first");
        assert!(!first.cached);
        let second = broker.complete(&req).await.expect("second");
        assert!(second.cached);
    }

    #[tokio::test]
    async fn budget_exceeded_produces_error() {
        let broker = Broker::new(broker_config(), Arc::new(TestSecrets)).expect("broker init");
        let req = ModelRequest {
            budget_tokens: Some(1),
            prompt: "wide prompt exceeding budget".into(),
            ..request()
        };
        let err = broker.complete(&req).await.expect_err("should err");
        assert!(matches!(err, BrokerError::BudgetExceeded { .. }));
    }

    #[tokio::test]
    async fn streaming_unsupported_error() {
        let broker = Broker::new(broker_config(), Arc::new(TestSecrets)).expect("broker init");
        let req = ModelRequest {
            stream: true,
            ..request()
        };
        let err = broker.complete(&req).await.expect_err("should err");
        assert!(matches!(err, BrokerError::StreamingUnsupported));
    }

    #[tokio::test]
    async fn cache_respects_budget_changes() {
        let broker = Broker::new(broker_config(), Arc::new(TestSecrets)).expect("broker init");
        let warm = ModelRequest {
            cache_ttl_ms: Some(30_000),
            budget_tokens: Some(500),
            ..request()
        };
        broker.complete(&warm).await.expect("warm cache");

        let tightened = ModelRequest {
            cache_ttl_ms: Some(30_000),
            budget_tokens: Some(1),
            ..request()
        };
        let err = broker
            .complete(&tightened)
            .await
            .expect_err("budget enforcement");
        assert!(matches!(err, BrokerError::BudgetExceeded { .. }));
    }

    #[tokio::test]
    async fn http_provider_missing_base_url_fails() {
        let cfg = BrokerConfig {
            providers: vec![ModelProvider {
                id: "http".into(),
                kind: ProviderKind::Http,
                model_dir: None,
                base_url: None,
                secret_id: None,
                headers: Default::default(),
                schema: None,
            }],
            route: vec![ModelRoute {
                pattern: "*".into(),
                provider: "http".into(),
                target_model: None,
            }],
            cache: BrokerCacheConfig::default(),
            budgets: ModelBudgets::default(),
        };
        match Broker::new(cfg, Arc::new(TestSecrets)) {
            Ok(_) => panic!("expected broker init to fail"),
            Err(BrokerInitError::MissingBaseUrl { .. }) => {}
            Err(err) => panic!("unexpected error: {err:?}"),
        }
    }
}
