# Architecture Overview

> **Doc status:** Informative overview that mirrors the current workspace layout
> and references normative specs. Last updated: 2025-11-16.

## Goals

- Route user prompts between POSIX shell execution and AI agent “openings”.
- Provide deterministic provenance (POG) and audit trails.
- Enforce least-privilege capability security for agents.
- Remain terminal-first and local-first (offline-friendly).

## Non-goals (v1)

- Shipping a kernel or Linux distribution.
- Multi-host scheduling or distributed openings.
- Non-Rust agent SDKs (beyond WASM interop).
- GUI/window manager integration.

## Components & Boundaries (current)

Each entry lists the owning crate/binary and the primary interfaces it exposes
or depends on. Paths refer to workspace members declared in `Cargo.toml`.

### Router & CLI (`crates/router`, `crates/rlp`)

- Classifies interactive prompts (shell fast-path vs. agent opening) and
  prepares `ControlRequest::RunSubmit` frames for the daemon.
- Reads configuration from `~/.runloop/config.yaml`/`/etc/runloop/config.yaml`
  via `crates/core` and obeys `router.fastpath_*` policies described in
  [README.md](../README.md).
- Talks to `runloopd` over the Unix socket using
  [`CT_CTRL_REQ/RESP`](message-protocol.md#9-control-plane-mvp) and subscribes
  to `rlp/runs/<trace_id>/events` for streaming output.
- Provides shim binaries such as `rlp run`, `rlp replay`, and `rlp trace` plus
  router-aware shell integration snippets (see TODO epic O).

### Openings Engine (`crates/openings`, `crates/runloopd`)

- Compiles YAML openings into DAGs with node budgets, retries, and deterministic
  replay metadata (e.g., `trace_id`, `opening_id`).
- Exposes planners/executors consumed by `runloopd`; local/offline execution is
  surfaced through `crates/executor-local` when `rlp --local` is used.
- Persists node/run transitions to the KB via `crates/kb` APIs so replay and
  audit trails share the same provenance data.

### Runtime & Agent Host (`crates/runloopd`, `crates/runtime`)

- Owns the Wasmtime engine, spawns agent WASM bundles with capability gating,
  and enforces lifecycle (timeouts, retry orchestration, resource budgeting).
- Hosts the message bus listener and publishes audit events (`run.*`, `node.*`,
  `cap.audit`) into the KB.
- Exposes the daemon service entrypoint packaged for both user and system mode.

### Message Bus & RMP (`crates/rmp`, `crates/bus`)

- Implements the 64-byte header/frame codec plus MsgPack body helpers described
  in [docs/message-protocol.md](message-protocol.md).
- Provides subscription/publish APIs layered on Unix domain sockets with TTL
  enforcement, dedupe caches, ACLs for `action.decision`, and drop telemetry on
  `rlp/sys/drops`.
- Shared by the daemon, CLI, TUI, and agents; all components depend on the same
  schema registry (`docs/rmp-registry.md`).

### Knowledge Base / POG (`crates/kb`, `docs/kb-schemas.md`)

- Maintains the append-only ledger (`events.sqlite`) plus materialized views
  (`pog.sqlite`) and optional embedding stores.
- Validates event payloads against schemas checked into `crates/kb/schemas/` and
  exposes migration/verify commands consumed by the CLI (`rlp kb ...`).
- Provides APIs to query runs, artifacts, contacts, and deterministic replay
  fixtures.

### Model Broker (`crates/model-broker`)

- Centralizes provider credentials, rate limits, and caching for model/tool
  calls made by agents.
- Enforced budgets flow through `runloopd` (per-opening limits) and are exposed
  to TUI panes for operator visibility.

### CLI Tooling & TUI (`crates/rlp`, `crates/agtop`, `crates/executor-local`)

- `rlp` offers operational commands (`run`, `trace`, `replay`, `kb`, `config`).
- `agtop` consumes NDJSON run streams or live bus subscriptions to show plan
  DAGs, logs, metrics, and trace ladders.
- `executor-local` backs `rlp --local` by instantiating agents within the CLI
  process for offline validation while still emitting the unified run-event
  schema.

### Agent Registry & Bundles (`crates/agent-registry`, `crates/agents/*`, `agents/`)

- Source of truth for canonical agent bundles, manifests, capabilities, and DCO
  signatures referenced by the runtime.
- Includes shared SDK layers (`crates/sdk/runloop-sdk`, `crates/sdk/agent-shim`)
  so native and WASM agents speak RMP consistently until the pure WASM path is
  complete.

### Observability & Ops (`crates/agtop`, `docs/ops.md`, `docs/perf.md`)

- `tracing`-based spans propagate `trace_id`/`opening_id`; metrics exporters can
  emit OTLP when configured via `observability.traces`.
- ASCII ladder traces (`rlp trace`) and KB-backed audits provide operator-level
  visibility; packaging assets (`packaging/`) install service units and shell
  hooks on Debian.

## Interaction Model

1. **Prompt ingress:** An interactive shell prompt is handed to the router via
   shell hooks or directly via `rlp run`. Policies decide between POSIX
   execution and submitting an opening request.
2. **Control-plane negotiation:** The CLI publishes `CT_CTRL_REQ` on `rlp/ctrl`
   (or runs locally) and waits up to 2000 ms for `RunAccepted`. The daemon
   replies with `CT_CTRL_RESP` including the assigned `trace_id` and opening
   metadata.
3. **Run event stream:** Once accepted, `runloopd` emits `CT_RUN_EVENT` frames
   to `rlp/runs/<trace_id>/events`. Consumers (CLI stdout, `agtop`, tests)
   render logs, plan status, artifacts, and trace ladder entries from this
   single stream.
4. **Agent execution:** Opening nodes orchestrated by the engine exchange RMP
   messages over the bus; capability checks fire before any hostcall (FS/NET/
   TIME/Kb/Model) and denied calls generate `cap.audit` events.
5. **Persistence & replay:** Ledger events (`run.started`, `run.finished`,
   `artifact.created`, etc.) are recorded through `crates/kb`. Replays sourced
   from the KB must deterministically regenerate the same outputs
   (`rlp replay`).
6. **Observability:** Drops, TTL expirations, and dedupe hits publish structured
   diagnostics on `rlp/sys/drops`, while `tracing` spans feed OTLP exporters
   when enabled.

## Runtime Modes & Packaging

- **User mode:** `cargo run -p runloopd` plus `rlp` uses `~/.runloop` for state
  and discovers sockets via `runtime.socket_path` or `runtime.sockets_dir`.
- **System mode:** Debian packages install `runloopd` as `runloop:runloop`,
  store KB data under `/var/lib/runloop`, drop sockets into `/run/runloop`, and
  install shell integration snippets (opt-in) under `/etc/profile.d`.
- Live-build ISO tooling in `packaging/live-build` consumes the same binaries
  and seeds demo scenarios referenced in `examples/openings/`.

## Security & Capabilities

- Capability manifests reside alongside agents (`policy.caps`) and are parsed by
  the runtime before any hostcall is granted; overrides can only further limit
  access (see `docs/policy-caps.md`).
- Secrets always travel via opaque `secret_id` indirections, never raw blobs in
  KB events; the CLI documents rollback/disable paths in `docs/ops.md`.
- The router enforces shell fast-path guards to avoid routing non-interactive
  scripts; ACLs prevent untrusted publishers from emitting `action.decision`.

## Observability Surfaces

- `agtop` panes (Log, Plan, agtop metrics) subscribe to the same run stream and
  bus metrics.
- `docs/perf.md` outlines the throughput harness (`≥600 msgs/s` target for
  loopback testing) and should be referenced when adjusting bus internals.
- Counters (`msgs_sent`, `drops_total`, `cap_denied`, etc.) and gauges
  (`agents_running`, `bus_queue_depth`) are emitted via the runtime’s metrics
  facade for scraping or interactive display.

## Reference Map

- **Protocol:** [docs/message-protocol.md](message-protocol.md)
- **Knowledge base schemas:** [docs/kb-schemas.md](kb-schemas.md)
- **Openings grammar:** [docs/openings-dsl.md](openings-dsl.md)
- **Router & shell integration:** forthcoming section per TODO Epic O; interim
  coverage lives in [README.md](../README.md).
- **Operations:** [docs/ops.md](ops.md)
- **Observability tooling:** [docs/perf.md](perf.md), `docs/tui.md`
  (screenshots).

## Router configuration (MVP)

- `router.fastpath_shell`: enable or disable the shell fast-path.
- `router.default_opening`: opening name used when routing to agents.
- `router.allowlist` / `router.denylist`: prompt patterns that force or block
  shell execution.
- `router.known_commands`: extra command names layered onto the builtin + PATH
  discovery set.

## Trust & Capabilities Model

- Agents declare capabilities via `policy.caps` (see `docs/policy-caps.md`).
- Overrides located at `~/.runloop/caps/overrides` only revoke privileges.
- Trust policy controls which signatures can install/run agents (`docs/ops.md`,
  `docs/security-model.md`).

## Configuration precedence (summary)

- CLI flags → environment (`RUNLOOP_*`) → user config (`~/.runloop/config.yaml`)
  → system config (`/etc/runloop/config.yaml`) → defaults.
- Policy keys defined by system config act as hard limits; lower layers can only
  tighten them.
- Merge semantics detailed in `docs/ops.md`.

## Portability Plan

- Primary target: Debian 12 (x86_64, arm64) host OS.
- Future exploration: Redox and container-portable builds once v1.0 baseline
  achieved.

For deeper details, consult `README.md`, `docs/message-protocol.md`,
`docs/kb-schemas.md`, and `docs/ops.md`.

## Control & Transport (MVP)

- A single Unix domain socket serves both the message bus and control plane.
- The CLI submits openings by publishing `CT_CTRL_REQ` on `rlp/ctrl`. The daemon
  responds with `CT_CTRL_RESP::RunAccepted` and streams `CT_RUN_EVENT` to
  `rlp/runs/<trace_id>/events`.
- Socket discovery precedence: `runtime.socket_path` (short‑circuit), then
  `${runtime.sockets_dir}/rmp.sock`, then `~/.runloop/sock/rmp.sock`, then
  `/run/runloop/rmp.sock`.
- Only UI/TUI publishers may emit `action.decision` on the bus; CLI does not
  prompt when connected to the daemon.

## KB Ownership (MVP)

- For daemon-backed runs, `runloopd` records `run.started` and `run.finished` in
  the KB. Node-level persistence is planned; the CLI records both start and
  finish only in `--local` mode today.
