## Roadmap (12 months to v1.0)

> **Doc status:** Draft — acceptance metrics marked normative. Last updated: 2025‑11‑02.

The plan is milestone‑driven with clear exit criteria. Month counts are from project start (M0 = kickoff). If you want to move faster, you can run some tracks in parallel given staffing.

### Staffing assumptions (minimal viable team)

* **Core**: 1 runtime lead, 1 infra/ops, 2 systems engineers (Rust), 1 ML/LLM engineer, 1 TUI/UX engineer, 1 PM/TPM, 1 security/priv reviewer (part‑time), 1 QA/automation.
* **Optional**: 1 tech writer, 1 developer relations.

Governance & repo hygiene are tracked as Phase G in TODO.md and run in parallel to Phases 0–7.

---

### Phase 0 — Preflight & foundation (M0–M1)

**M0.1 – Charter & constraints**

* Deliverables:

  * Product brief v1, non‑goals, supported platforms (Debian 12, x86_64 + arm64).
  * Public license decision (e.g., Apache‑2.0) and third‑party notice template.
* Exit criteria:

  * Sign‑off on scope, principles, and release targets.

**M0.2 – Architecture & interfaces 0.1**

* Deliverables:

  * RMP 0.1 (message header/body schemas).
  * Agent packaging spec 0.1 (manifest + capabilities).
  * Openings DSL 0.1 (ABNF + examples).
  * POG data model 0.1 (entities + events).
* Exit criteria:

  * RFCs merged; skeletal code scaffolds compile.

**M1.0 – Repo, build, CI**

* Deliverables:

  * Monorepo layout (`/runloopd`, `/sdk/agent-rust`, `/cli`, `/tui`, `/pog`, `/broker`, `/examples`).
  * CI: fmt, clippy, unit tests, cross‑compile, release artifacts.
* Exit criteria:

  * One‑command dev setup, nightly artifacts build.

---

### Phase 1 — Minimal runnable system (M2)

**M2.0 – Runtime & sandbox MVP**

* Deliverables:

  * `runloopd` skeleton: process manager, config loader, capability registry.
  * Wasmtime (WASI) integration; launch a “hello‑agent.wasm”.
  * Capability gate: FS (scoped), Net (off by default), Time, POG read.
* Exit criteria:

  * Run a sample agent from CLI, see logs, controlled failure/exit code.

**M2.1 – CLI & TUI skeleton**

* Deliverables:

  * `rlp` basic commands: `rlp run`, `rlp plan`, `rlp trace <id>`, `rlp cap grant`.
  * TUI with status bar + panes (Plan, Log).
* Exit criteria:

  * Can route a prompt to a static agent via CLI, watch in TUI.

**M2.2 – POG storage 0.1**

* Deliverables:

  * Append‑only event log (SQLite) + materialized view service.
  * Entities: `Identity`, `Account`, `Contact`, `Artifact`, `Event`, `Policy`.
  * `kb.query`, `kb.why`, `kb.write_event` minimal APIs (local sock).
* Exit criteria:

  * Insert `contact.upserted`, read back; `kb.why` shows provenance.

---

### Phase 2 — Protocol, router, and orchestration (M3–M4)

**M3.0 – RMP 0.2 & bus**

* Deliverables:

  * RMP header/body with: `msg_id`, `trace_id`, `opening_id`, `from`, `to`, `type`, `schema_ref`, `ttl`, `deadline`, `budget`, `caps`, `provenance`, `signature`.
  * Local bus (Unix domain sockets), topics, direct addressing, acks.
* Exit criteria:

  * Two agents exchange `Observation` and `Intent` with signed headers.

**M3.1 – Router 0.1 (shell‑first)**

* Deliverables:

  * Fast path for POSIX shell patterns (`pipes`, `globs`) → run as‑is.
  * Classifier for agent/opening routes with explainability.
  * Inline budget annotations (`:budget 30s $0.05`).
* Exit criteria:

  * “`ls | grep foo`” runs locally; “`draft email to john`” routes to opening stub and shows plan.

**M4.0 – Openings orchestrator 0.1**

