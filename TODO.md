# TODO Backlog

> **MVP scope (explicit):**
>
> - Single‑machine, single‑user.
> - Debian host, terminal‑only.
> - Agents run as **WASM/WASI** tasks under Wasmtime with **capabilities**.
> - Message bus (local UDS), **RMP** (Runloop Message Protocol) v0.
> - Knowledge Base (**Pog**) with SQLite **event log + materialized views**;
>   **FTS** optional; embeddings **postponed** (semantic search can be mocked).
> - Router v0: shell fast‑path + heuristic route to a single opening.
> - Opening engine v0: **DAG**, retries, timeouts, **replay**.
> - Canonical opening: **“compose_email”** (contact_resolver → context_gatherer
>   → writer → critic → mailer). Mailer does **dry‑run** (prints), human confirm
>   required.
> - Observability v0: `agtop` (cpu/mem/tokens), `trace` ladder text, audit log
>   for caps.
> - Packaging: `.deb` + live‑build ISO (dev grade).

---

## Epic A — Core types, config, errors

### A1. Core crate for shared types

- [x] Path: `crates/core/` (add to workspace).
- [x] Define error enum (thin, `thiserror`):
      `Error::{Io, Config, Bus, Rmp, Runtime, Kb, Broker, Router, Opening, CapDenied, Timeout, BudgetExceeded}`.
- [x] Define core IDs: `AgentId`, `OpeningId`, `TraceId`, `EventId`.
- [x] Define **content types** registry constants (u16): `CT_OBSERVATION=1`,
      `CT_INTENT=2`, `CT_TOOL_CALL=3`, `CT_TOOL_RESULT=4`, `CT_ARTIFACT=5`,
      `CT_CRITIQUE=6`, `CT_STATE_DELTA=7`.

**DoD:** All types compile; docs comment each type’s purpose; used by other
crates.

### A2. Config loader

- [x] Path: `crates/core/src/config.rs`.
- [x] Structure mirrors `~/.runloop/config.yaml` (runtime, models, kb, security,
      ui, bus).
- [x] Implement: `Config::load()` (env var override path), `Config::validate()`
      (required fields, sane ranges).
- [x] Add `runtime.socket_path` (wins over `sockets_dir`); user-mode defaults to
      `$XDG_RUNTIME_DIR/runloop/rmp.sock` (fallback `~/.runloop/sock/rmp.sock`);
      deprecate `~/.runloop/run` with warning.
- [x] Long-lived KB aliases: `kb.ledger` → `kb.events_db`, `kb.materialized` →
      `kb.view_db` (warn on use).

**DoD:** A unit test loads a sample `tests/fixtures/config.yaml` and validates
defaults.

---

## Epic B — RMP protocol & local bus

### B1. RMP header & frame codec

- [x] Path: `crates/rmp/`.
- [x] Define fixed 64-byte header with `magic="RMP0"`, `header_version=0`,
      `header_len=64`, zeroed `flags`/reserved words, `schema_id`, `reserved2`,
      `body_len`, `created_at_ms`, `ttl_ms`, `trace_id`, `msg_id`, `reserved4`.
- [x] Implement `{type, payload}` MsgPack envelope helpers + encode/decode APIs.
- [x] Parse/validate TTL, expose helpers for `expires_at_ms`, duplicate keys,
      and enforce schema ↔ body kind cross-checks.

**DoD:** Round‑trip test over a `tokio::io::duplex()` stream; fuzz decoder
(corpus with corrupt lengths).

### B2. Bus abstraction (UDS)

- [x] Path: `crates/bus/`.
- [x] Public API: `Bus::bind(path)`, `Bus::connect(path)`,
      `Bus::publish(topic,msg)`, `Bus::subscribe(topic) -> Stream<Message>`,
      `Bus::send(dest,msg)`.
- [x] Implement back-pressure: bounded channel; drop policy = block writer with
      timeout, emit metric.
- [x] **Idempotency**: include `(trace_id,msg_id)` in a dedupe cache (LRU) on
      receivers, ignore dups.
