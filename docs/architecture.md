# Architecture Overview (Draft)

> **Doc status:** Informative — highlights top-level structure and references
> normative specs. Last updated: 2025-11-02.

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

## Components (high-level)

- **Router:** classifies prompts, applies policies, launches shell commands or
  openings.
- **Openings engine:** DAG scheduler with retries, budgets, and deterministic
  replay.
- **Runtime (`runloopd`):** WASM host (Wasmtime), capability enforcement,
  lifecycle + logging.
- **Knowledge Base (POG):** event ledger (`events.sqlite`), materialized views
  (`pog.sqlite`), vector search.
- **Model broker:** mediates LLM/tool providers with budgets and rate limits.
- **TUI (`agtop` + main UI):** live monitoring, plan visualization, provenance
  inspection.

## Data Flow (summary)

1. Router receives prompt → decision (shell vs. opening).
2. Opening plan compiled → DAG executed; nodes exchange RMP messages over local
   bus.
3. Agents emit events → ledger append; materializer updates views; vector index
   refreshes.
4. Outputs recorded in POG, surfaced in UI; provenance links back to agent +
   schema IDs.

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
