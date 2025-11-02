# Operations & Packaging Guide (Draft)

> **Doc status:** Draft — normative for migration, trust policy, and config precedence. Last updated: 2025-11-02.

This guide covers operational tasks: configuration layering, KB migrations, vector index maintenance, packaging targets, and trust management.

## 1. Configuration precedence _(normative)_

Runloop merges configuration from several layers. Highest precedence wins unless a system policy forbids the change.

1. **CLI flags** (`rlp run --model=…`, `:budget …` inline overrides)
2. **Environment variables** (`RUNLOOP_*`)
3. **User config** `~/.runloop/config.yaml`
4. **System config** `/etc/runloop/config.yaml`
5. **Built-in defaults**

### 1.1 Policy overlays

System config may define `policy.*` keys that represent **hard limits** (e.g., `policy.max_tokens`, `policy.providers.allowlist`, `policy.confirm_external_actions = true`). Lower layers may only tighten these values. Attempts to exceed policy MUST cause the command to fail with a descriptive error.

### 1.2 Merge semantics

| Type | Rule |
| ---- | ---- |
| Scalars | Last writer wins (respecting precedence). |
| Maps | Deep merge; map entries follow precedence per key. |
| Lists | Replace entirely (last writer). Exceptions: `models.providers` unions entries before applying allow/deny lists. |
| Capability sets / allowlists | Intersect with policy first, then apply precedence. |

Environment variables mirror YAML paths (upper case, underscores). Examples:

```
RUNLOOP_MODELS_DEFAULT=local:llama3.1-8b
RUNLOOP_MODELS_BUDGETS_SYSTEM_TOKENS_HARD=750000
RUNLOOP_SECURITY_CONFIRM_EXTERNAL_ACTIONS=true
RUNLOOP_CONFIG=/custom/path/config.yaml
```

## 2. Knowledge Base (POG) operations _(normative)_

The POG consists of two SQLite files and a derived vector index.

- `~/.runloop/pog/events.sqlite` — append-only ledger (WAL, synchronous=FULL)
- `~/.runloop/pog/pog.sqlite` — materialized views (WAL, synchronous=NORMAL)
- `~/.runloop/pog/vectors/` — HNSW index files (derived; safe to rebuild)

### 2.1 Migration workflow

`rlp kb migrate` orchestrates upgrades across both stores.

1. Ensure `runloopd` is stopped (command refuses to run if sockets are open; override with `--force`).
2. Create timestamped backups of both DBs.
3. Apply schema migrations to `events.sqlite` (rare; append-only).
4. Rebuild `pog.sqlite` by replaying events (`events.sqlite` → views). Use `--inplace` only for emergency SQL patches.
5. Rebuild vector index using the `VectorStore::rebuild` path.
6. Set `meta.dirty = 0`, record new `schema_version`, and create a `snapshots` entry.

Supporting commands:

- `rlp kb verify` — referential integrity, hashes, BLAKE3 checks
- `rlp kb backup` — consistent hot backup (uses SQLite backup APIs)
- `rlp kb vacuum` — optional compaction (requires exclusive lock)

### 2.2 Metadata tables

Both databases include `meta(schema_version TEXT, dirty INTEGER, ts DATETIME)`. `pog.sqlite` also tracks `snapshots(id INTEGER PRIMARY KEY, ts DATETIME, events_high_watermark INTEGER, comment TEXT)`.

### 2.3 Retention

- Ledger retains all events; corrections produce new `StateDelta` entries.
- Operators can archive older events by copying subsets elsewhere; never delete rows in-place.
- Materialized views compact automatically during rebuild; configure retention by emitting `StateDelta` events that mark artifacts/contacts inactive.

## 3. Vector index lifecycle _(normative)_

- Implementation milestone 1 uses a pure-Rust HNSW crate (`hnsw_rs` class). Keyword search uses SQLite FTS5; results fuse via Reciprocal Rank Fusion (RRF).
- Embeddings are stored in `pog.sqlite` (blob column) with metadata. The vector index is derived and can be discarded/rebuilt.
- `VectorStore` trait (conceptual):

```rust
trait VectorStore {
  fn upsert(&self, id: ItemId, embedding: &[f32], meta: &Meta) -> Result<()>;
  fn delete(&self, id: ItemId) -> Result<()>;
  fn search(&self, q: &[f32], k: usize, filter: &MetaFilter) -> Result<Vec<Hit>>;
  fn rebuild(&self, iter: impl Iterator<Item = (ItemId, Embedding, Meta)>) -> Result<()>;
}
```

- Provenance filters (`confirmed_only`, `agent_allowlist`) run before final scoring.
- Future milestone may integrate Tantivy; implementations must conform to the same trait.

## 4. Packaging targets _(informative)_

| Artifact | Location | Status |
| -------- | -------- | ------ |
| Debian packages | `packaging/systemd/` + `packaging/container/` | WIP — templates only. |
| Live ISO | `packaging/live-build/` | Folders exist; scripts TBD after `.deb` packaging. |
| Dev container | `packaging/container/` | README tracks mounts, base image expectations. |

No build scripts exist yet; add them once runtime crates compile.

## 5. Trust policy & agent signatures _(normative)_

Runloop enforces signatures on agent bundles before install/launch.

- **Algorithm:** Ed25519 detached signature over `manifest.toml` (canonicalized) and referenced files.
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
  - First-party releases signed with Runloop Release key (private material stored outside repo).
  - Third-party vendors sign with their key; operators add the corresponding anchor.
  - `rlp trust update` fetches keysets/CRLs.
  - Install flow: `rlp agent install bundle.tar` → verify signature → enforce trust policy → stage bundle.
  - Launch flow re-verifies manifest + signature as defense in depth.

- **Revocation:** increment keyset version or publish revocation list; runtime refuses to start bundles signed by revoked keys.

## 6. Secrets backends _(summary)_

See `docs/security-model.md` for secret-store details. Ops tasks:

- `rlp secrets init --backend=secret-service|pass|age`
- `rlp secrets put runloop/mail/smtp_api_key` (reads from stdin)
- `rlp secrets list` and `rlp secrets delete` for maintenance

## 7. Observability _(summary)_

- Default logging: JSON (ndjson) with keys `ts`, `level`, `service.name`, `trace_id`, `opening_id`, `agent_id`.
- Tracing & metrics via OpenTelemetry OTLP. Configure endpoint, protocol, and sampling under `observability` in config.
- `agtop` pane + `rlp trace` rely on the metrics exported by agents.

## Appendix A. Repo admin checklist

### Branch protection (owner: @release-eng)
- Protect `main`: require PRs, 1+ code owner review, dismiss stale reviews on changes.
- Require status checks: build, test, clippy, fmt, docs-check, commitlint.
- Require branch to be up to date before merging.
- Disallow force-push to `main`.

### Security features (owner: @release-eng)
- Enable Dependabot alerts & updates.
- Enable secret scanning & push protection.
- Enable code scanning (CodeQL or equivalent).

### Labels (owner: @pm)
- Create: bug, feature, task, docs, infra, security, design, good-first-issue, epic, phase:g.

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