- [x] TTL enforcement: drop messages beyond `ttl_ms`.
- [x] Surface drop counters and broadcast notifications on `rlp/sys/drops`.
- [x] ACL: only UI/TUI may publish `action.decision`; bus rejects other
      publishers (Forbidden) and tests cover both reject/allow paths.
- [x] Wire ACL to config (`bus.auth.publishers.action_decision.allowed_kinds`)
      in daemon initialization.

**DoD:** Throughput test ≥ **600 msgs/s** loopback; TTL respected; duplicate
injection ignored; drop counters observable via API/topic.

### B3. Trace propagation

- [x] Ensure `trace_id` persists end-to-end; add helper to spawn child spans
      with same id.

**DoD:** `runloop trace` (later) shows consistent IDs through 3 hops in an
integration test.

---

## Epic C — WASM runtime & agent container

### C1. Wasmtime integration

- [x] Path: `crates/runtime/`.
- [x] Initialize `Engine`, `Store`, `Linker`.
- [x] Pre‑load allowed WASI functions; disable everything else by default.

**DoD:** Load a trivial WASM and call `_start` without panic; process exits
cleanly. _(Met via `tests/smoke.rs`.)_

### C2. Capabilities model

- [x] Define `Caps` struct (fs allowlist, net allowlist, time, kb_read/write,
      model, secrets, exec).
- [x] Manifest parser: read `policy.caps` (TOML) → `Caps` + overrides.
- [x] Implement **hostcalls** that check caps before performing:
  - FS: open/read/write within allowed roots; symlink traversal blocked outside
    root.
  - NET: only HTTP(S) to allowed domains (DNS resolution gate).
  - TIME: allow monotonic/now only if cap set.
  - KB: map to `kb` client (see Epic E).
  - MODEL: map to broker (Epic F).
  - SECRETS: return opaque `secret_id` values only.

- [x] Record **audit events** for denied attempts (currently buffered in-memory;
      hook KB once available). _(See
      `tests/capabilities.rs::time_capability_denied_records_audit`.)_
- [x] Add integration coverage that the WASI preopen list reflects `Caps::fs`
      (DirPerms/FilePerms mapping and missing roots).
  - Host-side plan coverage: `tests/preopen_introspection.rs`.
  - Behavioral harness: `tests/preopen_harness.rs` exercises read/write
    enforcement.

**DoD:** Attempting a forbidden operation yields `CapDenied` and writes an audit
event to KB.

### C3. Agent lifecycle

- [x] API: `spawn(AgentSpec) -> AgentHandle`, `send(AgentId, Message)`,
      `stats(AgentId)`, `kill(AgentId)`.
- [x] Implement stdout/stderr ring buffer per agent (for TUI log).
- [x] Track RSS/CPU via `/proc` (best‑effort) and expose via `stats` (Linux
      default `procfs`, opt-out via `no-procfs`).
- [x] Wire mailbox delivery to bus routing (add `mailbox_peek_meta` for header
      introspection).
- [x] Measure cold-start perf (50 idle agents, p50 < 40 ms) and document harness
      in `docs/perf.md`.

**DoD:** Spawn 50 idle agents; p50 cold-start < **40 ms**; stats show non-zero
RSS.

### C4. Canonical agent bundles (WASM)

- [x] Replace the temporary native shims in `agents/*/bin/` with
      wasm32-wasip1 artifacts built from the `crates/agents-wasm/*` crates
      (`just build-agents-wasm`).
- [x] Extend the build to copy the compiled `.wasm` files into each agent bundle
      alongside refreshed BLAKE3 digests (`scripts/build_agents_wasm.sh`).
- [x] Wire the runtime/executor so `runloopd` launches the wasm agents with the
      declared `policy.caps`, eliminating the in-process fallbacks. Coverage:
      `scripts/test_agents_wasm.sh` + `crates/executor-local/tests/compose_email.rs`.

**DoD:** `rlp run examples/openings/compose_email.yaml` succeeds via the runtime
with wasm agents and no Python dependencies. ✅

---

## Epic D — Router v0 (shell vs agent)

### D1. Shell fast‑path

