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

- Cache key = hash(model, provider_id, kind, normalized_prompt_or_input, policy_params, tools_sig).
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

### Policy

- Enforce `max_tokens`, `max_usd`, `max_wall_ms`.
- Deny with typed error and emit audit event to POG.
