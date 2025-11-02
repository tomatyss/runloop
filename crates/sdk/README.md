# Agent SDK (Rust)

## Lifecycle
- `init(&mut self, ctx: Ctx)` -> setup, capability inspection.
- `handle(&mut self, msg: Message, ctx: Ctx)` -> Option<Message>.
- `shutdown(&mut self)` -> optional cleanup.

```rust
#[async_trait]
pub trait Agent {
  async fn init(&mut self, ctx: Ctx) -> Result<()>;
  async fn handle(&mut self, msg: Message, ctx: Ctx) -> Result<Option<Message>>;
}

pub struct Ctx {
  pub caps: CapsView,           // read-only view of granted caps
  pub kb: KbClient,             // kb.query/search/why/propose (scoped)
  pub model: ModelClient,       // broker facade with budgets
  pub bus: BusClient,           // send/subscribe messages
  pub log: Logger,              // structured logs with trace_id
  pub time: Time,               // monotonic clock, deadlines
}
```

## Messaging

- RMP message types: `Observation|Intent|ToolCall|ToolResult|Artifact|Critique|StateDelta`.
- Schema registry IDs are generated; codegen creates typed structs.

## Capabilities

- Declare in `policy.caps` at bundle time; SDK exposes `ctx.caps.check("kb.write.contacts")`.
- All external actions must be guarded and produce human-readable errors on denial.

## Testing

- `runloop-agent-test` harness spins the agent in-process with fake caps, golden messages, and asserts deterministic output.

## Packaging

- `agent.toml` (id, version, entry wasm, requires caps, limits).
- Build: `cargo runloop:build` → wasm32-wasi binary + manifest + caps file.