- [x] Path: `crates/rlp/` (CLI) + `crates/router/` (logic).
- [x] Implement simple classifier: if input parses as POSIX pipeline/redirection
      (`|`, `>`, `<`, `;`, `&&`, `||`) or `^[a-zA-Z0-9_\-./]+(\s+.+)?$` **and**
      known shell builtins/commands present → `Shell`.
- [x] Else route to configured opening (`compose_email` by default).

**DoD:** Corpus of 100 prompts (50 shell, 50 agentable) → ≥ **98%** correct.
(See `crates/router/tests/corpus/router_prompts.csv` + `classifier_corpus`
test.)

### D2. Explainability

- [x] `rlp why "<prompt>"` prints features used and matched rule.

**DoD:** Unit test shows stable string with rule id.
(`crates/router/src/classifier.rs` tests + CLI integration.)

---

## Epic E — Knowledge Base (Pog) v0

### E1. Event log schema

- [x] Path: `crates/kb/`.
- [x] SQLite file `events.sqlite` (+ WAL): table
      `events(id, ts_ms, actor, kind, payload_json, provenance_json, scope, hash_blake3 BLOB(32))`.
- [x] Payloads stored as **JCS canonical JSON TEXT**; compute `hash_blake3` over
      `{kind,actor,scope,payload,provenance}` and enforce uniqueness.
- [x] Migration scaffold: version table, `migrate_up()`.

**DoD:** Insert/read event; duplicate hash rejected; migrations apply
idempotently. ✅

### E2. Materialized views

- [x] Separate `pog.sqlite`: tables `contacts`, `accounts`, `artifacts`, `runs`.
- [x] Materializer service reads new events and updates views (+ indexes).
- [x] `kb.why(<entity_id>)` returns ordered source events.

**DoD:** `contact.upserted` creates/updates a row; `why` returns the upserting
event id. ✅

### E3. API layer

- [x] `propose(StateDelta) -> EventId` (validator checks schema & caps).
- [x] `query(sql) -> rows` (read‑only).
- [x] `search(keyword) -> rows` (FTS optional; if absent, LIKE fallback).
- [x] Validation rules: referential integrity (evidence events exist), scope
      rules, provenance fill.

**DoD:** Invalid deltas rejected with reason; tests for each rule. ✅

### E4. Seed schemas

- [x] `contact.upserted {name,email,org,trust,evidence[]}`
- [x] `artifact.created {kind,path,sha256,summary}`
- [x] `email.sent {to[],cc[],subject,artifact_id}`
- [x] `run.started/finished {opening_id,status}`

**DoD:** JSON schemas exist in `docs/kb-schemas.md` and are enforced. ✅

---

## Epic F — Model Broker v0

### F1. Broker interface

- [x] Path: `crates/model-broker/`.
- [x] `complete(ModelRequest) -> ModelOutput` with `budget_tokens`, `timeout`,
      `cache_key`.
- [x] Providers: `NullProvider` (echo), `HttpProvider` (configurable base URL +
      `secret_id`).

**DoD:** Budget/timeout enforced; `NullProvider` available for offline tests;
`stream=true` returns a deterministic `StreamingUnsupported` error until the
Phase-3 feature flag lands. ✅

### F2. Simple cache

- [x] LRU in‑memory keyed by `(model,prompt,params)`; TTL configurable.

**DoD:** Cache hit metric increments; unit test hits after warm. ✅

---

## Epic G — Opening engine v0

### G1. DSL parser

- [x] Path: `crates/openings/`.
- [x] YAML→IR: nodes (name,use,with), edges (from,to), policy
      (budget,timeout,confirm_external).
- [x] Templating: `{{params.foo}}` expansion only (no loops/logic).

**DoD:** Parse `examples/openings/compose_email.yaml` into IR; validation errors
include line/col. ✅

### G2. Runner & scheduler

- [x] Topological execution; fan‑in waits; fan‑out sends clones of artifacts.
- [x] Retries with backoff from policy; per‑node timeout; propagate failure with
      reason.
- [x] Pass **Artifacts** and simple scalars between nodes.

**DoD:** End‑to‑end run with recorded per‑node status. ✅

### G3. Replay

- [x] Record inputs/outputs per node and output hashes; `rlp replay` re‑feeds
      inputs to produce same outputs (when providers deterministic).
- [x] Diff tool: show mismatches (if any).