* Deliverables:

  * DAG execution: fan‑out/in, retries, timeouts, success criteria.
  * Deterministic replay with fixed seeds and snapshot inputs.
* Exit criteria:

  * Example “compose_email” opening runs across 3 agents with replay.

---

### Phase 3 — Model broker, agent SDK, and first agents (M5–M6)

**M5.0 – Model broker 0.1**

* Deliverables:

  * Provider drivers: `local` (e.g., llama.cpp binding), `remote` (HTTP adapter).
  * Caching, metering, policy (max tokens, cost ceilings, temperature bounds).
  * Streaming and function/tool calling surface.
* Exit criteria:

  * Agents request completions/embeds via broker; costs accounted.

**M5.1 – Agent SDK (Rust) 0.1**

* Deliverables:

  * `Agent` trait, message handlers, capability request flow, test harness.
  * Agent packaging/manifest builder.
* Exit criteria:

  * Build/run a sample agent with <100 LOC.

**M6.0 – Core agents (v0)**

* Deliverables:

  * `contact_resolver`, `context_gatherer`, `writer`, `critic`, `mailer` (mailer in “dry‑run” with human confirm).
  * Tool stubs: filesystem read, KB queries, simple template rendering.
* Exit criteria:

  * “Draft email to John” demo: plan → draft → review → (confirm) → send (dry‑run).

---

### Phase 4 — Knowledge base deepening & observability (M7)

**M7.0 – POG 0.2**

* Deliverables:

  * Vector index (HNSW) + hybrid search (keyword + vector).
  * Secrets integration (OS keyring or `age` vault) via opaque `secret_id`.
  * Policies: retention windows, redaction, scoped views (shadow KB).
* Exit criteria:

  * Agents retrieve context via hybrid search with provenance filters; secrets never appear in logs.

**M7.1 – Observability 0.1**

* Deliverables:

  * `agtop` pane: per‑agent CPU/RSS, tokens in/out, error rate, cache hits.
  * Tracing with ladder diagram in `rlp trace`.
  * Cost accounting by opening/agent/provider.
  * Performance lab harness capturing cold/warm startups, message latency, and memory RSS against acceptance budgets.
* Exit criteria:

  * Run 100 agents (synthetic); watch resources, traces, and costs update live.

---

### Phase 5 — Safety, scheduling, and self‑improvement (M8–M9)

**M8.0 – Scheduler & pressure controls**

* Deliverables:

  * Fair‑share per opening; cgroups for CPU/mem/io; soft throttles under pressure.
  * Circuit breakers for flapping agents; quarantine on sandbox crash.
* Exit criteria:

  * Stress test maintains interactivity; bad agents quarantined automatically.

**M8.1 – Safety 0.1**

* Deliverables:

  * Capability tokens with expirations; human confirmation for external side effects (send/delete/spend).
  * “Tripwires” (e.g., outbound network bursts, data exfil heuristics).
* Exit criteria:

  * Attempted disallowed actions blocked with human‑readable reasons.

**M9.0 – Self‑improvement harness 0.1**

* Deliverables:

  * Trace capture → clustering → patch proposals (prompts/policies) → sandbox A/B → adoption rules.
  * Golden task suite with regressions dashboard.
* Exit criteria:

  * System proposes and validates at least one improvement without human prompt engineering.

---

### Phase 6 — Beta hardening (M10)

**M10.0 – Beta (0.9)**

* Deliverables:

  * Installers for Debian/Ubuntu (.deb), codesigned binaries.
  * Docs: Quickstart, SDK guide, Opening cookbook, Security whitepaper.
  * Telemetry opt‑in (anonymous) for stability metrics.
  * Reliability/performance dashboard tracking acceptance metrics (cold/warm start, message latency, RSS).
* Exit criteria (beta gate):

  * 24h soak with zero critical crashes; 3 reference openings reproducible; successful upgrade/downgrade.

---

### Phase 7 — Release candidate & 1.0 (M11–M12)

**M11.0 – RC (0.99)**

* Deliverables:

  * Backward‑compatible RMP/DSL finalized; migration scripts.
  * API freeze; integration tests with fault injection.
* Exit criteria:

  * No known P0/P1 defects; perf & memory targets met (see below).

**M12.0 – v1.0**

