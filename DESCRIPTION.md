# Runloop OS — DESCRIPTION

Runloop is an **agent‑native, terminal‑first operating system**. Every prompt
you type is routed either to the **shell** (fast path) or to a **crew of AI
agents** (“openings”) that plan, act, and produce artifacts in the background.
Agents are ultra‑light, capability‑scoped tasks (WASI/Wasm) orchestrated over a
typed message bus, with complete provenance and deterministic replay.

## Why Runloop

- **Terminal‑first**: no windows; everything is inspectable as text.
- **Local‑first, cloud‑optional**: runs offline; can sync/enrich when online.
- **Composable agents**: declare a plan (“opening”), run/replay it, and audit
  the trace.
- **Trustworthy memory**: an event‑sourced personal knowledge base with
  explainable answers.
- **Least‑privilege by default**: capabilities gate every file, net, time, KB,
  secrets, and model access.

## Core mental model

- **Trajectories** — individual agents (LLM + tools) with a goal and budget.
- **Crossings** — typed interactions (messages, artifacts) among agents.
- **Openings** — a declarative DAG (the crew and their crossings) that you can
  run, pause, step, or replay.

## System overview

**Router → Opening Engine → Runtime → Message Bus → Model Broker → Knowledge
Base (KB) → Observability/TUI**

### 1) Router

- Fast‑path executes exact shell commands (`ls | grep foo`).
- Otherwise, classifies intent and launches a matching **opening** (crew plan).
- Always shows _why_ it routed and the proposed plan; you press `Enter` to run.

### 2) Openings (composition)

- Openings are declarative DAGs with retries, timeouts, budgets, and success
  criteria.
- Deterministic replay against the event log enables debugging and
  self‑improvement.
- Example (sketch):

```text

opening "compose_email" {
goals: ["email to john about q4 plan"]
nodes:
contacts := agent("contact_resolver")
context  := agent("context_gatherer")
draft    := agent("writer", model="mixtral-8x7b")
review   := agent("critic")
send     := agent("mailer", require_human_confirm=true)
edges:
contacts.out -> draft.recipients
context.out  -> draft.context
draft.out    -> review.in
review.ok    -> send.in
}

```

### 3) Runtime & agent container

- **Process model**: wasm32‑WASI (Wasmtime). Cold‑start and RSS are
  near‑process; sandboxing is strong.
- **Agent bundle**: `{ manifest.toml, wasm, tools.json, policy.caps }`.
- **Capabilities**: filesystem (scoped), network (allow‑list), exec (off by
  default), time, KB read/write, secret‑ids, model access via broker.
- **IPC**: local message bus (Unix sockets) with zero‑copy blobs and signed,
  typed messages.

### 4) Message protocol (RMP)

- **Framing**: stream transport with `u32 frame_len`, fixed 64-byte header, then
  MsgPack body. All integers are big-endian.
- **Header (v0)**: `magic="RMP0"`, `header_version=0`, `header_len=64`,
  `flags=0`, `schema_id`, `body_len`, `created_at_ms`, `ttl_ms`, `trace_id`,
  `msg_id`, reserved zeros. `opening_id` and other metadata stay in the body.
- **Body**: `{ type, payload }` MsgPack map with JSON Schema on disk. Body
  `type` must align with the primitive family named by `schema_id`.
- **Primitives**: `Observation`, `Intent`, `ToolCall`, `ToolResult`, `Artifact`,
  `Critique`, `StateDelta`, `ErrorReport`, etc.
- **Delivery**: TTL enforced via `created_at_ms + ttl_ms` (u128 math), dedupe on
  `(trace_id,msg_id)` per topic/subscriber, `rlp/sys/drops` telemetry for TTL,
  duplicate, and back-pressure drops.
- **Provenance**: every message carries `who/why/what/model@version` in the body
  payload for replay and audit.

### 5) Knowledge Base (KB) — “Personal Ops Graph”