**DoD:** Replay of a deterministic run matches outputs hash. ✅

### Follow-ups (Epic G)

- [ ] Integrate Runner with `runloopd`/bus executor so node work goes through
      real agents instead of the local stub. _(Bus executor + dispatcher landed;
      remaining work: launch actual WASM/shim agents rather than proxying
      through the LocalExecutor.)_
- [x] Persist run/replay traces in the knowledge base (`run.*` / `node.*`
      events) once schemas are ready.

---

## Epic H — Canonical agents (MVP set)

> All agents are WASM bundles with `manifest.toml`, `policy.caps`, `README.md`.
> For MVP you can implement them as _native processes_ first (behind a host
> “shim”) to validate flow, then convert to WASM—**but** the runtime and caps
> checks must be in place.

### H0. native_agent_shim

- [x] `runloop-sdk` crate exposes capability parser, handshake payloads, and bus
      helpers.
- [x] `agent-shim` binary loads `RUNLOOP_*` env, publishes `agent.hello`, then
      launches the native process under the enforced caps.
- [x] Add README/docs so agents know the env contract; add publish/subscribe
      integration test in `runloop-sdk`.

### H1. contact_resolver

- [x] Inputs: `recipient_query` (string).
- [x] Action: `kb.query` for contact; if none, create stub with low trust and
      request human confirm.
- [x] Outputs: `ResolvedContact {name,email,confidence,contact_id}`.

**DoD:** Given seeded KB with “John [john@acme.com](mailto:john@acme.com)”,
returns correct email, confidence ≥ 0.8.

### H2. context_gatherer

- [x] Inputs: `topic`, `contact_id`.
- [x] Action: fetch recent artifacts tagged with contact/topic; summarize via
      broker or simple heuristic (fallback).
- [x] Outputs: `ContextBundle {bullets[], citations[event_id[]]}`.

**DoD:** Returns ≥1 bullet and citations referencing KB events.

- [x] Harden the LIKE predicate (lowercase `payload_json`, escape `%`/`_`) so
      topic/contact filters behave case-insensitively without accidental
      wildcard injection.

### H3. writer

- [x] Inputs: `recipient, topic, context`.
- [x] Action: prompt template; call broker; generate `Artifact(draft_email.md)`.
- [x] Outputs: `Draft {artifact_id, rationale}`.

**DoD:** Draft ≤ 180 words; artifact recorded; rationale present.

### H4. critic

- [x] Inputs: `Draft`.
- [x] Action: check for tone/length; if failing, propose edits; set
      `ok:boolean`.
- [x] Outputs: `Review {ok, notes}`.

**DoD:** For a too‑long draft, sets `ok=false` and suggests trimming.

### H5. mailer (dry-run)

- [x] Inputs: `Draft`, `ResolvedContact`, `Review`.
- [x] Action: if `review.ok` → **require human confirm**; on confirm, record
      `email.sent` with `artifact_id`, print to stdout (no network send).
- [x] Outputs: `MailResult {status:'dry-run', recipients[]}`.

**DoD:** Confirmation gate works; KB has `email.sent` event referencing the
draft artifact.

---

## Epic I — CLI (`rlp`) & TUI (`agtop`)

### I1. CLI surface

- [x] `rlp run openings/compose_email.yaml --params '{...}'`
- [x] `rlp run` submits to `runloopd` via UDS and streams `RunEvent`s. _CLI now
      implements control-plane submission over `rlp/ctrl` (30s TTL, idempotent
      by `trace_id == request_id`), waits up to 2s for `RunAccepted`, then
      subscribes to `rlp/runs/<trace_id>/events` and renders NDJSON. No
      auto‑fallback; when unreachable it fails with guidance to start the daemon
      or use `--local`. Daemon must handle `CT_CTRL_REQ` and publish
      `CT_RUN_EVENT`; KB persistence of run/node events is owned by the daemon._
- [x] `rlp why "<prompt>"` (table output by default, `--json` flag shared with
      other subcommands). _Shared renderer now enforces table-by-default when
      TTY; honors `--json/--table`/`--max-_`.
- [x] `rlp replay <trace_id>` (reads stored traces from KB; still accepts
      `<trace.json>` for dev).