* Deliverables:

  * Stable 1.0 release notes, long‑term support policy, deprecation policy.
* Exit criteria:

  * All acceptance metrics green; docs complete; supply chain attestation published.

---

## Acceptance metrics & performance budgets

* **Agent startup (cold)**: ≤ 25 ms p50, ≤ 60 ms p99 (fresh WASM instantiation).
* **Agent activation (warm)**: ≤ 5 ms p50, ≤ 15 ms p99 (pre‑warmed instance).
* **Per‑message overhead (in‑host)**: ≤ 0.5 ms p50, ≤ 2 ms p99.
* **Idle memory (unique RSS)**: ≤ 8 MB per agent p50; ≤ 800 MB total for 100 concurrent idle agents.
* **Router latency**: shell classification < 3 ms; agent vs opening decision < 60 ms (with model).
* **Openings replay determinism**: ≥ 99% identical outputs given same seeds/inputs.
* **POG query p50**: < 20 ms for indexed queries; hybrid search < 150 ms p50.
* **Crash‑free sessions**: > 99.5% over 24h soak.
* **Security**: zero known remote code exec from agent boundary; all outbound actions require explicit caps; secrets leakage = 0 in logs.

> **Measurement notes:** “Cold” implies no cached WASM instance and no warmed file cache. “Warm” assumes a pre‑instantiated module in the pool. Memory figures are unique RSS per agent; shared pages (e.g., libc) are accounted separately.

---

## Detailed specs (ready to implement)

### 1) RMP (Runloop Message Protocol) 0.2 (sketch)

**Header (JSON, msgpack on wire)**

```json
{
  "v": "0.2",
  "msg_id": "uuidv7",
  "trace_id": "uuidv7",
  "opening_id": "uuidv7",
  "from": "agent:contact_resolver@1.0.3",
  "to": "agent:writer@0.5.1",
  "type": "Intent|Observation|ToolCall|Artifact|Critique|StateDelta|Ack|Error|Control",
  "schema_ref": "sha256:...",
  "content_type": "application/json",
  "ttl_ms": 60000,
  "deadline_unix_ms": 1730549000000,
  "budget": { "tokens": 8000, "usd": 0.05, "wall_ms": 30000 },
  "caps": { "kb_read": ["contacts"], "broker_call": ["model"] },
  "provenance": {
    "model": "local:llama3.1-8b",
    "provider": "local",
    "parameters": { "temperature": 0.2, "top_p": 0.9, "seed": 42 },
    "tooling": ["kb", "fs.read"]
  },
  "qos": "durable|ephemeral",
  "sig": "ed25519:base64..."
}
```

| Field               | Status       | Notes                                                   |
| ------------------- | ------------ | ------------------------------------------------------- |
| `msg_id`, `trace_id`, `opening_id`, `from`, `to`, `type`, `schema_ref` | **Required** | Always present; UUIDv7 or equivalent monotonic IDs.    |
| `ttl_ms`, `budget`, `caps`, `provenance`               | **Recommended** | Required for orchestrated openings; optional for shell fast path. |
| `content_type`, `deadline_unix_ms`, `qos`, `sig`       | **Optional**    | Enable as the deployment needs delivery guarantees or signatures. |
| Experimental fields                                 | **Experimental** | Introduced via ADRs; guard behind feature flags.        |

**Body**

* Arbitrary bytes; must validate against `schema_ref`.
* Large artifacts sent via content‑addressed blobs; message body carries pointer.

### 2) Agent package format (TOML)

`agent.toml`

```toml
id = "contact_resolver"
version = "0.1.0"
entry = "bin/contact_resolver.wasm"
runtime = "wasm32-wasi"

[requires]
kb_read = ["contacts"]
kb_search = true
broker = ["embed"]

topics = ["contacts.resolve", "contacts.suggest"]
limits = { cpu = "50ms/s", mem_mb = 64, wall_ms = 5000, msgs_per_sec = 50 }
```

### 3) Openings DSL 0.1 (ABNF snippet)

