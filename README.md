# Runloop

> An **agent‑native operating layer** for your machine: terminal‑first,
> Rust‑powered, agents in lightweight **WASM/WASI** sandboxes, composed into
> **Openings** (DAGs) over a typed local message bus (RMP).

**Status:** pre‑alpha design + docs. Debian‑first; portable later. Runloop is
**not a kernel or distro**—it sits above your OS to route prompts to either the
shell or to AI agents.  
See [ROADMAP.md](./ROADMAP.md) for the phased plan and
[docs/perf.md](./docs/perf.md) for the performance harness.

---

## Documentation (mdBook)

The documentation under `docs/` is organized as an mdBook.

- Build: `mdbook build docs`
- Serve locally: `mdbook serve docs -n 127.0.0.1 -p 3000`

If you use `just`, convenient tasks are available:

- `just docs-book` – builds the book into `docs/book/`
- `just docs-serve` – serves with live-reload for local editing

---

## What this repo is / is not

**Is:** a terminal‑first layer that:

- routes your prompt to the shell _or_ to agents,
- runs many small agents (WASM/WASI sandboxes) with least‑privilege
  capabilities,
- composes agents into **Openings** (typed DAGs) you can run, pause, replay,
- maintains a **personal ops graph (POG)**: an event‑sourced knowledge base with
  provenance and semantic search.

**Is not:** a new kernel, a full Linux distro, or a desktop/windowing
environment.

---

## Quick start

### From source (dev / user mode)

Requirements: Rust (edition 2024), `cargo`, and a recent WASI runtime (e.g.,
Wasmtime).

```bash
git clone https://example.com/runloop.git
cd runloop
cargo build --workspace
```

Run the daemon and CLI locally (user mode uses `~/.runloop` for
config/artifacts):

```bash
# daemon (user mode)
cargo run -p runloopd

# CLI (daemon-first)
cargo run -p rlp -- help

# Run an opening locally (daemon offline)
cargo run -p rlp -- run examples/openings/compose_email.yaml --local --params '{"recipient":"john"}'

# monitor (agent-top)
cargo run -p agtop

# inspect resolved config layers
cargo run -p rlp -- config path --all
```

> **Note:** `rlp run` now probes the daemon socket before doing any local work.
> Provide `--local` explicitly when you want inline execution; both modes stream
> NDJSON `RunEvent` records so monitors such as `agtop` can consume the same
> schema.

### Packages & images (daemon / system mode)

When installed from a .deb or image, the service runs as **`runloop:runloop`**
and writes state under **`/var/lib/runloop`**; its UDS socket lives at
`/run/runloop/runloopd.sock`. User mode continues to use `~/.runloop` for
config/artifacts (runtime socket defaults to
`$XDG_RUNTIME_DIR/runloop/runloopd.sock`, falling back to
`~/.runloop/run/runloopd.sock`).

---

## Configuration (Config v1)

Create `~/.runloop/config.yaml` for user mode, or `/etc/runloop/config.yaml` for
system mode:

```yaml
version: 1

runtime:
  base: "debian"
  agent_container: "wasm32-wasi"

models:
  default: "local:llama3.1-8b"
  broker:
    providers:
      - id: "openai"
        kind: "http"
        base_url: "https://api.openai.com"
        secret_id: "runloop/models/openai"
      # Gemini (text-only) example:
      # - id: "gemini"
      #   kind: "http_gemini"
      #   base_url: "https://generativelanguage.googleapis.com"
      #   secret_id: "runloop/models/gemini"
    route:
      - pattern: "*"
        provider: "openai"
    cache:
      ttl_ms: 600000
      capacity: 1024
    budgets:
      default_tokens: 8000
      hard_cap_usd: 0.50

kb:
  # root_dir differs by mode; user mode defaults to "~/.runloop/pog",
  # system mode defaults to "/var/lib/runloop/pog"
  root_dir: "~/.runloop/pog"
  events_db: "events.db" # append-only event log
  view_db: "pog.sqlite" # materialized views

logging:
  level: "info" # error | warn | info | debug | trace
  format: "auto" # auto | json | text
  file: "" # optional path

observability:
  traces:
    enabled: false
    otlp_endpoint: "" # e.g., http://localhost:4317
    sampling: "parent" # parent | always_on | ratio:0.1

security:
  confirm_external_actions: true
  secrets:
    provider: "os-keyring" # stub | os-keyring | age
    root: "~/.runloop/secrets" # only used by 'age' or 'stub'

router:
  fastpath_shell: true
  default_opening: "compose_email"
  allowlist: []
  denylist: []
  known_commands: []

ui:
  theme: "mono"
```

Runtime socket settings: prefer `runtime.socket_path` (explicit file). If unset,
`runtime.sockets_dir` is used with implied filename `runloopd.sock`. Defaults:
user mode → `$XDG_RUNTIME_DIR/runloop/runloopd.sock` (fallback
`~/.runloop/run/runloopd.sock`); system mode → `/run/runloop/runloopd.sock`.

**Aliases (compatibility):** `kb.ledger` → `<root_dir>/<events_db>`,
`kb.materialized` → `<root_dir>/<view_db>`. The config loader maps old keys and
warns; aliases are kept for compatibility. **Environment overrides:** any key
via `RUNLOOP__SECTION__SUBKEY=value` (e.g., `RUNLOOP__LOGGING__LEVEL=debug`).

---

## Architecture at a glance

- **Daemon (`runloopd`)** – hosts the local bus, schedules agents, enforces
  capabilities.
