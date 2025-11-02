# Runloop

> **An agent‑native, terminal‑first OS layer** — routes your prompt to the shell or to AI agents, composes agent “crews” (Openings) over a typed message bus, and keeps a trustworthy personal knowledge base.

---

<p align="center">
  <em>Status: Pre‑alpha • Platform: Debian (host) • Language: Rust • Runtime: WASM/WASI</em>
</p>

> **Doc status:** Draft — normative sections are labeled. Last updated: 2025‑11‑02.

---

## Table of contents

- [What is Runloop?](#what-is-runloop)
- [Why Runloop?](#why-runloop)
- [Project status & scope](#project-status--scope)
- [Core ideas & terminology](#core-ideas--terminology)
- [Architecture (high level)](#architecture-high-level)
- [Repository layout](#repository-layout)
- [Getting started (developers)](#getting-started-developers)
- [Configuration](#configuration)
- [Security model (capabilities)](#security-model-capabilities)
- [Knowledge Base (POG)](#knowledge-base-pog)
- [Message Protocol (RMP)](#message-protocol-rmp)
- [Openings DSL (sketch)](#openings-dsl-sketch)
- [Observability & tracing](#observability--tracing)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [Security & responsible disclosure](#security--responsible-disclosure)
- [License](#license)
- [FAQ](#faq)

---

## What is Runloop?

Runloop is an **agent‑native operating layer**: a terminal UI and background services that decide whether a user prompt should run as a **shell command** or be handled by a **set of AI agents**. Agents are sandboxed **WASM** tasks with **least‑privilege capabilities**, connected by a small, typed message protocol. Outputs and decisions are recorded into a **local, event‑sourced knowledge base** you can query and audit.

- **Terminal‑first**: no desktop “windows.” You drive everything from the TUI/CLI.
- **Composable agents**: agents work alone or as a **crew** (“Opening”) with clear inputs/outputs.
- **Trust & audit**: every step carries provenance; you can replay a run deterministically.
- **Local‑first**: works offline on a Debian host; cloud is optional (model providers, sync).

> ⚠️ **Runloop is not a kernel or Linux distribution.** It runs on an existing OS (Debian recommended) and provides an “OS‑like” agent runtime, router, and knowledge base.

---

## Why Runloop?

**TL;DR:** Runloop unifies the *shell* and *AI agents* under one terminal‑first runtime with a typed message bus, deterministic replay, and a trustworthy, local knowledge base. You keep your workflow; Runloop makes it safer, auditable, and automatable.

### The problems it solves

- **Fractured workflows.** Today you bounce between shell scripts, ad‑hoc Python, SaaS “AI assistants,” and sticky notes. Runloop gives you *one cockpit* (the terminal) and a router that decides: *execute as shell* vs *hand off to agents*.
- **Unreliable agent glue.** Plain prompt chaining lacks types, budgets, and error handling. Runloop composes agents as **Openings** (DAGs) with timeouts, retries, and success criteria.
- **No memory you can trust.** SaaS assistants hoard your data; local scripts forget. Runloop maintains a **local, event‑sourced knowledge base** (Pog) with provenance—every fact can be traced back to its source.
- **Opaque behavior & drift.** “Why did it do that?” Runloop records **typed crossings** and supports **deterministic replay** so you can inspect, test, and improve behavior.
- **Security theater.** Most tools assume all‑powerful agents. Runloop enforces **capabilities** (fs/net/kb/secrets/model) and **confirmation gates** for external actions.
- **Cost and latency sprawl.** The **model broker** centralizes LLM usage with per‑request budgets, caching, and provider choice (local or cloud).

### Why it’s different

- **Terminal‑first, not app‑first.** Keep `ls | grep` muscle memory. Runloop routes only when an agent adds value.
- **Typed, minimal protocol.** Agents talk over a small, versioned message format (RMP), enabling interoperability without giant frameworks.
- **Local‑first by default.** Works offline; sync and remote models are optional—not required.
- **Designed for operations.** Built‑in **agtop** metrics, trace views, budgets, and audit logs treat agents like real system processes.
- **Built to get better.** Self‑improvement is a first‑class Opening: collect failures → propose patches → A/B → adopt with provenance.

### Before vs. with Runloop

| Dimension            | Shell scripts & CLIs       | “AI assistant” SaaS           | Task runners/Notebooks     | **Runloop** |
|----------------------|----------------------------|-------------------------------|----------------------------|-------------|
| Interface            | Terminal                    | Web/app                       | Mixed                      | **Terminal‑first** |
| Composition          | Manual glue                 | Hidden orchestration          | Imperative cells/pipelines | **Declarative Openings (DAG)** |
| Memory/State         | Files/logs (ad‑hoc)         | Vendor database               | Project‑local              | **Local event‑log + facts (Pog)** |
| Reproducibility      | Hard; brittle               | Opaque                        | Partial                    | **Deterministic replay** |
| Safety               | Whatever the script does    | Black‑box policies            | Varies                     | **Capability security + confirms** |
| Observability        | Grep logs                   | Limited UI                    | Some dashboards            | **agtop + trace ladder + audits** |
| Offline              | Yes                         | Rare                          | Yes                        | **Yes (cloud‑optional)** |
| Extensibility        | Anything you can exec       | Limited APIs                  | Language‑specific          | **WASM agents + Rust SDK** |
| Vendor lock‑in       | None                        | High                          | Medium                     | **Low** |

### When to use Runloop

- You want to **keep the shell** but add reliable agent automation (e.g., “draft email to John from last week’s meeting notes”).
- You need **auditability** and **provenance** for generated outputs (who/what/why/which model).
- You care about **privacy** and **local control** of your data and tools.
- You plan to **operate many small agents** concurrently and need observability, budgets, and guardrails.

### When not to use it (yet)

- You need a full desktop/window manager.
- You want multi‑tenant, cross‑machine orchestration (single‑host focus in v1).
- You require non‑Rust SDKs immediately (WASM makes this possible later, but Rust ships first).

---

## Project status & scope

- **Status:** Pre‑alpha scaffolding (docs, structure, packaging plans). Implementation lands in phases.
- **Primary target:** Debian stable (bookworm) as host OS.
- **Language:** Rust across the stack.
- **Runtime:** WASM/WASI via Wasmtime for sandboxed agents.

**Non‑goals for v1**
- GUI/window manager
- Multi‑tenant remote service
- Cross‑machine distributed bus (single host focus)
- Non‑Rust SDKs (can follow after 1.0)

---

## Core ideas & terminology

- **Trajectory** — an individual agent with a goal and a budget (LLM + tools).
- **Crossing** — a typed interaction between agents (messages, artifacts).
- **Opening** — a *crew* (plan/DAG) of agents + their crossings to accomplish a task.
- **Router** — entrypoint that decides: run in shell vs. route to an Opening.
- **POG (Personal Ops Graph)** — Runloop’s local knowledge base (events + facts + artifacts).

---

## Architecture (high level)

```

┌────────────┐      ┌───────────────┐      ┌────────────────────┐
│  Terminal  │ ───▶ │   Router      │ ──┬▶ │  Shell (fast path) │
│   (TUI)    │      │  (prompt→plan)│   │  └────────────────────┘
└────────────┘      └───────────────┘   │
│
▼
┌─────────────┐
│  Openings   │   DAG scheduler,
│   Engine    │   retries, budgets
└─────┬───────┘
│ crossings (typed messages)
┌──────────────────────┼─────────────────────────┐
▼                      ▼                         ▼
┌────────────┐         ┌────────────┐            ┌─────────────┐
│  Agent A   │         │  Agent B   │    ...     │  Agent N    │
│  (WASM)    │         │  (WASM)    │            │  (WASM)     │
└─────┬──────┘         └─────┬──────┘            └─────┬───────┘
│                      │                           │
└──────────────┬───────┴───────────────┬──────────┘
▼                       ▼
┌─────────────┐         ┌───────────────┐
│ Model       │         │  KB (POG)     │
│  Broker     │         │  (events+facts│
│ (LLMs)      │         │   +artifacts) │
└─────────────┘         └───────────────┘

```

---

## Repository layout

```
runloop/
├─ README.md
├─ LICENSE, CODE_OF_CONDUCT.md, CONTRIBUTING.md, SECURITY.md, SUPPORT.md, CODEOWNERS
├─ .editorconfig, .gitattributes, .gitignore, rust-toolchain.toml, CHANGELOG.md, Justfile
├─ docs/
│  ├─ architecture.md
│  ├─ roadmap.md
│  ├─ getting-started.md
│  ├─ contributor-guide.md
│  ├─ release-process.md
│  ├─ security-model.md
│  ├─ message-protocol.md
│  ├─ kb-schemas.md
│  ├─ openings-dsl.md
│  ├─ tui.md
│  ├─ ops.md
│  ├─ policy-caps.md
│  └─ adr/
│     └─ README.md
├─ crates/
│  ├─ core/ (shared types & config loader)
│  ├─ bus/ (local message bus + TTL/dupe handling)
│  ├─ rmp/ (Runloop Message Protocol codec)
│  ├─ kb/ (knowledge base)
│  ├─ openings/ (DSL parser)
│  ├─ model-broker/ (LLM broker)
│  ├─ runtime/ (WASM runtime, capabilities)
│  ├─ sdk/ (agent SDK)
│  ├─ rlp/ (CLI)
│  ├─ runloopd/ (daemon)
│  └─ agtop/ (monitor TUI)
├─ agents/ (agent bundles + capability manifests)
├─ examples/
│  ├─ openings/ (YAML samples)   └─ config/ (sample configs)
├─ packaging/
│  ├─ systemd/ (units)           ├─ live-build/ (ISO scaffolding)
│  └─ container/ (dev container scaffolding)
├─ infra/
│  ├─ ci/                        └─ release/
└─ .github/
   ├─ ISSUE_TEMPLATE/            ├─ pull_request_template.md
   └─ workflows/ (CI scaffolding)
````

> **Normative docs today:** `docs/message-protocol.md`, `docs/rmp-registry.md`, `docs/kb-schemas.md`, `docs/policy-caps.md`, `docs/ops.md`, `docs/security-model.md`. Other docs remain informative until promoted via ADR.

---

## Getting started (developers)

> This repo starts with **structure and docs**. Code lands incrementally. The steps below describe the intended flow once binaries exist.

### 1) Prerequisites (host)
- Debian/Ubuntu dev machine (or WSL2/VM)
- Rust toolchain (`rustup`), `cargo`, `clippy`, `rustfmt`
- Optional: `just`, `live-build`, `qemu-system-x86`, `cargo-deb`

### 2) Clone
```bash
git clone https://github.com/<you>/runloop.git
cd runloop
````

### 3) Read the docs

Start with:

* `docs/architecture.md`
* `docs/message-protocol.md`
* `docs/kb-schemas.md`
* `docs/openings-dsl.md`
* `docs/roadmap.md`

### 4) Build & test (when code is present)

```bash
# format & lint
cargo fmt --all
cargo clippy --workspace -- -D warnings

# build everything
cargo build --workspace

# run unit tests
cargo test --workspace
```

### 5) Packages & images (optional, later)

* **Debian packages**: `cargo deb` per binary crate (see `packaging/systemd/`).
* **Live ISO**: use `packaging/live-build` to build an `iso-hybrid` that includes Runloop packages.
* **Dev container/VM**: use `packaging/container` (Docker/Podman) or QEMU for a closer‑to‑real test.

---

## Configuration

Global config lives at `~/.runloop/config.yaml` (user‑scoped). Example (schema **v1**):

```yaml
version: 1
kb:
  root_dir: "~/.runloop"
  events_db: "pog/events.sqlite"
  view_db: "pog/views.sqlite"
logging:
  level: info
  format: text                # valid: text | json
  file: null
observability:
  otlp_endpoint: ""
  traces_sampling_ratio: 0.01
security:
  secrets:
    provider: "auto"          # stub | os-keyring | age | auto
router:
  default_opening: "compose_email"
  confirm_external: true
models:
  broker:
    endpoint: "http://127.0.0.1:8082"
    cache_ttl_sec: 30
```

Legacy aliases — `kb.ledger`, `kb.materialized`, `security.secrets_backend`, `observability.logs_format` — map to the v1 keys and emit a warning when encountered. Prefer the canonical fields above. Every key can be overridden from the environment using `RUNLOOP__` prefixes, e.g. `RUNLOOP__MODELS__BROKER__ENDPOINT=https://broker.internal`.

User-mode runs keep state under `~/.runloop/**` and expose the bus at `~/.runloop/run/runloopd.sock`. Packaged/systemd deployments run as `runloop:runloop`, store state in `/var/lib/runloop`, and bind the bus at `/run/runloop/runloopd.sock`.

Runloopd bootstraps the directory structure (`0700`), initializes both SQLite databases, and selects a secrets provider automatically when `security.secrets.provider = "auto"`. If no platform keyring is available it falls back to the stub provider, storing only opaque `secret_id` references. CLI helpers such as `rlp kb init` and `rlp secrets put` operate against the same layout.

---

## Security model (capabilities)

Agents run in WASM sandboxes with **least‑privilege** capabilities. Authoring happens in the repo at `agents/<name>/policy.caps`; operator overrides live in `~/.runloop/caps/overrides/` and can only **remove** permissions (intersection semantics). See `docs/policy-caps.md` for the full schema.

Each agent declares a manifest (example: `policy.caps`):

```toml
[capabilities]
fs = ["/home/user/work/notes"]      # scoped read/write paths
net = ["api.mailprovider.com"]      # allowed hostnames
time = true
kb_read = true
kb_write = ["contacts","artifacts"] # fine-grained domains
secrets = ["mail.smtp.key"]         # references, not raw secrets
model = true
exec = false
```

* **Secrets** are referenced by ID and resolved via the OS keyring (or an encrypted vault).
* Every capability use is **audited**, and external actions require **confirmations** by default.
* List syntax shown above is canonical; the runtime normalizes each entry to an internal bitset (no dotted aliases).

---

## Knowledge Base (POG)

A **local‑first** store for events, facts, and artifacts:

* **Event log (append‑only)** → `~/.runloop/pog/events.sqlite` (WAL, synchronous=FULL)
* **Materialized views** → `~/.runloop/pog/pog.sqlite` (rebuilt from the ledger if corrupted)
* **Vector index** → `~/.runloop/pog/vectors/` (HNSW files linked via `pog.sqlite`)
* **Indexes** → `contacts(email)`, `accounts(handle)`, `artifacts(kind, ts)`, `edges(from_id, kind, to_id)`; tunable per deployment.
* **Retention** → ledger is indefinite; materialized views compact automatically; operators can archive events older than 365 days via `rlp kb vacuum --before`.
* **Automatic init** → first run creates the directories (`0700`) and seeds the schema; `rlp kb init` will re-initialize/verify
* **Usage ledger** → broker/app subsystems append cost + token usage events for auditing
* **APIs** (conceptual): `kb.propose(delta)`, `kb.query(sql_like)`, `kb.search(q, k, filter)`, `kb.why(id)`
* **Content hashing** → ledger stores canonical JSON payloads with a `BLOB(32)` BLAKE3 digest; tooling renders hex only for logs/UI.

Provenance chains link outputs to inputs, models, and agent versions for **explainability**.

---

## Message Protocol (RMP)

MVP ships **RMP v0**: a fixed 60‑byte header plus a MsgPack body `{ type: schema_id, payload: ... }`. The header carries real-time metadata, including creation time, so receivers can enforce time-to-live.

**Header layout (60 bytes):**

| Offset | Field | Type | Notes |
| ------ | ----- | ---- | ----- |
| 0 | Magic | `[u8;4]` | ASCII `RMP0` |
| 4 | `header_version` | `u16` | `0` for v0 |
| 6 | `header_len` | `u16` | Always `60` |
| 8 | `flags` | `u16` | bit0 = signed, bit1 = zstd |
| 10 | `schema_id` | `u16` | See registry |
| 12 | `body_len` | `u32` | MsgPack bytes |
| 16 | `created_at_ms` | `u64` | Header timestamp |
| 24 | `ttl_ms` | `u32` | Relative TTL; `0` = infinite |
| 28 | `trace_id` | `[u8;16]` | UUIDv7 suggested |
| 44 | `msg_id` | `[u8;16]` | UUIDv7 suggested |

Receivers drop frames with expired TTLs, dedupe on `(trace_id, msg_id)`, and emit metrics via `rlp/sys/drops`. Signatures and reliability ACKs remain on the roadmap for **RMP 0.2**.

Core payload schemas map to the registry (`docs/rmp-registry.md`): `Observation`, `Intent`, `ToolCall`, `ToolResult`, `Artifact`, `Critique`, `StateDelta`, and control frames (`Control.Heartbeat`, `Control.Ack`, `Control.Error`).

See the normative description in `docs/message-protocol.md`.

---

## Openings DSL (sketch)

An Opening defines a **plan/DAG** of agents, with budgets and policies:

```yaml
api: v1
name: compose_email
params: { recipient: string, topic: string }

nodes:
  contacts: { use: contact_resolver, with: { query: "{{params.recipient}}" } }
  context:  { use: context_gatherer,  with: { topic: "{{params.topic}}" } }
  draft:
    use: writer
    with: { prompt: "Write a concise email about {{params.topic}}" }

edges:
  - { from: contacts.out, to: draft.recipients }
  - { from: context.out,  to: draft.context }

policy:
  budget_tokens: 2000
  timeout_ms: 20000
  confirm_external: true
```

Per-opening `policy` settings override the global defaults declared in `security.confirm_external_actions` and related knobs.

Authoring format is YAML; the ABNF in `docs/openings-dsl.md` documents the grammar.

See: `docs/openings-dsl.md`.

---

## Observability & tracing

* **agtop**: live CPU/RSS/tokens/errors per agent, system health.
* **runloop trace <id>**: prints a ladder diagram of the Opening run, with spans per Crossing.
* **Replay**: deterministic re‑execution for debugging and self‑improvement.
* **OpenTelemetry**: traces + metrics export over OTLP; configure sampling/endpoints in `observability`.
* **Structured logs**: JSON lines by default with `trace_id`, `opening_id`, and agent metadata.
* **Logging knobs**: `logging.*` handles level/file/format; `observability.*` is reserved for tracing/OTLP (the legacy `observability.logs_format` alias still works with a warning).

See: `docs/tui.md`, `docs/ops.md`.

---

## Roadmap

Runloop ships in **phases** (Seed → Openings/SDK → KB Hardening → Reliability/Security → Beta → v1.0/Portability). The active plan, milestones, and exit criteria are tracked in:

* `docs/roadmap.md`
* `docs/adr/` (architecture decisions)
* **MVP constraint:** the model broker only serves non-streaming completions; streaming toggles on behind a Phase-3 feature flag (`docs/roadmap.md` M5/M6).

---

## Contributing

We welcome early contributors, especially around:

* Protocol and schema design
* Packaging and reproducible builds
* TUI ergonomics and accessibility
* Knowledge retrieval evaluation

Please read:

* `CONTRIBUTING.md` — how we work (issues, PRs, reviews)
* `CODE_OF_CONDUCT.md` — expected behavior
* `docs/contributor-guide.md` — style & process
* `docs/release-process.md` — versioning and artifacts
* `docs/security-model.md` — sandboxing, secret handling, provenance
* `docs/policy-caps.md` — capability manifests and override semantics
* `docs/ops.md` — secrets, observability, packaging, model budgets

**Important:** Never commit secrets. Use `.env.example` and OS keyrings.

---

## Security & responsible disclosure

If you believe you’ve found a security issue, please read `SECURITY.md` and email the listed contact. We aim to acknowledge within the documented window.

---

## License

Unless noted otherwise, code and docs are licensed under **Apache‑2.0**. See `LICENSE`.

---

## FAQ

**Is Runloop a Linux distro or a kernel?**
**No. Runloop is not a kernel or Linux distribution.** It’s an **agent runtime + router + TUI + POG** that runs on top of an existing OS (Debian recommended).

**Do I need to clone Debian sources?**
No. We rely on official Debian packages/images. Only clone Debian/kernel sources if you plan to patch them.

**Can I build agents in Python or Node?**
Yes—if they compile to **WASI/WASM**. The first SDK ships for Rust, but any language that produces compatible WASM binaries (Python via Pyodide, AssemblyScript/TypeScript, Go, Zig, etc.) can participate once it follows the capability manifest and RMP contracts.

**Does Runloop require the cloud?**
No. Runloop is **local‑first**. You can add cloud providers via the model broker if desired.

**What about privacy?**
Agents are capability‑scoped; secrets are referenced, not stored raw. The KB is local and event‑sourced with provenance. External actions require confirmation by default.

---

> Questions? Ideas? Open a discussion or issue. See `SUPPORT.md` for where to get help.

```