- **Event‑sourced**: append‑only `events` log (SQLite).
- **Materialized views**: `facts`, `artifacts`, `contacts`, `accounts`, `edges`.
- **Vector index**: HNSW for semantic recall with provenance filters.
- **Canonical forms**:
- JSON payloads/provenance are **JCS‑canonical** (stable hashing).
- Content hashes use **BLAKE3**; stored as 32‑byte binary.
- Timestamps are **integers in milliseconds** (`ts_ms`); ISO is rendered on
  read.
- **Ingestion**: agents don’t mutate state directly; they **propose**
  `StateDelta` events which a validator stamps and applies.
- **Retrieval**: hybrid (keyword + vector) with “only user‑confirmed sources”
  filter; any answer is explainable via `kb.why(<id>)`.
- **Secrets**: only opaque `secret_id` references in KB; actual secrets live in
  the OS keystore or an encrypted vault.

### 6) Model broker

- Single service mediates all model calls (local or remote), sets budgets,
  caches, and meters cost.
- **MVP guardrail**: **no streaming** responses in v0 (can be feature‑flagged
  later).

### 7) Security & privacy

- **Capability security** end‑to‑end.
- **Human confirmation** required for outbound side‑effects (sending, deleting,
  spending).
- **Signed bundles/SBOM**; only trusted, signed agents may run
  (policy‑controlled).
- **Redaction**: agents can request redacted/embedded‑only views to avoid
  handling raw PII.

### 8) Observability & debugging

- **agtop** shows per‑agent CPU/RSS/tokens/errors.
- **runloop trace** prints a ladder diagram of a full opening (spans across
  retries).
- Crash quarantine + minimal repro capture.

## Packaging & run modes

Runloop supports **user** and **system** modes with explicit, non‑overlapping
paths.

- **User mode (single user / dev)**
- State: `~/.runloop/`
- Socket: `$XDG_RUNTIME_DIR/runloop/runloopd.sock` (fallback
  `~/.runloop/run/runloopd.sock`)
- Logs: `~/.runloop/logs` (or `stderr` when attached)
- **System mode (daemon)**
- Service user: `runloop`
- State: `/var/lib/runloop/`
- Socket: `/run/runloop/runloopd.sock`
- Logs: journald or `/var/log/runloop/`

## Configuration (v1 schema sketch)

```yaml
# ~/.runloop/config.yaml  (user mode)  |  /etc/runloop/config.yaml (system mode)

runtime:
  base: debian
  agent_container: wasi

models:
  default: local:llama3.1-8b
  broker:
    providers: [local, openai, anthropic]
    budgets:
      default_tokens: 8000
      hard_cap_usd: 0.50
    streaming: false # MVP: off; may be enabled via feature flag later

kb:
  root_dir: "~/.runloop/pog"
  events_db: "events.sqlite"
  view_db: "pog.sqlite"
  vectors_dir: "vectors"
  hashing: blake3
  json_canonicalization: jcs
  timestamps: ms

security:
  confirm_external_actions: true
  secrets:
    provider: os-keyring
    root: "~/.runloop/secrets"
    encryption: none

router:
  fastpath_shell: true
  default_opening: general
  known_commands: []

ui:
  theme: mono
```

## TUI

Single status bar (mode, opening, token burn, health) and toggleable panes:
**plan**, **log**, **artifacts**, **agtop**, **trace**. Keybinds: `space`
(run/pause), `.` (step), `?` (why), `!` (escalate to human).

## Non‑goals for v1

- Graphical desktop/windowing.
- Distributed multi‑host orchestration (beyond local device).
- Model‑streaming UX (guardrailed off in MVP).
- Unscoped network/filesystem access for agents.

## Performance intent

Cold start near process‑launch; low RSS per agent; ≥1k msgs/s bus on a developer
laptop (post‑beta tuning).

## Documentation & decisions

- Docs follow **Diátaxis**, diagrams use **C4 + Mermaid**, and architectural
  decisions are captured as **MADR ADRs**.
- Two baseline ADRs: **0001 Debian + WASM/WASI + SQLite**, **0002 RMP v0
  (header, framing, compatibility)**.
