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
git clone https://github.com/tomatyss/runloop.git
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
rlp run ... > run.ndjson
cargo run -p agtop -- --input run.ndjson

# inspect resolved config layers
cargo run -p rlp -- config path --all
```

> **Note:** `rlp run` now probes the daemon socket before doing any local work.
> Provide `--local` explicitly when you want inline execution; both modes stream
> NDJSON `RunEvent` records so monitors such as `agtop` can consume the same
> schema (pipe `rlp run ... > run.ndjson` for live monitoring or feed stdin).

### Agent bundles (wasm)

The canonical `compose_email` agents now ship as wasm32-wasip1 bundles generated
from the helper crates under `crates/agents-wasm/*` (kept outside the default
workspace so host builds remain fast). Run `just build-agents-wasm` to
cross-compile the binaries, copy them into `agents/*/bin/*.wasm`, and refresh
the manifest digests. This command requires the `wasm32-wasip1` target to be
installed (`rustup target add wasm32-wasip1`). Each bundle is a self-contained
CLI that prints JSON to stdout, which the runtime captures when executing an
opening. Use `just test-agents-wasm` to rebuild the bundles (if needed) and run
`compose_email` end-to-end via `rlp --local` as a smoke test. When new bundles
are produced, commit the `.wasm` artifacts plus their updated BLAKE3 digests so
the manifests continue to verify.

Scaffold your own agent and run the starter opening:

```bash
rlp agent scaffold note_taker --opening
rlp agent build note_taker
cargo run -p rlp -- run examples/openings/note_taker.yaml --local \
  --params '{"prompt":"draft a standup note"}'
```

Install a prebuilt bundle into a registry dir (directory or `.tar`/`.tar.gz`):

```bash
rlp agent install /path/to/agent.bundle.tar
```

### Packages & images (daemon / system mode)

When installed from a .deb or image, the service runs as **`runloop:runloop`**
and writes state under **`/var/lib/runloop`**; its UDS socket lives at
`/run/runloop/rmp.sock` (owned by `runloop:runloop`, mode `0660`). User mode continues to use `~/.runloop` for
config/artifacts. Runtime socket discovery precedence:

1. `runtime.socket_path` (short‑circuit; error if unreachable)
2. `${runtime.sockets_dir}/rmp.sock`
3. `~/.runloop/sock/rmp.sock`
4. `/run/runloop/rmp.sock`

#### Debian 13 (`trixie`) packages

Build the .deb via `dpkg-buildpackage` (convenience target provided):

```bash
just deb
# artifacts land in ../runloop_<version>_<arch>.deb
```

Install and manage the daemon:

```bash
sudo apt install ../runloop_0.1.0~alpha1-1_amd64.deb
sudo systemctl status runloopd
sudo systemctl restart runloopd   # when updating /etc/runloop/config.yaml
```

The package ships `runloopd`, `rlp`, and `agtop`, configures the `runloop`
system user, and writes state under `/var/lib/runloop`. Remove with
`sudo apt purge runloop` to drop both configuration and data.
Default agent bundles are installed under `/usr/lib/runloop/agents`, with
writable copies seeded under `/var/lib/runloop/agents` (refreshed on upgrade
only when the seeded directory is unchanged). The `compose_email` opening is installed at
`/etc/runloop/openings/compose_email.yaml` and copied to
`/var/lib/runloop/openings/compose_email.yaml` for easy editing.

---

## Configuration (Config v1)

Create `~/.runloop/config.yaml` for user mode, or `/etc/runloop/config.yaml` for
system mode:

```yaml
version: 1

runtime:
  base: "debian"
  agent_container: "wasm32-wasip1"

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
  events_db: "events.sqlite" # append-only event log
  view_db: "pog.sqlite" # materialized views

logging:
  level: "info" # error | warn | info | debug | trace
  format: "auto" # auto | json | text (auto picks JSON when stdout is not a TTY)
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
`runtime.sockets_dir` is used with implied filename `rmp.sock`. Defaults for
user mode favor `~/.runloop/sock/rmp.sock`; system mode uses
`/run/runloop/rmp.sock`.

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

**RMP v0 is frozen**: stream transports carry a `u32 frame_len` prefix, a fixed
64-byte header, and a MsgPack body (`frame_len = header_len + body_len`). All
integers are big-endian; anything else is rejected.

<!-- markdownlint-disable MD013 -->

| Offset | Size | Field            | Notes                                                  |
| -----: | ---: | ---------------- | ------------------------------------------------------ |
|      0 |    4 | `magic`          | ASCII `"RMP0"`                                         |
|      4 |    2 | `header_version` | `0` only; mismatch → `UnsupportedVersion`              |
|      6 |    2 | `header_len`     | `64`; compare literally                                |
|      8 |    4 | `flags`          | MUST be `0` in v0; otherwise `InvalidHeaderFlags`      |
|     12 |    2 | `schema_id`      | Primitive family ID (see `docs/rmp-registry.md`)       |
|     14 |    2 | `reserved2`      | MUST be `0`                                            |
|     16 |    4 | `body_len`       | Length of MsgPack body                                 |
|     20 |    8 | `created_at_ms`  | Sender clock (epoch ms)                                |
|     28 |    8 | `ttl_ms`         | Relative TTL; `0` rejected, overflow → `InvalidExpiry` |
|     36 |   16 | `trace_id`       | u128 trace for dedupe/telemetry                        |
|     52 |    8 | `msg_id`         | u64 monotonic per publisher                            |
|     60 |    4 | `reserved4`      | MUST be `0`                                            |

<!-- markdownlint-enable MD013 -->

**Body envelope.** MsgPack map
`{ "type": "<family.kind.vN>", "payload": <object>, "meta"?: <map> }`.
`schema_id` picks the primitive family (Observation, Intent, Artifact,
ToolResult, Critique, StateDelta, ErrorReport, etc.); the body `type` string is
the registry entry (e.g., `"error.report.v1"`). Implementations MUST cross-check
family ↔ kind (`BodyTypeMismatch` on failure). `meta` is optional and
forward-compatible; `opening_id`, `priority`, and diagnostics live here—not in
the fixed header.

**Framing & safety.** `frame_len` MUST equal `header_len + body_len` or the
frame is dropped with `LengthMismatch`. TTL uses u128 math (`InvalidTtl` when 0,
`InvalidExpiry` on overflow); receivers drop messages once `now >= expires_at`.
Dedupe caches key `(trace_id, msg_id)` per (topic + subscriber); `Duplicate`
drops, TTL expirations, and back-pressure timeouts increment drop counters and
publish `rlp/sys/drops {reason, topic, trace_id, msg_id, expires_at_ms?}`
(rate-limited).

**Limits.** Default body cap is **8 MiB** (`BodyTooLarge`). Unknown `schema_id`
is rejected; non-zero flags/reserved words throw `InvalidHeaderFlags`. MsgPack
failures surface as `BodyDecodeError`. Implementations must treat the error
taxonomy (`InvalidMagic`, `UnsupportedVersion`, `TruncatedHeader`,
`InvalidHeaderFlags`, `LengthMismatch`, `UnknownSchema`, `BodyTooLarge`,
`InvalidTtl`, `InvalidExpiry`, `Expired`, `Duplicate`, `BodyDecodeError`,
`BodyTypeMismatch`) as normative test cases. See
[`docs/message-protocol.md`](docs/message-protocol.md) for the frozen spec,
hex-dump golden vector, and TTL/duplicate walkthrough.

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
  - Route prompts programmatically with
    `cargo run -p rlp -- route "draft email"` (or `--stdin` to read the buffer).
    The command prints JSON like
    `{ "version": 1, "route": "agent", "rule": "fallback:opening", "blocked": false }`
    and exits `10` for shell decisions or `11` for agent decisions so shells can
    branch without parsing stdout.
  - See [`docs/router-shell.md`](docs/router-shell.md) for opt-in shell
    integration (zsh/bash widgets, env toggles, and the `rlp shell enable`
    helper).
  - Run an Opening locally with
    `cargo run -p rlp -- run examples/openings/compose_email.yaml --params '{"recipient":"john","topic":"Q4 plan"}' --trace-out trace.json`.
    The command now drives the full compose-email stack (contact resolver →
    context gatherer → writer → critic → mailer), prints per-node status, and
    writes a replayable trace whether the run executes inline or via the daemon
    (daemon mode pulls the canonical `run.trace` from the KB once it is
    persisted). Make sure `runloop.json` points to a writable KB folder, that
    the model broker has at least one provider (or rely on the writer's
    heuristic fallback), and export any provider secrets to the environment so
    the CLI secret resolver can read them (either the exact `secret_id` or its
    upper-snake variant such as `RUNLOOP_MODELS_GEMINI`). Mail send still runs
    as a dry-run and prompts for approval unless
    `security.confirm_external_actions=false`.
  - Replay a recorded run with either a stored trace ID or a JSON file:
    `cargo run -p rlp -- replay trace:<trace_uuid> --opening examples/openings/compose_email.yaml`
    pulls the canonical `run.trace` payload from the KB, while passing a file
    path (e.g. `trace.json`) keeps the previous developer workflow. Mismatches
    are reported per node with output hashes.
  - Knowledge base helpers: `rlp kb migrate`, `rlp kb query "<SQL>"`,
    `rlp kb search <keyword>`, and `rlp kb why <entity>` all operate on the
    local POG databases.
- **`agtop`** – live NDJSON TUI; point it at the `rlp run` stream to watch
  per-node status.
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
