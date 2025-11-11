# Operations & Packaging Guide (Draft)

> **Doc status:** Draft — normative for migration, trust policy, and config
> precedence. Last updated: 2025-11-02.

This guide covers operational tasks: configuration layering, KB migrations,
vector index maintenance, packaging targets, and trust management.

## 1. Configuration precedence _(normative)_

Runloop merges configuration from several layers. Highest precedence wins unless
a system policy forbids the change.

1. **CLI flags** (`rlp run --model=…`, `:budget …` inline overrides)
2. **Environment variables** (`RUNLOOP_*`)
3. **User config** `~/.runloop/config.yaml`
4. **System config** `/etc/runloop/config.yaml`
5. **Built-in defaults**

### 1.1 Policy overlays

System config may define `policy.*` keys that represent **hard limits** (e.g.,
`policy.max_tokens`, `policy.providers.allowlist`,
`policy.confirm_external_actions = true`). Lower layers may only tighten these
values. Attempts to exceed policy MUST cause the command to fail with a
descriptive error.

### 1.2 Merge semantics

| Type                         | Rule                                                                                                            |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------- |
| Scalars                      | Last writer wins (respecting precedence).                                                                       |
| Maps                         | Deep merge; map entries follow precedence per key.                                                              |
| Lists                        | Replace entirely (last writer). Exceptions: `models.providers` unions entries before applying allow/deny lists. |
| Capability sets / allowlists | Intersect with policy first, then apply precedence.                                                             |

Environment variables mirror YAML paths (upper case, underscores). Examples:

```
RUNLOOP_MODELS_DEFAULT=local:llama3.1-8b
RUNLOOP_MODELS_BUDGETS_SYSTEM_TOKENS_HARD=750000
RUNLOOP_SECURITY_CONFIRM_EXTERNAL_ACTIONS=true
RUNLOOP_CONFIG=/custom/path/config.yaml
```

### 1.3 Model broker configuration _(MVP)_

- `models.broker.providers` lists named backends. `kind` may be `local`, `http` (OpenAI-compatible schemas such as `openai-completions`), or `http_gemini` (Google Gemini `generateContent`). Both HTTP kinds accept `base_url`, `secret_id`, and optional static headers.
- `models.broker.route` is an ordered array of `{ pattern, provider, target_model? }` entries; the first matching pattern wins. Legacy map syntax like `{ "*": "local" }` (or the legacy key `routing`) still deserialises into the same shape.
- `models.broker.cache` exposes `ttl_ms` and `capacity` for the in-memory LRU. Requests may override TTL via `cache_ttl_ms`; `0` disables caching for that call.
- `models.broker.budgets` retains `default_tokens`, `per_request_tokens_cap`, and `hard_cap_usd`. Per-request budgets clamp to the stricter of the request and config-provided values.
- Provider `secret_id` values resolve at runtime via the configured secret store; raw API keys should never be stored in YAML.

### 1.4 Runtime readiness gate _(normative)_

- Agents only become visible to supervisors after a **two-sided readiness
  handshake**: Wasmtime instantiates, the bus mailbox subscribes, tracing
  context is seeded, and the guest either calls the hostcall
  `runloop::notify_ready()` or enters its `mailbox_recv` loop (fallback for
  pre-ready binaries).
- `runtime.spawn_ready_timeout_ms` (default **5000 ms**) controls how long the
  runtime waits for that handshake. Per-agent overrides live in
  `AgentSpec::spawn_ready_timeout_ms`; environment variable
  `RUNLOOP_SPAWN_READY_TIMEOUT_MS` is the lowest-precedence fallback.
- When the timeout elapses, callers receive `Error::ReadyTimeout`, the runtime
  emits `runloop.runtime.spawn.ready_timeouts_total`, and it tears down any
  partially created bus subscriptions/audit state to prevent ghost agents.
- Treat `notify_ready` as part of the minimum agent ABI going forward; older
  agents that cannot be rebuilt should block on `mailbox_recv` immediately so
  the fallback signal still fires.