- **Runtime** – spawns agents as **WASM/WASI** tasks (fast start, low RSS,
  sandboxed).
- **SDK & Shim** – `runloop-sdk` + the `agent-shim` bootstrap allow MVP native
  agents to speak the bus/RMP protocol with the same capability envelope until
  their WASM bundles land.
- **RMP (Runloop Message Protocol)** – typed, traceable messages over UDS:
  headers carry trace/budget/TTL; bodies are schema‑tagged.
- **Openings** – declarative DAGs that define a crew of agents and their
  crossings; supports retries, timeouts, budgets, and deterministic replay.
- **POG (knowledge base)** – local‑first event log + materialized views, with
  embeddings for semantic recall and full provenance.
- **Model broker** – centralizes model/provider selection, budgets, caching.

---

## Key concepts

- **Trajectories** – individual agents with goal + budget.
- **Crossings** – typed interactions between agents (messages, artifacts).
- **Openings** – a plan (DAG) of agents + crossings you can run/pause/replay.

Example Opening:

```text
opening "compose_email" {
  goals: ["email to john about q4 plan"]
  nodes:
    contacts := agent("contact_resolver")
    context  := agent("context_gatherer", topic="{{params.topic}}")
    draft    := agent("writer", model="mixtral-8x7b", topic="{{params.topic}}", tone="neutral-friendly")
    review   := agent("critic")
    send     := agent("mailer", require_human_confirm=true, topic="{{params.topic}}")
  edges:
    contacts.out -> draft.recipients
    contacts.out -> context.contact
    context.out  -> draft.context
    draft.out    -> review.in
    draft.out    -> send.draft
    review.review -> send.review
    contacts.out -> send.contact
    review.ok    -> send.in
}
```

See the canonical YAML at `examples/openings/compose_email.yaml` for the
normative form used by the parser.

---

## Message Protocol (RMP)

**MVP (RMP v0)** wire format uses a fixed 68-byte header (`magic`, `version`,
`len`, `flags`, `schema_id`, `body_len`, `created_at_ms`, `ttl_ms`, `trace_id`,
`opening_id`, `msg_id`) followed by a MsgPack body containing typed payload +
optional metadata (budgets, priority, capability hints). Receivers enforce TTL
using `created_at_ms + ttl_ms`. **RMP 0.2** (later in the roadmap) adds
signatures/acks and richer metadata; wire compatibility is preserved.

Message bodies are schema‑tagged (JSON Schema on write; msgpack on the wire is
allowed). Provenance is attached to each message for audit/replay.

---

## Knowledge Base (POG)

Local‑first storage with:

- **Events** (append‑only, SQLite) and **Views** (materialized tables), plus a
  vector index for semantic recall.
- All state changes are proposed as `StateDelta` with provenance; a validator
  stamps & applies them.
- Hashing uses **BLAKE3** (binary `BLOB(32)`); hex is a UI/log rendering.

---

## CLI & TUI

- **`rlp`** – prompt entry (routes to shell fast-path or to an Opening), budget
  flags, dry-run.
  - Explain routing decisions with `cargo run -p rlp -- why "ls -la"` (plain
    text) or append `--json` for machine-readable output.
  - Run an Opening locally with
    `cargo run -p rlp -- run examples/openings/compose_email.yaml --params '{"recipient":"john","topic":"Q4 plan"}' --trace-out trace.json`.
    The command now drives the full compose-email stack (contact resolver →
    context gatherer → writer → critic → mailer), prints per-node status, and
    optionally writes a replayable trace. Make sure `runloop.json` points to a
    writable KB folder, that the model broker has at least one provider (or rely
    on the writer's heuristic fallback), and export any provider secrets to the
    environment so the CLI secret resolver can read them. Mail send still runs
    as a dry-run and prompts for approval unless
    `security.confirm_external_actions=false`.
  - Replay a recorded run with
    `cargo run -p rlp -- replay trace.json --opening examples/openings/compose_email.yaml`;
    mismatches are reported per node with output hashes.
  - Knowledge base helpers: `rlp kb migrate`, `rlp kb query "<SQL>"`,
    `rlp kb search <keyword>`, and `rlp kb why <entity>` all operate on the
    local POG databases.
- **`agtop`** – per-agent CPU/RSS/token metrics, error rate.
- **Tracing** – `runloop trace <id>` prints a ladder diagram of crossings.

---

## Repository layout

```text
crates/
  runloopd/      # daemon
  rlp/           # CLI
  agtop/         # TUI monitor
  core/          # shared types & capabilities
  bus/           # local message bus & codecs
  openings/      # opening engine & DSL
  runtime/       # WASM/WASI execution
  rmp/           # message protocol helpers
  kb/            # knowledge base layer
  model-broker/  # provider abstraction & caching
  sdk/           # agent SDK
```

The README lists **`core`**, **`bus`**, and **`openings`** explicitly to match
the workspace plan.

---

## Security & privacy

- Strict capability grants per agent/opening (FS/net/time/kb/secrets).
- **Confirm external actions** (sending, deleting, spending) unless explicitly
  allowed.
- Secrets are referenced by **opaque IDs** and stored in OS keyring or an
  encrypted vault.

---

## Roadmap, contributing, and community

- See [ROADMAP.md](./ROADMAP.md) for phases (Seed → Openings/SDK → KB →
  Reliability/Security → Beta → 1.0).
- CONTRIBUTING, CODE OF CONDUCT, and SECURITY guidelines live in the repo root.
- Please open design questions as “discussions” with links to ADRs.

---

## License

See [LICENSE](./LICENSE).