- [x] `rlp kb query "<sql>"` (table default + `--json`).
- [x] `rlp kb why <entity_id>` (ditto formatting and provenance view). _Outputs
      ladder table with `--resolve` stub TBD._
- [x] `rlp config path` / `rlp config path --all` (highest-precedence file +
      provenance list). _Command implemented with layered table + JSON export._

**DoD:** Commands print structured output (table or JSON); exit codes sensible.

### Follow-ups (Epic I)

- [x] `rlp run`: surface invalid agent param types (validated against agent
      `manifest.toml` schemas; openings may carry temporary hints).
- [x] `rlp run`: `--trace-out` writes the daemon-provided trace
      (side-effect-free replayer). _CLI now fetches the canonical `run.trace`
      from the KB after daemon-backed runs so traces are saved consistently in
      either mode._
- [x] `rlp agent scaffold` interactive wizard that walks through provider
      selection (model broker route + secrets), capability grants (fs/net/kb),
      optional tool attachments (`tools.json`, see `docs/tool-attachments.md`),
      and emits a new `crates/agents-wasm/<name>`
      crate plus `agents/<name>/manifest.toml`, `policy.caps`, and a starter
      opening YAML (current nodes/edges DSL). Include prompts for available
      openings/structures so users can extend DSL plans without manual file
      edits; document generated artifacts in `docs/`.

### I2. TUI monitor (`agtop`)

- [x] Status bar (mode, opening name + trace, active pane, token/health summary,
      confirm badge).
- [x] Panes: **Log** (streaming), **Plan** (table with node statuses + dep
      counts), **agtop** (system + per-agent metrics), **Trace** (ladder text). Navigation keys: `Tab`, `Shift+Tab`, `q`, `?`, `/`, `.`, `!`.
- [x] Subscribes to `rlp/runs/<trace_id>/events` (unified stream) plus
      `rlp/sys/metrics`; per-agent metrics via `rlp/agents/<agent>/metrics`
      when provided with `--monitor-agents` (auto-discovery TBD).
- [x] Toggle confirm dialogs for external actions via bus (`action.proposal` →
      `action.decision`). Decisions publish `CT_ACTION_DECISION` with proposal
      correlation.

> Follow-ups: auto-discover agent metrics; richer DAG/edge rendering beyond the
current table; surface tokens/costs once metrics payload includes them.

**DoD:** While running the opening, panes update live; switching panes does not
freeze updates.

---

## Epic J — Observability, audit, metrics

### J1. Tracing

- [x] Use `tracing` crate; span per crossing; include `trace_id`, `opening_id`,
      `agent_id`.
- [x] `runloop trace <id>` prints ladder: timestamps, sender→receiver, type,
      bytes.

**DoD:** Run a composed opening and print its ladder with ≥5 steps.

### J2. Metrics

- [x] Counters: msgs sent/received, drops, cap_denied, broker_calls, cache_hits,
      tokens_prompt/completion (monotonic).
- [x] Gauges: agents_running, rss_total, bus_queue_depth_max/capacity_max,
      per-agent mailbox_depth/rss/cpu.

**DoD:** `agtop` shows these metrics; unit tests increment expected counters.
      CT_METRICS_SNAPSHOT v1 published every `observability.metrics_interval_ms`
      with TTL = 2× interval; per-agent final snapshot on teardown.

### J3. Audit log (caps)

- [x] KB event `cap.audit {agent,cap,args_hash,decision}` on deny and
      (optionally) allow.
- [x] Config toggle to limit volume.

**DoD:** Attempted forbidden FS write produces an audit event.

---

## Epic K — Security & privacy

### K1. Capability enforcement completeness

- [x] Ensure **every** hostcall mapping checks caps (fs, net, time, kb, model,
      secrets, exec).
- [x] Deny by default; empty caps = inert agent (launch still permitted, emits
      `caps.empty` audit on start; hostcalls remain denied).

**DoD:** Static audit (grep/inspection) & tests verify enforcement paths.

### K2. Secrets handling

