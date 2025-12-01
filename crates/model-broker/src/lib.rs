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
use serde_json::Value;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    pub role_system: Option<String>,
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
    #[serde(default)]
    pub extras: Option<Value>,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct BrokerStats {
    pub calls: u64,
    pub cache_hits: u64,
    pub tokens_prompt: u64,
    pub tokens_completion: u64,
}

struct BrokerInner {
    providers: HashMap<String, Arc<dyn Provider>>,
    routes: Vec<RouteRule>,
    cache: Option<Arc<BrokerCache>>,
    budgets: BudgetLimits,
    calls: AtomicU64,
    cache_hits: AtomicU64,
    tokens_prompt: AtomicU64,
    tokens_completion: AtomicU64,
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
                calls: AtomicU64::new(0),
                cache_hits: AtomicU64::new(0),
                tokens_prompt: AtomicU64::new(0),
                tokens_completion: AtomicU64::new(0),
            }),
        })
    }

    pub fn stats(&self) -> BrokerStats {
        BrokerStats {
            calls: self.inner.calls.load(Ordering::Relaxed),
            cache_hits: self.inner.cache_hits.load(Ordering::Relaxed),
            tokens_prompt: self.inner.tokens_prompt.load(Ordering::Relaxed),
            tokens_completion: self.inner.tokens_completion.load(Ordering::Relaxed),
        }
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
        self.inner.calls.fetch_add(1, Ordering::Relaxed);

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
            self.inner.cache_hits.fetch_add(1, Ordering::Relaxed);
            let mut output = hit;
            let (tokens_in, tokens_out, total_tokens) =
                usage_profile(&output, &request.prompt, request.role_system.as_deref());
            if let Err(err) = inner.budgets.enforce(request, total_tokens) {
                record_error(&err);
                return Err(err);
            }
            output.tokens_in = tokens_in;
            output.tokens_out = tokens_out;
            record_usage(&self.inner, tokens_in, tokens_out);
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

        let (tokens_in, tokens_out, total_tokens) =
            usage_profile(&output, &request.prompt, request.role_system.as_deref());
        if let Err(err) = inner.budgets.enforce(request, total_tokens) {
            record_error(&err);
            return Err(err);
        }
        output.tokens_in = tokens_in;
        output.tokens_out = tokens_out;
        record_usage(&self.inner, tokens_in, tokens_out);

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
    if let Some(system) = &request.role_system {
        hasher.update(system.as_bytes());
    }
    if let Some(params) = &request.params
        && let Ok(encoded) = serde_json::to_vec(params)
    {
        hasher.update(&encoded);
    }
    if let Some(extras) = &request.extras
        && let Ok(encoded) = serde_json::to_vec(extras)
    {
        hasher.update(&encoded);
    }
    if let Some(key) = extra {
        hasher.update(key.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn estimate_tokens(text: &str) -> u32 {
    estimate_tokens_from_len(text.len())
}

fn estimate_tokens_from_len(len: usize) -> u32 {
    // Simple heuristic: 4 characters ≈ 1 token.
    len.div_ceil(4) as u32
}

fn usage_profile(
    output: &ModelOutput,
    prompt: &str,
    system_instruction: Option<&str>,
) -> (Option<u32>, Option<u32>, u32) {
    let prompt_len = prompt.len() + system_instruction.map_or(0, |text| text.len());
    let tokens_in = output
        .tokens_in
        .or_else(|| Some(estimate_tokens_from_len(prompt_len)));
    let tokens_out = output
        .tokens_out
        .or_else(|| Some(estimate_tokens(&output.text)));
    let total = tokens_in
        .unwrap_or(0)
        .saturating_add(tokens_out.unwrap_or(0));
    (tokens_in, tokens_out, total)
}

fn record_usage(inner: &Arc<BrokerInner>, tokens_in: Option<u32>, tokens_out: Option<u32>) {
    if let Some(t_in) = tokens_in {
        inner
            .tokens_prompt
            .fetch_add(t_in as u64, Ordering::Relaxed);
    }
    if let Some(t_out) = tokens_out {
        inner
            .tokens_completion
            .fetch_add(t_out as u64, Ordering::Relaxed);
    }
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

#[derive(Debug)]
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

struct OpenAiChatProvider {
    id: String,
    client: Client,
    base_url: String,
    secret_id: Option<String>,
    headers: HeaderMap,
    secrets: Arc<dyn SecretResolver>,
}

impl OpenAiChatProvider {
    fn new(cfg: ModelProvider, secrets: Arc<dyn SecretResolver>) -> Result<Self, BrokerInitError> {
        let base_url = cfg
            .base_url
            .ok_or_else(|| BrokerInitError::MissingBaseUrl { id: cfg.id.clone() })?;

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
            secrets,
        })
    }
}

#[async_trait]
impl Provider for OpenAiChatProvider {
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

        let mut messages = Vec::new();
        if let Some(system) = &request.role_system {
            messages.push(OpenAiChatMessage {
                role: "system".into(),
                content: system.clone(),
            });
        }
        messages.push(OpenAiChatMessage {
            role: "user".into(),
            content: request.prompt.clone(),
        });

        let mut body = serde_json::Map::new();
        body.insert("model".into(), serde_json::json!(provider_model));
        body.insert("messages".into(), serde_json::json!(messages));
        body.insert("stream".into(), serde_json::json!(false));

        if let Some(params) = &request.params {
            if let Some(value) = params.max_tokens {
                body.insert("max_tokens".into(), serde_json::json!(value));
            }
            if let Some(value) = params.temperature {
                body.insert("temperature".into(), serde_json::json!(value));
            }
            if let Some(value) = params.top_p {
                body.insert("top_p".into(), serde_json::json!(value));
            }
            if let Some(stop) = &params.stop {
                body.insert("stop".into(), serde_json::json!(stop));
            }
        }
        let body = serde_json::Value::Object(body);

        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
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

        let payload: OpenAiChatResponse =
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

        let text = choice
            .message
            .as_ref()
            .map(|msg| msg.content.clone())
            .unwrap_or_default();
        if text.is_empty() {
            return Err(BrokerError::ProviderFault {
                code: "empty_response".into(),
                message: "chat completion returned empty content".into(),
            });
        }

        let resolved_model = payload.model.unwrap_or_else(|| provider_model.to_string());

        Ok(ProviderCompletion {
            text,
            tokens_in: payload.usage.as_ref().and_then(|usage| usage.prompt_tokens),
            tokens_out: payload
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens),
            finish_reason: choice
                .finish_reason
                .or(payload.finish_reason)
                .map(|val| val.to_ascii_lowercase()),
            provider_model: resolved_model,
        })
    }
}

