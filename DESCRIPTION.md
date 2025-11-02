## Project description — Runloop

### One‑liner

**Runloop** is an **agent‑native operating system** (terminal‑first) that routes user prompts to either *shell commands* or *AI agents*, executes them in lightweight sandboxes, composes agents into **Openings** (crews) over a typed message bus, and maintains a **personal operations graph** (knowledge base) for durable memory, provenance, and reuse.

### Goals

* **Terminal‑first UX** with zero windows; everything observable and controllable from TUI and CLI.
* **Universal agent protocol** so an “infinite” set of agents can interoperate (typed messages, provenance, capabilities).
* **Lightweight isolation** so hundreds of agents can run concurrently on commodity hardware.
* **Router** that decides shell vs agent vs opening with explainability and budget controls.
* **Openings** DSL to declaratively compose multi‑agent plans with replay and step‑through debugging.
* **Self‑improvement** loop that learns from failures and user feedback to tune prompts, policies, and plans.
* **Personal operations graph (POG)**: event‑sourced, queryable, privacy‑preserving knowledge base for accounts, contacts, artifacts, logs.

### Non‑goals (v1)

* GUI window manager, desktop integration.
* Distributed scheduling across a fleet of machines (single host first; multi‑node later).
* Full Redox port (tracked as long‑term exploration).
* Agent marketplace (build local packaging first).

### Core principles

* **Rust everywhere** for safety, performance, and predictable latency.
* **WASM (WASI)** as agent container for startup speed, tiny RSS, and strong sandboxing.
* **Capability security**: least privilege, explicit grants, human confirmation for side effects.
* **Local‑first**: everything works offline; cloud is an optional extension via the model broker.
* **Explainability**: every route, plan, and answer has “why” and provenance.
* **Deterministic replay** for debugging and self‑improvement.

### Synergetics vocabulary (your terms)

* **Trajectories** → *Agents* (goal‑directed, tool‑using units).
* **Crossings** → *Interactions* (typed messages/artifacts between agents).
* **Openings** → *Crews/Plans* (a set of agents plus their crossings, represented as a DAG).

---

## High‑level architecture

* **runloopd** (daemon): process supervisor, scheduler, message bus, model broker, capability gate, POG access.
* **rlp** (CLI) and **TUI** (ratatui): router entrypoint, plan viewer, logs/trace, `agtop` (agent htop), budget controls.
* **Agent runtime**: Wasmtime (WASI), per‑agent capability sandbox, signed message endpoints.
* **Runloop Message Protocol (RMP)**: typed, signed messages with provenance and budgets.
* **Openings orchestrator**: DAG execution, retries, fan‑in/out, step/run/replay.
* **POG (Personal Operations Graph)**: event log (append‑only), materialized relational view (SQLite), vector index (HNSW), content‑addressed blob store.
* **Model broker**: pluggable local/remote model providers, caching, metering, policy.
* **Observability**: tracing, metrics, cost accounting, structured logs.