- [ ] `secret_id` indirection only; no raw secrets in KB/events. Hostcall can
      emit opaque handles when `expose_raw_secrets=false`; default still returns
      raw for compatibility. Remaining: shared redactor across broker/KB/log
      sinks and age backend encryption.
- [ ] Stub “keyring” provider that returns opaque tokens (no real secrets for
      MVP). Providers wired: stub/env, secret-service (stub), pass, age
      plaintext; add real secret-service/age encryption, CLI tooling, and
      finish auto-probe coverage.
- [x] Fail agent launch when declared secrets are absent unless an explicit
      dev override is set; avoid silently returning the ID itself.

**DoD:** Search repo for “api_key” yields no values; unit tests pass with fake
ids.

### K3. Redaction

- [ ] KB returns redacted views for agents without `kb_read.contacts_raw`
      (example: embeddings or masked emails); for MVP, you can implement a
      simple mask `j***@acme.com`.

**DoD:** Agent without cap cannot read full email values.

---

## Epic L — Packaging & runnable artifacts

### L1. Debian packages (dev grade)

- [ ] Use `cargo-deb` for `runloopd`, `rlp`, `agtop`.
- [ ] Install `runloopd.service` (enable but **do not start automatically** in
      dev package).
- [ ] Postinst: create system user `runloop:runloop`, own `/var/lib/runloop`,
      and place the bus socket under `/run/runloop/rmp.sock`.

**DoD:** `dpkg -i` installs binaries; `systemctl enable --now runloopd` starts
cleanly.

### L2. Live ISO (dev grade)

- [ ] `packaging/live-build` hooks to copy `.deb`s and enable `runloopd`.
- [ ] TTY autologin for easy TUI demo.

**DoD:** ISO boots in QEMU; `rlp run ...` works; `agtop` renders.

---

## Epic M — Golden tasks & regression harness

### M1. Golden corpus


- [x] `tests/golden/compose_email/inputs.json` variants (recipient known/unknown, long/short topics).

- [x] Expected outputs (properties, not exact strings): recipient email equals, word count range, citations present.


**DoD:** `cargo test --package runloop-executor-local --test golden -- --ignored` runs opening end‑to‑end (with `NullProvider`) and checks properties.

### M2. Router corpus

- [ ] 50 shell prompts, 50 agent prompts; store as CSV with expected route.

**DoD:** Router unit test ≥ **98%** accuracy.

### M3. Replay fidelity

- [ ] Record a deterministic trace; ensure `rlp replay` reproduces identical
      artifacts (hash).

**DoD:** Hash match asserted.

---

## Epic N — Documentation (implementation‑level)

### N1. Docs that match what you built

- [x] Update `docs/architecture.md` with **current** component boundaries.
- [x] Add a “Prompt routing & shell integration” section
      (`docs/router-shell.md`) describing how interactive prompts flow through
      the router, how shell hooks work, and how to disable them.
- [ ] `docs/message-protocol.md`: fill header fields table; example frame.
- [ ] `docs/kb-schemas.md`: list MVP event kinds with fields.
- [ ] `docs/openings-dsl.md`: grammar + `compose_email` example.
- [ ] `docs/tui.md`: pane screenshots (ASCII ok).
- [ ] `docs/ops.md`: how to run dev packages and ISO.

**DoD:** README links render; no TODO placeholders remain for MVP sections.

---

## Epic O — Prompt routing & shell integration

> Goal: after Runloop installation (and opt‑in), interactive terminal prompts
> are classified by the router and either passed to the POSIX shell fast‑path or
> routed to openings, without per‑command user wiring.

**O1. Machine-friendly router CLI**

- [x] Add `rlp route "<prompt>"` (or extend `rlp why`) that emits a minimal,
      stable JSON payload including at least
      `{route:"shell|agent", rule, blocked}` and well-defined exit codes (`0` =
      ok, `10` = shell, `11` = agent, non-zero error).
- [x] Ensure command is fast (no KB/model work) and side-effect-free so it can
      be called on every prompt in interactive sessions.

**O2. Zsh integration (preferred path)**

- [x] Ship a `runloop.zsh` snippet (e.g. under `packaging/shell/`) that defines
      a ZLE widget (`runloop-accept-line`) hooking `accept-line`, inspects
      `$BUFFER`, and calls `rlp route` to classify.