struct AnthropicProvider {
    id: String,
    client: Client,
    base_url: String,
    secret_id: Option<String>,
    headers: HeaderMap,
    secrets: Arc<dyn SecretResolver>,
}

impl AnthropicProvider {
    fn new(cfg: ModelProvider, secrets: Arc<dyn SecretResolver>) -> Result<Self, BrokerInitError> {
        let base_url = cfg
            .base_url
            .ok_or_else(|| BrokerInitError::MissingBaseUrl { id: cfg.id.clone() })?;

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
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

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
            secrets,
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        provider_model: &str,
        timeout: Option<Duration>,
    ) -> Result<ProviderCompletion, BrokerError> {
        let api_key = match self.secret_id.as_ref() {
            Some(id) => self
                .secrets
                .resolve(id)
                .ok_or_else(|| BrokerError::ProviderFault {
                    code: "secret_missing".into(),
                    message: format!("secret '{id}' not found"),
                })?,
            None => String::new(),
        };

        let mut body = serde_json::json!({
            "model": provider_model,
            "messages": [
                {
                    "role": "user",
                    "content": request.prompt
                }
            ],
            "stream": false,
        });
        if let Some(system) = &request.role_system {
            body["system"] = serde_json::json!(system);
        }
        let mut max_tokens = request
            .params
            .as_ref()
            .and_then(|p| p.max_tokens)
            .unwrap_or(512);
        if max_tokens == 0 {
            max_tokens = 1;
        }
        body["max_tokens"] = serde_json::json!(max_tokens);
        if let Some(params) = &request.params {
            if let Some(value) = params.temperature {
                body["temperature"] = serde_json::json!(value);
            }
            if let Some(value) = params.top_p {
                body["top_p"] = serde_json::json!(value);
            }
            if let Some(stop) = &params.stop {
                body["stop_sequences"] = serde_json::json!(stop);
            }
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut builder = self.client.post(url).json(&body);
        if let Some(duration) = timeout {
            builder = builder.timeout(duration);
        }
        if !api_key.is_empty() {
            builder = builder.header("x-api-key", api_key);
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

        let payload: AnthropicResponse =
            response
                .json()
                .await
                .map_err(|err| BrokerError::ProviderFault {
                    code: "decode".into(),
                    message: err.to_string(),
                })?;

        let text = payload
            .content
            .iter()
            .filter_map(|block| block.text.clone())
            .collect::<Vec<_>>()
            .join("");
        if text.is_empty() {
            return Err(BrokerError::ProviderFault {
                code: "empty_response".into(),
                message: "anthropic response contained no text".into(),
            });
        }

        Ok(ProviderCompletion {
            text,
            tokens_in: payload.usage.as_ref().and_then(|u| u.input_tokens),
            tokens_out: payload.usage.as_ref().and_then(|u| u.output_tokens),
            finish_reason: payload.stop_reason.map(|val| val.to_ascii_lowercase()),
            provider_model: provider_model.to_string(),
        })
    }
}

struct OllamaProvider {
    id: String,
    client: Client,
    base_url: String,
}

impl OllamaProvider {
    fn new(cfg: ModelProvider) -> Result<Self, BrokerInitError> {
        let base_url = cfg
            .base_url
            .ok_or_else(|| BrokerInitError::MissingBaseUrl { id: cfg.id.clone() })?;
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
        })
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        provider_model: &str,
        timeout: Option<Duration>,
    ) -> Result<ProviderCompletion, BrokerError> {
        let mut options = serde_json::Map::new();
        if let Some(params) = &request.params {
            if let Some(value) = params.temperature {
                options.insert("temperature".into(), serde_json::json!(value));
            }
            if let Some(value) = params.top_p {
                options.insert("top_p".into(), serde_json::json!(value));
            }
            if let Some(stop) = &params.stop {
                options.insert("stop".into(), serde_json::json!(stop.clone()));
            }
        }
        if let Some(Value::Object(map)) = request
            .extras
            .as_ref()
            .and_then(|extras| extras.get("ollama"))
        {
            for (k, v) in map {
                options.insert(k.clone(), v.clone());
            }
        }

        let mut body = serde_json::json!({
            "model": provider_model,
            "prompt": request.prompt,
            "stream": false,
        });
        if let Some(system) = &request.role_system {
            body["system"] = serde_json::json!(system);
        }
        if !options.is_empty() {
            body["options"] = serde_json::Value::Object(options);
        }

        let url = format!("{}/api/generate", self.base_url.trim_end_matches('/'));
        let mut builder = self.client.post(url).json(&body);
        if let Some(duration) = timeout {
            builder = builder.timeout(duration);
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

        let payload: OllamaResponse =
            response
                .json()
                .await
                .map_err(|err| BrokerError::ProviderFault {
                    code: "decode".into(),
                    message: err.to_string(),
                })?;

        if payload.response.trim().is_empty() {
            return Err(BrokerError::ProviderFault {
                code: "empty_response".into(),
                message: "ollama returned empty response".into(),
            });
        }

        Ok(ProviderCompletion {
            text: payload.response,
            tokens_in: payload.prompt_eval_count,
            tokens_out: payload.eval_count,
            finish_reason: None,
            provider_model: provider_model.to_string(),
        })
    }
}

struct GeminiProvider {
    id: String,
    client: Client,
    base_url: String,
    secret_id: Option<String>,
    headers: HeaderMap,
    secrets: Arc<dyn SecretResolver>,
}

impl GeminiProvider {
    fn new(cfg: ModelProvider, secrets: Arc<dyn SecretResolver>) -> Result<Self, BrokerInitError> {
        let base_url = cfg
            .base_url
            .ok_or_else(|| BrokerInitError::MissingBaseUrl { id: cfg.id.clone() })?;

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
            secrets,
        })
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        provider_model: &str,
        timeout: Option<Duration>,
    ) -> Result<ProviderCompletion, BrokerError> {
        let api_key = match self.secret_id.as_ref() {
            Some(id) => self
                .secrets
                .resolve(id)
                .ok_or_else(|| BrokerError::ProviderFault {
                    code: "secret_missing".into(),
                    message: format!("secret '{id}' not found"),
                })?,
            None => String::new(),
        };

        let hints = parse_gemini_hints(&request.extras)?;
        let generation_config = build_gemini_generation_config(request, &hints);
        let system_instruction = request.role_system.as_ref().map(|text| GeminiContent {
            role: Some("system".into()),
            parts: vec![GeminiPart {
                text: Some(text.clone()),
            }],
        });
        let payload = GeminiRequestPayload {
            contents: vec![GeminiContent {
                role: Some("user".into()),
                parts: vec![GeminiPart {
                    text: Some(request.prompt.clone()),
                }],
            }],
            system_instruction,
            generation_config,
            safety_settings: hints.safety_settings.clone(),
        };

        let model_path = gemini_model_path(provider_model);
        let url = format!(
            "{}/v1beta/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            model_path
        );

        let mut builder = self.client.post(url).json(&payload);
        if let Some(duration) = timeout {
            builder = builder.timeout(duration);
        }
        if !api_key.is_empty() {
            builder = builder.header("X-Goog-Api-Key", api_key);
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

        let payload: GeminiResponse =
            response
                .json()
                .await
                .map_err(|err| BrokerError::ProviderFault {
                    code: "decode".into(),
                    message: err.to_string(),
                })?;

        let GeminiResponse {
            candidates,
            usage_metadata,
            model_version,
            prompt_feedback,
        } = payload;

        if let Some(reason) = prompt_block_reason(&prompt_feedback) {
            return Err(BrokerError::ProviderFault {
                code: "safety_blocked".into(),
                message: format!("Gemini blocked the prompt ({reason})"),
            });
        }

        let mut text_candidate: Option<(Option<String>, String)> = None;
        let mut safety_reason: Option<String> = None;
        for candidate in candidates {
            if text_candidate.is_some() {
                break;
            }
            if safety_reason.is_none()
                && candidate
                    .finish_reason
                    .as_deref()
                    .is_some_and(is_safety_finish_reason)
            {
                safety_reason = candidate.finish_reason.clone();
            }
            if let Some(content) = candidate.content
                && let Some(text) = collect_text_parts(content.parts)
            {
                text_candidate = Some((candidate.finish_reason.clone(), text));
                break;
            }
        }

        let (finish_reason_raw, text) = match text_candidate {
            Some(value) => value,
            None => {
                if let Some(reason) = safety_reason {
                    return Err(BrokerError::ProviderFault {
                        code: "safety_blocked".into(),
                        message: format!("Gemini blocked the completion (finish_reason={reason})"),
                    });
                }
                return Err(BrokerError::ProviderFault {
                    code: "empty_response".into(),
                    message: "no completion candidates returned".into(),
                });
            }
        };

        let finish_reason = finish_reason_raw
            .as_deref()
            .map(|value| value.to_ascii_lowercase());
        let tokens_in = usage_metadata
            .as_ref()
            .and_then(|usage| usage.prompt_token_count);
        let tokens_out = usage_metadata
            .as_ref()
            .and_then(|usage| usage.candidates_token_count);
        let resolved_model = model_version.unwrap_or(model_path);

        Ok(ProviderCompletion {
            text,
            tokens_in,
            tokens_out,
            finish_reason,
            provider_model: resolved_model,
        })
    }
}