## 2. Knowledge Base (POG) operations _(normative)_

The POG consists of two SQLite files and a derived vector index.

- `~/.runloop/pog/events.sqlite` — append-only ledger (WAL, synchronous=FULL)
- `~/.runloop/pog/pog.sqlite` — materialized views (WAL, synchronous=NORMAL)
- `~/.runloop/pog/vectors/` — HNSW index files (derived; safe to rebuild)
- `runloopd` runs a background materializer that tails the ledger and updates
  the views. Progress is tracked in
  `pog.sqlite.materializer_state(id INTEGER PRIMARY KEY CHECK (id = 1), watermark INTEGER NOT NULL)`.

### 2.1 Migration workflow

`rlp kb migrate` orchestrates upgrades across both stores.

1. Ensure `runloopd` is stopped (command refuses to run if sockets are open;
   override with `--force`).
2. Create timestamped backups of both DBs.
3. Apply schema migrations to `events.sqlite` (rare; append-only).
4. Rebuild `pog.sqlite` by replaying events (`events.sqlite` → views). Use
   `--inplace` only for emergency SQL patches.
5. Rebuild vector index using the `VectorStore::rebuild` path.
6. Set `meta.dirty = 0`, record new `schema_version`, and create a `snapshots`
   entry.
7. Update `materializer_state.watermark` with the highest applied ledger id.

Supporting commands:

- `rlp kb verify` — referential integrity, hashes, BLAKE3 checks
- `rlp kb backup` — consistent hot backup (uses SQLite backup APIs)
- `rlp kb vacuum` — optional compaction (requires exclusive lock)
- `rlp kb why <entity>` — print ordered source events for a materialized entity
  key.

### 2.2 Metadata tables

Both databases include `meta(schema_version TEXT, dirty INTEGER, ts DATETIME)`.
`pog.sqlite` also tracks
`snapshots(id INTEGER PRIMARY KEY, ts DATETIME, events_high_watermark INTEGER, comment TEXT)`.

### 2.3 Retention

- Ledger retains all events; corrections produce new `StateDelta` entries.
- Operators can archive older events by copying subsets elsewhere; never delete
  rows in-place.
- Materialized views compact automatically during rebuild; configure retention
  by emitting `StateDelta` events that mark artifacts/contacts inactive.

## 3. Vector index lifecycle _(normative)_

- Implementation milestone 1 uses a pure-Rust HNSW crate (`hnsw_rs` class).
  Keyword search uses SQLite FTS5; results fuse via Reciprocal Rank Fusion
  (RRF).
- Embeddings are stored in `pog.sqlite` (blob column) with metadata. The vector
  index is derived and can be discarded/rebuilt.
- `VectorStore` trait (conceptual):

```rust
trait VectorStore {
  fn upsert(&self, id: ItemId, embedding: &[f32], meta: &Meta) -> Result<()>;
  fn delete(&self, id: ItemId) -> Result<()>;
  fn search(&self, q: &[f32], k: usize, filter: &MetaFilter) -> Result<Vec<Hit>>;
  fn rebuild(&self, iter: impl Iterator<Item = (ItemId, Embedding, Meta)>) -> Result<()>;
}
```

- Provenance filters (`confirmed_only`, `agent_allowlist`) run before final
  scoring.
- Future milestone may integrate Tantivy; implementations must conform to the
  same trait.

## 4. Packaging targets _(informative)_

| Artifact        | Location                                      | Status                                             |
| --------------- | --------------------------------------------- | -------------------------------------------------- |
| Debian packages | `packaging/systemd/` + `packaging/container/` | WIP — templates only.                              |
| Live ISO        | `packaging/live-build/`                       | Folders exist; scripts TBD after `.deb` packaging. |
| Dev container   | `packaging/container/`                        | README tracks mounts, base image expectations.     |

No build scripts exist yet; add them once runtime crates compile.

## 5. Trust policy & agent signatures _(normative)_

Runloop enforces signatures on agent bundles before install/launch.

- **Algorithm:** Ed25519 detached signature over `manifest.toml` (canonicalized)
  and referenced files.
