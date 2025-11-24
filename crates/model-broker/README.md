# Model Broker (crates/model-broker)

## Goals

- Provide a single API for completions, embeddings, and tokenization.
- Enforce budgets & policy (tokens, cost, wall-time).
- Cache results and meter usage.
- Support local and HTTP-compatible providers.

## Trait surface

```rust
pub enum ModelKind { Chat, Completion, Embedding, Tokenizer }

pub struct Budget { pub tokens: u32, pub usd: f32, pub wall_ms: u32 }
pub struct RequestMeta { pub trace_id: u128, pub opening_id: u128, pub agent: String }

#[async_trait]
pub trait Provider: Send + Sync {
  fn id(&self) -> &'static str;
  fn supports(&self, model: &str, kind: ModelKind) -> bool;

  async fn complete(&self, model: &str, prompt: Prompt, budget: &Budget, meta: &RequestMeta) -> Result<Completion>;
  async fn embed(&self, model: &str, input: Vec<String>, budget: &Budget, meta: &RequestMeta) -> Result<Embeddings>;
  async fn tokenize(&self, model: &str, text: &str) -> Result<Vec<u32>>;
}

pub struct Broker {
  // registered providers, cache, meter, policy
}
```

### Caching & invalidation

- Cache key = hash(model, provider_id, kind, normalized_prompt_or_input,
  policy_params, tools_sig).
- Invalidate on:
  - provider version change
  - model name/version change
  - policy change (temperature/stop/budget)
  - TTL expiry (configurable per provider/model)

### Metering storage

- SQLite `broker.db` with tables:
  - `calls(trace_id, agent, provider, model, kind, tokens_in, tokens_out, usd, wall_ms, ts)`
  - `cache(key primary, value, created_ts, hit_count)`
- Expose metrics: tokens, usd, cache_hit_ratio, p50/95 latency.

### Providers (v0)

- `local:llama.cpp` (bindings)
- `http:openai` (OpenAI-compatible; also covers Anthropic/others via adapters)
- `http_gemini` (Google Gemini `generateContent`, non-streaming text
  completions)

Secret resolution: CLI and daemon resolvers currently read secrets from the
environment, trying both the raw `secret_id` and an upper-snake variant with
non-alphanumeric characters replaced by `_` (e.g., `runloop/models/gemini` →
`RUNLOOP_MODELS_GEMINI`).

### Request shape (MVP)

`ModelRequest` carries a single `prompt: String`, optional `ModelParams`, and
additive fields used by specific providers:

- `role_system: Option<String>` — forwarded as a system instruction for
  providers that support it (e.g., Gemini).
- `extras: Option<serde_json::Value>` — provider hints; the Gemini adapter reads
  `{"gemini": {"safety": [...], "response_mime_type": "text/plain"}}`.
- Existing knobs (`budget_tokens`, `timeout_ms`, `cache_*`) remain unchanged.

The broker still rejects streaming requests; when a provider adds streaming we
will gate it behind a new request/response surface.

### Policy

- Enforce `max_tokens`, `max_usd`, `max_wall_ms`.
- Deny with typed error and emit audit event to POG.