fn build_gemini_generation_config(
    request: &ModelRequest,
    hints: &GeminiHints,
) -> Option<GeminiGenerationConfig> {
    let mut cfg = GeminiGenerationConfig::default();
    if let Some(params) = &request.params {
        cfg.temperature = params.temperature;
        cfg.top_p = params.top_p;
        cfg.max_output_tokens = params.max_tokens;
        if let Some(stop) = &params.stop {
            cfg.stop_sequences = stop.clone();
        }
    }
    cfg.response_mime_type = hints.response_mime_type.clone();
    if cfg.is_empty() { None } else { Some(cfg) }
}

#[derive(Default)]
struct GeminiHints {
    safety_settings: Option<Vec<GeminiSafetySetting>>,
    response_mime_type: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct GeminiRequestPayload {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    safety_settings: Option<Vec<GeminiSafetySetting>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeminiContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeminiPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
}

impl GeminiGenerationConfig {
    fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.max_output_tokens.is_none()
            && self.stop_sequences.is_empty()
            && self.response_mime_type.is_none()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct GeminiSafetySetting {
    category: String,
    threshold: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsageMetadata>,
    #[serde(default)]
    model_version: Option<String>,
    #[serde(default)]
    prompt_feedback: Option<GeminiPromptFeedback>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    #[serde(default)]
    prompt_token_count: Option<u32>,
    #[serde(default)]
    candidates_token_count: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPromptFeedback {
    #[serde(default)]
    block_reason: Option<String>,
}

fn prompt_block_reason(feedback: &Option<GeminiPromptFeedback>) -> Option<String> {
    feedback
        .as_ref()
        .and_then(|fb| fb.block_reason.as_deref())
        .and_then(|reason| {
            if reason.eq_ignore_ascii_case("BLOCK_REASON_UNSPECIFIED") {
                None
            } else {
                Some(reason.to_string())
            }
        })
}

fn parse_gemini_hints(extras: &Option<Value>) -> Result<GeminiHints, BrokerError> {
    let Some(value) = extras else {
        return Ok(GeminiHints::default());
    };
    match value {
        Value::Null => Ok(GeminiHints::default()),
        Value::Object(map) => {
            let Some(gemini) = map.get("gemini") else {
                return Ok(GeminiHints::default());
            };
            if gemini.is_null() {
                return Ok(GeminiHints::default());
            }
            let payload: GeminiExtrasPayload =
                serde_json::from_value(gemini.clone()).map_err(|err| {
                    BrokerError::InvalidRequest {
                        reason: format!("extras.gemini invalid: {err}"),
                    }
                })?;
            Ok(payload.into())
        }
        _ => Err(BrokerError::InvalidRequest {
            reason: "extras must be a JSON object".into(),
        }),
    }
}

#[derive(Deserialize, Default)]
struct GeminiExtrasPayload {
    #[serde(default)]
    safety: Option<Vec<GeminiSafetySetting>>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    response_mime_type: Option<String>,
}

impl From<GeminiExtrasPayload> for GeminiHints {
    fn from(value: GeminiExtrasPayload) -> Self {
        Self {
            safety_settings: value.safety,
            response_mime_type: value.response_mime_type.or(value.mime_type),
        }
    }
}

fn gemini_model_path(model: &str) -> String {
    if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    }
}

fn is_safety_finish_reason(reason: &str) -> bool {
    matches!(
        reason.to_ascii_uppercase().as_str(),
        "SAFETY" | "BLOCKLIST" | "PROHIBITED_CONTENT"
    )
}

fn collect_text_parts(parts: Vec<GeminiPart>) -> Option<String> {
    let mut buffer = String::new();
    for part in parts {
        if let Some(text) = part.text {
            buffer.push_str(&text);
        }
    }
    if buffer.is_empty() {
        None
    } else {
        Some(buffer)
    }
}

fn build_provider(
    cfg: ModelProvider,
    secrets: Arc<dyn SecretResolver>,
) -> Result<Arc<dyn Provider>, BrokerInitError> {
    match cfg.kind {
        ProviderKind::Local => Ok(Arc::new(NullProvider::new(cfg.id))),
        ProviderKind::Http => Ok(Arc::new(HttpProvider::new(cfg, secrets)?)),
        ProviderKind::HttpGemini => Ok(Arc::new(GeminiProvider::new(cfg, secrets)?)),
        ProviderKind::HttpOpenAiChat => Ok(Arc::new(OpenAiChatProvider::new(cfg, secrets)?)),
        ProviderKind::HttpAnthropic => Ok(Arc::new(AnthropicProvider::new(cfg, secrets)?)),
        ProviderKind::HttpOllama => Ok(Arc::new(OllamaProvider::new(cfg)?)),
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

#[derive(Deserialize)]
struct OpenAiChatResponse {
    #[serde(default)]
    choices: Vec<OpenAiChatChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiChatChoice {
    #[serde(default)]
    message: Option<OpenAiChatMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct OpenAiChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct OllamaResponse {
    #[serde(default)]
    response: String,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
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
    use httpmock::prelude::*;
    use runloop_core::config::{
        BrokerCacheConfig, BrokerConfig, ModelBudgets, ModelProvider, ModelRoute,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::net::TcpListener;
    use std::sync::Arc;

    struct TestSecrets;

    impl SecretResolver for TestSecrets {
        fn resolve(&self, _secret_id: &str) -> Option<String> {
            None
        }
    }

    struct MapSecrets(HashMap<String, String>);

    impl MapSecrets {
        fn with_secret(id: &str, value: &str) -> Self {
            let mut map = HashMap::new();
            map.insert(id.to_string(), value.to_string());
            Self(map)
        }
    }

    impl SecretResolver for MapSecrets {
        fn resolve(&self, secret_id: &str) -> Option<String> {
            self.0.get(secret_id).cloned()
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
            role_system: None,
            params: None,
            budget_tokens: None,
            timeout_ms: None,
            cache_ttl_ms: None,
            cache_key: None,
            stream: false,
            extras: None,
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

    static HTTPMOCK_GUARD: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn net_available() -> bool {
        TcpListener::bind("127.0.0.1:0").is_ok()
    }

    fn httpmock_lock() -> std::sync::MutexGuard<'static, ()> {
        HTTPMOCK_GUARD
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("httpmock mutex poisoned")
    }

    #[tokio::test]
    async fn gemini_provider_maps_requests_and_responses() {
        if !net_available() {
            return;
        }
        let _guard = httpmock_lock();
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.method(POST).header("x-goog-api-key", "test-key");
                then.status(200).json_body(json!({
                    "candidates": [
                        {
                            "content": {
                                "parts": [
                                    {"text": "hi"},
                                    {"text": " there"}
                                ]
                            },
                            "finishReason": "STOP"
                        }
                    ],
                    "usageMetadata": {
                        "promptTokenCount": 12,
                        "candidatesTokenCount": 24
                    },
                    "modelVersion": "models/gemini-1.5-flash"
                }));
            })
            .await;

        let cfg = ModelProvider {
            id: "gemini".into(),
            kind: ProviderKind::HttpGemini,
            model_dir: None,
            base_url: Some(server.base_url()),
            secret_id: Some("gemini_api".into()),
            headers: Default::default(),
            schema: None,
        };
        let provider = GeminiProvider::new(
            cfg,
            Arc::new(MapSecrets::with_secret("gemini_api", "test-key")),
        )
        .expect("provider init");

        let request = ModelRequest {
            trace_id: TraceId::new(),
            model: "gemini-1.5-flash".into(),
            prompt: "hello".into(),
            role_system: Some("be concise".into()),
            params: Some(ModelParams {
                temperature: Some(0.2),
                top_p: Some(0.9),
                max_tokens: Some(64),
                stop: Some(vec!["STOP".into()]),
            }),
            budget_tokens: Some(8000),
            timeout_ms: Some(1_000),
            cache_ttl_ms: None,
            cache_key: None,
            stream: false,
            extras: Some(json!({
                "gemini": {
                    "safety": [{
                        "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                        "threshold": "BLOCK_NONE"
                    }],
                    "response_mime_type": "text/plain"
                }
            })),
        };

        let completion = provider
            .complete(&request, "gemini-1.5-flash", None)
            .await
            .expect("completion");
        assert_eq!(completion.text, "hi there");
        assert_eq!(completion.tokens_in, Some(12));
        assert_eq!(completion.tokens_out, Some(24));
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.provider_model, "models/gemini-1.5-flash");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn gemini_provider_surfaces_safety_blocks() {
        if !net_available() {
            return;
        }
        let _guard = httpmock_lock();
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.method(POST).header("x-goog-api-key", "test-key");
                then.status(200).json_body(json!({
                    "candidates": [
                        {
                            "finishReason": "SAFETY"
                        }
                    ],
                    "promptFeedback": {
                        "blockReason": "SAFETY"
                    }
                }));
            })
            .await;

        let cfg = ModelProvider {
            id: "gemini".into(),
            kind: ProviderKind::HttpGemini,
            model_dir: None,
            base_url: Some(server.base_url()),
            secret_id: Some("gemini_api".into()),
            headers: Default::default(),
            schema: None,
        };
        let provider = GeminiProvider::new(
            cfg,
            Arc::new(MapSecrets::with_secret("gemini_api", "test-key")),
        )
        .expect("provider init");

        let request = ModelRequest {
            trace_id: TraceId::new(),
            model: "gemini-pro".into(),
            prompt: "hello".into(),
            role_system: None,
            params: None,
            budget_tokens: None,
            timeout_ms: None,
            cache_ttl_ms: None,
            cache_key: None,
            stream: false,
            extras: None,
        };

        let err = provider
            .complete(&request, "gemini-pro", None)
            .await
            .expect_err("should be blocked");
        match err {
            BrokerError::ProviderFault { code, message } => {
                assert_eq!(code, "safety_blocked");
                assert!(message.contains("Gemini"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openai_chat_provider_maps_requests_and_responses() {
        if !net_available() {
            return;
        }
        let _guard = httpmock_lock();
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(json!({
                "choices": [
                    {
                        "message": {"role": "assistant", "content": "hi there"},
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 4
                },
                "model": "gpt-4o-mini"
            }));
        });

        let cfg = ModelProvider {
            id: "openai".into(),
            kind: ProviderKind::HttpOpenAiChat,
            model_dir: None,
            base_url: Some(server.base_url()),
            secret_id: Some("openai_api".into()),
            headers: Default::default(),
            schema: None,
        };
        let provider = OpenAiChatProvider::new(
            cfg,
            Arc::new(MapSecrets::with_secret("openai_api", "test-key")),
        )
        .expect("provider init");

        let request = ModelRequest {
            model: "gpt-4o-mini".into(),
            prompt: "hello".into(),
            role_system: Some("be short".into()),
            params: Some(ModelParams {
                temperature: Some(0.3),
                top_p: Some(0.9),
                max_tokens: Some(64),
                stop: Some(vec!["stop".into()]),
            }),
            ..request()
        };

        let completion = provider
            .complete(&request, "gpt-4o-mini", None)
            .await
            .expect("completion");
        assert_eq!(completion.text, "hi there");
        assert_eq!(completion.tokens_in, Some(12));
        assert_eq!(completion.tokens_out, Some(4));
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.provider_model, "gpt-4o-mini");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn anthropic_provider_maps_requests_and_responses() {
        if !net_available() {
            return;
        }
        let _guard = httpmock_lock();
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1/messages")
                    .header("x-api-key", "anth-key")
                    .header("anthropic-version", "2023-06-01");
                then.status(200).json_body(json!({
                    "content": [
                        {"text": "hello from claude"}
                    ],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 5
                    }
                }));
            })
            .await;

        let cfg = ModelProvider {
            id: "claude".into(),
            kind: ProviderKind::HttpAnthropic,
            model_dir: None,
            base_url: Some(server.base_url()),
            secret_id: Some("anthropic_api".into()),
            headers: Default::default(),
            schema: None,
        };
        let provider = AnthropicProvider::new(
            cfg,
            Arc::new(MapSecrets::with_secret("anthropic_api", "anth-key")),
        )
        .expect("provider init");

        let request = ModelRequest {
            model: "claude-3-haiku-20240307".into(),
            prompt: "hello".into(),
            role_system: Some("you are brief".into()),
            params: Some(ModelParams {
                temperature: Some(0.5),
                top_p: Some(0.9),
                max_tokens: Some(32),
                stop: Some(vec!["stop".into()]),
            }),
            ..request()
        };

        let completion = provider
            .complete(&request, "claude-3-haiku-20240307", None)
            .await
            .expect("completion");
        assert_eq!(completion.text, "hello from claude");
        assert_eq!(completion.tokens_in, Some(10));
        assert_eq!(completion.tokens_out, Some(5));
        assert_eq!(completion.finish_reason.as_deref(), Some("end_turn"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn ollama_provider_maps_requests_and_responses() {
        if !net_available() {
            return;
        }
        let _guard = httpmock_lock();
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.method(POST).path("/api/generate").json_body(json!({
                    "model": "llama3:8b",
                    "prompt": "hello",
                    "stream": false
                }));
                then.status(200).json_body(json!({
                    "response": "hey there",
                    "prompt_eval_count": 8,
                    "eval_count": 6,
                    "done": true
                }));
            })
            .await;

        let cfg = ModelProvider {
            id: "ollama".into(),
            kind: ProviderKind::HttpOllama,
            model_dir: None,
            base_url: Some(server.base_url()),
            secret_id: None,
            headers: Default::default(),
            schema: None,
        };
        let provider = OllamaProvider::new(cfg).expect("provider init");

        let completion = provider
            .complete(&request(), "llama3:8b", None)
            .await
            .expect("completion");
        assert_eq!(completion.text, "hey there");
        assert_eq!(completion.tokens_in, Some(8));
        assert_eq!(completion.tokens_out, Some(6));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn ollama_provider_forwards_system_prompt() {
        if !net_available() {
            return;
        }
        let _guard = httpmock_lock();
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.method(POST).path("/api/generate").json_body(json!({
                    "model": "llama3:8b",
                    "prompt": "hello",
                    "system": "stay brief",
                    "stream": false
                }));
                then.status(200).json_body(json!({
                    "response": "brief reply",
                    "done": true
                }));
            })
            .await;

        let cfg = ModelProvider {
            id: "ollama".into(),
            kind: ProviderKind::HttpOllama,
            model_dir: None,
            base_url: Some(server.base_url()),
            secret_id: None,
            headers: Default::default(),
            schema: None,
        };
        let provider = OllamaProvider::new(cfg).expect("provider init");
        let request = ModelRequest {
            role_system: Some("stay brief".into()),
            ..request()
        };

        let completion = provider
            .complete(&request, "llama3:8b", None)
            .await
            .expect("completion");
        assert_eq!(completion.text, "brief reply");
        mock.assert_async().await;
    }

    #[test]
    fn usage_profile_counts_system_instruction_text() {
        let mut output = ModelOutput {
            text: "done".into(),
            tokens_in: None,
            tokens_out: None,
            cached: false,
            provider: "null".into(),
            provider_model: "null".into(),
            latency_ms: 0,
            finish_reason: None,
        };
        let (tokens_in, tokens_out, total) =
            usage_profile(&output, "prompt", Some("system instruction"));
        assert_eq!(tokens_out, Some(estimate_tokens("done")));
        let expected_in = estimate_tokens_from_len("prompt".len() + "system instruction".len());
        assert_eq!(tokens_in, Some(expected_in));
        assert_eq!(total, expected_in + tokens_out.unwrap());

        // Ensure explicit tokens override heuristic.
        output.tokens_in = Some(42);
        let (tokens_in, _, _) = usage_profile(&output, "prompt", Some("system instruction"));
        assert_eq!(tokens_in, Some(42));
    }

    #[tokio::test]
    async fn broker_routes_requests_to_gemini_provider() {
        if !net_available() {
            return;
        }
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1beta/models/gemini-1.5-flash:generateContent")
                    .header("x-goog-api-key", "test-key");
                then.status(200).json_body(json!({
                    "candidates": [
                        {
                            "content": {
                                "parts": [
                                    {"text": "r1"},
                                    {"text": " r2"}
                                ]
                            },
                            "finishReason": "STOP"
                        }
                    ]
                }));
            })
            .await;

        let cfg = BrokerConfig {
            providers: vec![ModelProvider {
                id: "gemini".into(),
                kind: ProviderKind::HttpGemini,
                model_dir: None,
                base_url: Some(server.base_url()),
                secret_id: Some("gemini_api".into()),
                headers: Default::default(),
                schema: None,
            }],
            route: vec![ModelRoute {
                pattern: "gemini-*".into(),
                provider: "gemini".into(),
                target_model: None,
            }],
            cache: BrokerCacheConfig::default(),
            budgets: ModelBudgets::default(),
        };
        let broker = Broker::new(
            cfg,
            Arc::new(MapSecrets::with_secret("gemini_api", "test-key")),
        )
        .expect("broker init");

        let request = ModelRequest {
            trace_id: TraceId::new(),
            model: "gemini-1.5-flash".into(),
            prompt: "hello".into(),
            role_system: Some("stay concise".into()),
            params: None,
            budget_tokens: Some(2_000),
            timeout_ms: Some(500),
            cache_ttl_ms: None,
            cache_key: None,
            stream: false,
            extras: None,
        };

        let output = broker.complete(&request).await.expect("completion");
        assert_eq!(output.text, "r1 r2");
        assert_eq!(output.provider, "gemini");
        mock.assert_async().await;
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