- [x] If route=`shell`, delegate to the builtin `accept-line`; if route=`agent`,
      invoke the default opening via `rlp run ... --params '{"prompt": "..."}'`
      (or a future `rlp prompt`), then clear the line instead of executing it in
      the shell.
- [x] Guard with an env toggle (e.g. `RUNLOOP_ROUTER_DISABLE=1`) and a small
      `:runloop-off`/`:runloop-on` helper so users can temporarily bypass
      routing.

**O3. Bash integration (best‑effort)**

- [x] Ship a `runloop.bash` snippet that wires an interactive‑only hook (e.g.
      `PROMPT_COMMAND` + `DEBUG` trap or a `bind -x` wrapper) to inspect the
      pending command line and call `rlp route`.
- [x] For route=`agent`, run the opening via `rlp` and prevent the original
      command from executing (e.g. by clearing `READLINE_LINE` / skipping
      execution); for route=`shell`, fall through with minimal latency.
- [x] Ensure non‑interactive shells (scripts, CI) never route through Runloop
      unless explicitly enabled.

**O4. Packaging & opt‑in flow**

- [ ] Install shell snippets into a well‑known location (e.g.
      `/usr/share/runloop/shell/`) and expose a helper
      (`rlp shell enable|disable`) that appends/removes `source` lines from user
      rc files (`~/.zshrc`, `~/.bashrc`) in a reversible way.
- [ ] Debian `postinst` prompts the current user to opt into shell integration
      (default = “no” for dev package); if accepted, call `rlp shell enable` for
      their account.
- [ ] Document a safe rollback path and ensure integration does not run when
      `$TERM=dumb` or inside restricted shells.

**O5. Acceptance & regression**

- [ ] Manual acceptance: after enabling integration, typing `ls -la` executes
      via the shell, while `draft email to John about Q4 plan` launches the
      configured opening; `rlp why` explains the decision for each.

---

## Epic P — Agent authoring UX (deb installs)

### P1. Scaffold that builds without a workspace

- [x] `rlp agent scaffold` emits crates with explicit `package.license` /
      edition (no `license.workspace=true`), so they build in
      `~/.runloop/agents-wasm` without a repo workspace.
- [x] Update README template to note the standalone build flow.

**DoD:** On a clean Debian host (no source tree), `cargo build --target
      wasm32-wasip1 --manifest-path ~/.runloop/agents-wasm/<name>/Cargo.toml`
      succeeds after scaffolding.

### P2. Build/install command for bundles

- [x] Add `rlp agent build` (or `build/install`) that compiles the wasm,
      copies it into `bin/`, refreshes `entry_wasm`/`tools.json` BLAKE3 digests,
      and validates caps.
- [x] Ship a digest helper with the deb (or vendor `b3sum`) and reuse it in the
      command.

**DoD:** On a clean Debian host: `rlp agent scaffold system_setup` →
      `rlp agent build system_setup` → `rlp run <opening>` succeeds without
      manual file edits or perl one-liners.

### P3. Config robustness for user installs

- [x] Config loader should skip unreadable `/etc/runloop/config.yaml` with a
      warning instead of failing the CLI.

**DoD:** With `/etc/runloop/config.yaml` unreadable, `rlp` still loads the user
      config and runs commands.

### P4. Docs (deb install flow)

- [x] Add a short guide for “authoring an agent on a deb install” (scaffold →
      build/install → run) to `docs/` and link from `docs/getting-started.md`.

**DoD:** Following the guide on a clean install produces a runnable agent
      without the source repo.
- [ ] Add a small integration test (or scripted demo) that opens an interactive
      shell under `script`/`expect`, feeds a few prompts, and asserts that
      router decisions and side effects match expectations.

**DoD:** With shell integration enabled, interactive prompts in a supported
shell are reliably classified by the router; shell‑routed commands behave like
normal terminal usage, agent‑routed prompts launch openings, and users can
disable integration via a documented toggle.

---

## Epic Q — Refactoring & Optimization (Post-MVP)

### Q1. Unified Agent Architecture