- **Bundle layout:**

```
agent.bundle/
├─ manifest.toml       # includes digests of contents
├─ policy.caps
├─ agent.wasm
├─ schemas/… (optional)
├─ SBOM/spdx.json (optional)
└─ SIGNATURES/manifest.sig
```

- **Trust policy file:** `~/.runloop/trust-policy.toml`

```toml
[anchors]
runloop_release = "ed25519:ABCD…"
dev = { key = "ed25519:DEAD…", allow_dev = true }

[rules]
runloop_release = { allow_caps = "any", allow_net = "any", allow_exec = false }
dev = { allow_caps = ["kb_read", "kb_write"], allow_net = [], allow_exec = false }
```

- **Lifecycle:**
  - First-party releases signed with Runloop Release key (private material
    stored outside repo).
  - Third-party vendors sign with their key; operators add the corresponding
    anchor.
  - `rlp trust update` fetches keysets/CRLs.
  - Install flow: `rlp agent install bundle.tar` → verify signature → enforce
    trust policy → stage bundle.
  - Launch flow re-verifies manifest + signature as defense in depth.

- **Revocation:** increment keyset version or publish revocation list; runtime
  refuses to start bundles signed by revoked keys.

## 6. Secrets backends _(summary)_

See `docs/security-model.md` for secret-store details. Ops tasks:

- `rlp secrets init --backend=secret-service|pass|age`
- `rlp secrets put runloop/mail/smtp_api_key` (reads from stdin)
- `rlp secrets list` and `rlp secrets delete` for maintenance

## 7. Observability _(summary)_

- Default logging: JSON (ndjson) with keys `ts`, `level`, `service.name`,
  `trace_id`, `opening_id`, `agent_id`.
- Tracing & metrics via OpenTelemetry OTLP. Configure endpoint, protocol, and
  sampling under `observability` in config.
- Model broker exports `runloop_broker_calls_total`,
  `runloop_broker_cache_hits_total`, and `runloop_broker_errors_total{kind=*}`
  counters for dashboards.
- `agtop` pane + `rlp trace` rely on the metrics exported by agents.

## 8. Message bus topics _(normative)_

- Only UI/TUI processes may publish `action.decision`; the bus rejects other
  publishers and emits an audit event.
- The runtime publishes drop notices (`DropNotice`) on `rlp/sys/drops` whenever
  TTL expiry or duplicate suppression occurs. Operators should scrape this topic
  for reliability dashboards.

### 8.1 Bus publisher ACL (configuration)

Configure publisher kinds allowed to emit specific schemas:

```yaml
bus:
  auth:
    publishers:
      action_decision:
        allowed_kinds: ["ui", "tui"]
```

Defaults permit only `ui` and `tui`. Publishers establish identity at connect
time (`connect_as`).

## Appendix A. Repo admin checklist

### Branch protection (owner: @release-eng)

- Protect `main`: require PRs, 1+ code owner review, dismiss stale reviews on
  changes.
- Require status checks: build, test, clippy, fmt, docs-check, commitlint.
- Require branch to be up to date before merging.
- Disallow force-push to `main`.

### Security features (owner: @release-eng)

- Enable Dependabot alerts & updates.
- Enable secret scanning & push protection.
- Enable code scanning (CodeQL or equivalent).

### Labels (owner: @pm)

- Create: bug, feature, task, docs, infra, security, design, good-first-issue,
  epic, phase:g.

### CI secrets (owner: @release-eng)

- `CRATES_IO_TOKEN` (future), signing keys, release GPG key (optional).

### Release gates (owner: @pm, @release-eng)

- Tag pattern `v0.x.y`.
- Required checks green.
- CHANGELOG updated.
- SBOM/signatures attached (when implemented).

---

**Further reading:**

- [`docs/message-protocol.md`](message-protocol.md)
- [`docs/rmp-registry.md`](rmp-registry.md)
- [`docs/security-model.md`](security-model.md)
- [`docs/kb-schemas.md`](kb-schemas.md)