```
opening     = "opening" WSP string WSP "{" *(stmt) "}"
stmt        = goals / node / edge / policy
goals       = "goals:" WSP "[" *(string *("," string)) "]"
node        = ident WSP ":=" WSP "agent(" string ["," param *("," param) ] ")"
edge        = ident "." ident WSP "->" WSP ident "." ident
policy      = "budget" WSP ":" WSP number "s" [ "," "$" number ]
```

Example opening:

```text
opening "compose_email" {
  goals: ["Email John about Q4"]
  contacts := agent("contact_resolver")
  context  := agent("context_gatherer")
  draft    := agent("writer", model="local:llama3.1-8b")
  review   := agent("critic")
  send     := agent("mailer", confirm=true)

  contacts.out -> draft.recipients
  context.out  -> draft.context
  draft.out    -> review.in
  review.ok    -> send.in
  budget: 30s, $0.05
}
```

### 4) POG (Personal Operations Graph) data model

**Event (append‑only)**

```sql
CREATE TABLE events (
  id TEXT PRIMARY KEY,                -- uuidv7
  ts INTEGER NOT NULL,                -- unix ms
  actor TEXT NOT NULL,                -- agent id or "user"
  kind TEXT NOT NULL,                 -- e.g., "contact.upserted"
  scope TEXT NOT NULL,                -- "personal|workspace:<id>|system"
  payload_json TEXT NOT NULL,         -- validated against schema
  provenance_json TEXT NOT NULL,      -- model/provider/tool versions
  prev TEXT,                          -- causal link if any
  hash TEXT NOT NULL                  -- content hash
);
```

**Materialized views (examples)**

```sql
CREATE TABLE contacts (
  id TEXT PRIMARY KEY,
  name TEXT,
  email TEXT,
  org TEXT,
  tags TEXT,              -- JSON array
  trust REAL,             -- 0..1
  source_event TEXT,      -- link to events.id
  created_ts INTEGER,
  updated_ts INTEGER
);

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  kind TEXT,              -- "github", "gmail", ...
  handle TEXT,
  auth_ref TEXT,          -- opaque secret_id; not raw secret
  scopes TEXT,            -- JSON array
  status TEXT,            -- "linked|revoked"
  updated_ts INTEGER
);

CREATE TABLE artifacts (
  id TEXT PRIMARY KEY,
  kind TEXT,              -- "note", "file", "draft"
  uri TEXT,               -- "blob:sha256:..."
  content_hash TEXT,
  title TEXT,
  created_by TEXT,
  created_ts INTEGER
);
```

**API sketch**

```rust
struct KBQuery;
impl KBQuery {
  fn query(&self, sql: &str, params: &[Value]) -> Result<RowSet>;
  fn search(&self, text: &str, k: usize, filters: &[Filter]) -> Result<Vec<SearchHit>>;
  fn why(&self, id: &str) -> Result<Vec<Event>>; // provenance chain
  fn write_event(&self, ev: Event) -> Result<()>;
}
```

---

## Terminal UX (TUI)

* **Status bar**: mode (Run/Paused), active opening, tokens/s, cost, system pressure indicator.
* **Panes**:

  * **Plan** (DAG as list): current node highlighted; step/run; show retries/timeouts.
  * **Log**: structured events from router/agents; filter by `trace_id`.
  * **Artifacts**: drafts/files; open in `$EDITOR` via temp path.
  * **agtop**: RSS/CPU/tokens/error‑rate per agent.
  * **Trace**: ladder diagram (agent ↔ agent) per `trace_id`.
* **Keys**: `Space` (run/pause), `.` (step), `?` (why routing), `!` (escalate to human), `/` (filter), `q` (quit).

---

## Security & privacy (v1)

* **Capabilities**: explicit allowlist per agent/opening; tokens have TTL & audit trails.
* **Secrets**: only `secret_id` references stored in POG; secrets live in OS keyring or `age` vault.
* **Outbound side‑effects**: require `confirm_external_actions=true` unless policy explicitly permits.
* **Tripwires**: outbound network volume, large PII patterns; hard block + notify.
* **Provenance**: every response references `events` enabling `kb.why`.
* **Supply chain**: SBOM generation; signature verification on agent packages; reproducible builds tracked for 1.0.

---

## Observability & SLOs