- [ ] **Common Data Access Layer:** Define a trait in `crates/agents/common` that
      abstracts data access (e.g., `resolve_contact(query) -> Contact`).
- [ ] **WASM Hostcalls:** Ensure `kb.query` hostcall is exposed to agents.
- [ ] **Refactor Agents:** Update `crates/agents-wasm` (e.g., `contact_resolver`)
      to use hostcalls for data access instead of static stubs.
- [ ] **Shared Domain Logic:** Move scoring, normalization, and other non-I/O logic
      from `crates/agents` into `crates/agents/common` to be reused by both
      native and WASM implementations.

### Q2. Bus Scalability

- [x] **Lock Granularity:** Replace `RwLock<HashMap<String, TopicState>>` in
      `crates/bus` with `DashMap<String, TopicState>` to reduce contention during
      high-frequency publish/subscribe operations.

### Q3. Router Optimization

- [ ] **Zero-allocation Classification:** Refactor `token_candidates` and
      matching logic in `crates/router` to operate on `&str` slices and use
      case-insensitive comparisons without allocating intermediate `String`s.

### Q4. Build & CI Maintenance

- [ ] **WASM Build Profile:** Add a `cargo` profile or alias (e.g., `cargo build`
      `--profile=agents`) that explicitly includes `crates/agents-wasm` to prevent
      code rot.
- [ ] **CI Enforcement:** Ensure CI pipelines build WASM agents strictly using
      the `wasm32-wasip1` target.

### Q5. Security & Sandboxing

- [ ] **Deprecate Native Execution:** Mark `executor-local`'s native agent path
      as deprecated/dev-only to enforce uniform sandboxing via WASM.
- [ ] **Audit:** Verify that all production paths use the WASM runtime.

### Q6. Testing Gaps

- [ ] **WASM Unit Tests:** Add unit tests for logic in `crates/agents-wasm`
      (e.g., slug generation, text processing).
- [ ] **Integration Tests:** Verify WASM bundles produce valid JSON output for
      known inputs using a test harness that mocks hostcalls.

---

## MVP Acceptance (end‑to‑end manual test)

1. **Start daemon & monitor**
   - [ ] `runloopd` running; `agtop` shows 0→N agents as run proceeds.

2. **Seed KB**
   - [ ] Insert `contact.upserted` for John (tool or `rlp kb query` with seed
         script).

3. **Run opening**
   - [ ] `rlp run examples/openings/compose_email.yaml --params '{"recipient":"john","topic":"Q4 plan"}'`
   - [ ] Router’s `why` explains why prompt → opening.
   - [ ] Opening executes nodes in order; DAG pane updates.

4. **Confirm mailer**
   - [ ] Confirm send; mailer prints “dry‑run send to
         [john@acme.com](mailto:john@acme.com)”.
   - [ ] KB shows `email.sent` referencing draft artifact.

5. **Trace & replay**
   - [ ] `rlp trace <id>` prints ladder with ≥5 steps.
   - [ ] `rlp replay <id>` reproduces identical draft hash.

6. **Caps audit**
   - [ ] Intentionally attempt forbidden FS write via a test agent; `cap.audit`
         event recorded.

7. **ISO boot**
   - [ ] Boot ISO in QEMU; repeat steps 3–5 successfully.

---

## Cut lines (what we purposely **skip** for MVP)

- Real email sending (network off for mailer).
- Multi‑user, multi‑machine, remote bus.
- Semantic embeddings (vector index) — OK to stub.
- Full secrets manager integration — use stub `secret_id`.
- Rich router LLM classification — heuristics only.

---

## Suggested implementation sequencing (summary checklist)

1. Core types & config (Epic A)
2. RMP + Bus (Epic B)
3. Runtime + Caps + Lifecycle (Epic C)
4. KB events + views + API (Epic E)
5. Broker (Null + HTTP stub) (Epic F)
6. Router v0 (Epic D)
7. Opening engine (parse + run + replay) (Epic G)
8. Canonical agents (H1–H5)
9. CLI/TUI & observability (I & J)
10. Security polish (K)
11. Packaging + ISO (L)
12. Golden harness + docs sync (M & N)
13. Prompt routing & shell integration (O)
14. Refactoring & Optimization (Q)