* **Metrics** (prom‑style names):

  * `runloop_agent_rss_bytes`, `runloop_agent_cpu_seconds_total`,
  * `runloop_llm_tokens_in_total`, `runloop_llm_tokens_out_total`,
  * `runloop_message_latency_ms`, `runloop_opening_success_total`,
  * `runloop_opening_cost_usd_total`, `runloop_agent_errors_total`.
* **Tracing**: `trace_id`, spans for each crossing; ladder diagram diff‑view in replay.
* **Logs**: jsonl with `level`, `trace_id`, `opening_id`, `agent_id`, `msg`.

---

## QA & testing strategy

* **Unit**: protocol parsers, capability checks, POG schema validators.
* **Property‑based**: message routing, DAG scheduler invariants (no deadlocks).
* **Sandbox fuzzing**: malformed WASM modules, syscalls denied.
* **Perf**: synthetic 100–500 agent runs; memory & latency budgets enforced.
* **Chaos**: agent crash, slow broker, disk full, network flap.
* **Replay**: golden traces for critical openings; bit‑for‑bit output (where deterministic).
* **Security**: cap‑bypass attempts, secrets leakage scans in logs, SBOM integrity.

---

## Risks & mitigations

* **Model drift/cost sprawl** → budgets per opening; local‑first models; cost dashboards.
* **Sandbox breakout** → WASM/WASI hardened, seccomp for host helpers, minimal syscalls.
* **POG PII leakage** → shadow views; redaction policies; default off for telemetry.
* **Protocol churn** → versioned schemas; migration tooling; compatibility tests.
* **Performance under load** → pressure‑based throttling, back‑pressure on router, caching at broker.

---

## Release packaging & docs

* **Artifacts**: `.deb` for Debian/Ubuntu; tarballs; checksums & signatures.
* **Docs**: Quickstart, TUI cheat‑sheet, SDK guide, Opening cookbook, Security model, Migration guide.
* **Examples**: “Compose Email,” “Summarize Folder,” “Weekly Report,” “Logbook Extract.”

---

## Backlog (first 90 days, issue‑sized)

* **Runtime**: Wasmtime embed; capability gate MVP; agent supervisor.
* **Protocol**: RMP header parser/validator; msgpack serialization; sig verify (ed25519).
* **POG**: events table; contacts/accounts/artifacts views; `kb.why`.
* **Router**: shell fast‑path; classifier; human‑readable route explanation.
* **Openings**: parser; DAG executor; steps/retry/timeouts; replay snapshotter.
* **Broker**: local provider adapter; token meter; prompt template engine; stream API.
* **Agents**: contact_resolver/context_gatherer/writer/critic/mailer (dry‑run).
* **TUI**: panes; `agtop`; ladder trace; keybindings; config UX.
* **Observability**: tracing; metrics; json logs; perf harness.
* **Security**: secrets via OS keyring; confirm external actions; capability tokens.

---

## Example “Definition of Done” per milestone

* **Spec milestone**: RFC merged; example payloads validated; round‑trip tests.
* **Runtime milestone**: feature flag on; integration tests green; perf budget met in CI.
* **Agent milestone**: packaged + signed; runs in sandbox; caps enforced; example opening passes.
* **POG milestone**: schema migration script; backfill verified; `kb.why` returns chain.
* **Release milestone**: installer works on clean VM; docs updated; checksums/signatures published.

---

## What “Beta” looks like (M10)

* Installable on Debian/Ubuntu; CLI/TUI stable.
* At least **3 reference Openings**:

  1. **Compose Email** (contacts + context + draft + critic + confirm).
  2. **Weekly Summary** (scan artifacts/calendar, draft report).
  3. **Folder Insight** (ingest directory, tag contacts/artifacts, produce notes).
* Self‑improvement suggests and validates at least one policy/prompt change weekly.
* 24h soak test with >99.5% stability and perf within budgets.

---

### Long‑term (post‑1.0) teasers

* **Multi‑node Runloop**: remote execution and data locality for openings.
* **Redox port**: microkernel isolation and capability alignment.
* **Typed tool ecosystem**: richer tool catalog and signing.
* **Workspace mode**: team‑scoped POG with fine‑grained sharing.
