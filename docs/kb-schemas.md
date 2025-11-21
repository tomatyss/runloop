# Knowledge Base Schemas (Draft)

> **Doc status:** Draft — schema tables and migration rules are normative for
> v0.1. Last updated: 2025-11-02.

The Personal Operations Graph (POG) comprises two SQLite databases plus derived
vector artifacts.

## 1. Ledger (`events.sqlite`)

- Journal mode: `WAL`
- Synchronous: `FULL`
- Immutable append-only; corrections are emitted as new events.

### 1.1 Tables

```sql
CREATE TABLE IF NOT EXISTS events (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  ts_ms         INTEGER NOT NULL,              -- unix time in milliseconds
  actor         TEXT NOT NULL,                 -- agent:<id> or user
  kind          TEXT NOT NULL,                 -- e.g. contact.upserted
  scope         TEXT NOT NULL,                 -- user|system|agent:<id>
  payload_json  TEXT NOT NULL,                 -- JCS canonical JSON blob
  provenance_json TEXT NOT NULL,               -- JCS canonical JSON
                                               -- (model inputs, hash, etc.)
  hash_blake3   BLOB NOT NULL UNIQUE           -- BLAKE3 digest of canonical envelope
);

CREATE INDEX IF NOT EXISTS idx_events_kind_ts
  ON events(kind, ts_ms);
CREATE INDEX IF NOT EXISTS idx_events_actor_ts
  ON events(actor, ts_ms);
```

`meta(schema_version TEXT, dirty INTEGER, ts DATETIME)` tracks migrations.
`schema_version` uses SemVer.

### 1.2 Constraints

- `payload_json` and `provenance_json` MUST be
  [JCS](https://www.rfc-editor.org/rfc/rfc8785) canonical JSON strings.
- Insertions must occur within transactions; never update/delete existing rows.
- `hash_blake3` validation occurs during `rlp kb verify`; the digest is stored
  as raw BLOB(32) and rendered in hex only for logs/UIs.

### 1.3 Seed JSON schemas

Normative JSON Schemas for the initial event kinds live under
[`crates/kb/schemas/`](../crates/kb/schemas/):

<!-- markdownlint-disable MD013 -->

| Kind               | Schema                                            |
| ------------------ | ------------------------------------------------- |
| `contact.upserted` | [`contact.upserted.schema.json`][schema-contact]  |
| `artifact.created` | [`artifact.created.schema.json`][schema-artifact] |
| `email.sent`       | [`email.sent.schema.json`][schema-email]          |
| `run.started`      | [`run.started.schema.json`][schema-run-started]   |
| `run.finished`     | [`run.finished.schema.json`][schema-run-finished] |
| `node.finished`    | [`node.finished.schema.json`][schema-node]        |
| `run.trace`        | [`run.trace.schema.json`][schema-trace]           |
| `cap.audit`        | [`cap.audit.schema.json`][schema-cap-audit]       |

<!-- markdownlint-enable MD013 -->

[schema-contact]: ../crates/kb/schemas/contact.upserted.schema.json
[schema-artifact]: ../crates/kb/schemas/artifact.created.schema.json
[schema-email]: ../crates/kb/schemas/email.sent.schema.json
[schema-run-started]: ../crates/kb/schemas/run.started.schema.json
[schema-run-finished]: ../crates/kb/schemas/run.finished.schema.json
[schema-node]: ../crates/kb/schemas/node.finished.schema.json
[schema-trace]: ../crates/kb/schemas/run.trace.schema.json
[schema-cap-audit]: ../crates/kb/schemas/cap.audit.schema.json

Validators MUST embed these schemas and enforce them at `StateDelta` ingestion
time. `$id` versions bump on breaking changes; implementations SHOULD accept the
latest compatible revision.

## 2. Materialized views (`pog.sqlite`)

- Journal mode: `WAL`
- Synchronous: `NORMAL`

### 2.1 Tables

```sql
CREATE TABLE IF NOT EXISTS contacts (
  id            INTEGER PRIMARY KEY,
  external_id   TEXT,
  name          TEXT,
  email         TEXT,
  org           TEXT,
  trust         REAL DEFAULT 0.5,
  source_event  INTEGER NOT NULL REFERENCES events(id)
);

CREATE INDEX IF NOT EXISTS idx_contacts_email ON contacts(email);

CREATE TABLE IF NOT EXISTS accounts (
  id            INTEGER PRIMARY KEY,
  kind          TEXT,
  handle        TEXT,
  auth_ref      TEXT,
  scopes        TEXT,
  verified      INTEGER DEFAULT 0,
  source_event  INTEGER NOT NULL REFERENCES events(id)
);

CREATE TABLE IF NOT EXISTS artifacts (
  id            INTEGER PRIMARY KEY,
  kind          TEXT,
  path          TEXT,
  sha256        TEXT,
  summary       TEXT,
  source_event  INTEGER NOT NULL REFERENCES events(id)
);
CREATE INDEX IF NOT EXISTS idx_artifacts_kind_ts
  ON artifacts(kind, source_event);

CREATE TABLE IF NOT EXISTS edges (
  from_id       INTEGER NOT NULL,
  to_id         INTEGER NOT NULL,
  kind          TEXT NOT NULL,
  source_event  INTEGER NOT NULL REFERENCES events(id)
);
CREATE INDEX IF NOT EXISTS idx_edges_from_kind
  ON edges(from_id, kind, to_id);
```

Supplemental tables:

- `embeddings`:
  - `artifact_id INTEGER PRIMARY KEY`
  - `dim INTEGER`
  - `embedding BLOB`
  - `meta JSON`
- `meta(schema_version TEXT, dirty INTEGER, ts DATETIME)` (mirrors ledger)
- `snapshots`:
  - `id INTEGER PRIMARY KEY`
  - `ts DATETIME`
  - `events_high_watermark INTEGER`
  - `comment TEXT`

### 2.2 Rebuild process

Rebuilds iterate over the ledger and repopulate tables; embeddings drive the
vector store rebuild. Existing rows are truncated before replay.

## 3. Vector index artifacts

- Primary store lives in `pog.sqlite.embeddings`.
- Derived HNSW files reside under `~/.runloop/pog/vectors/`.
- Rebuild steps:
  1. Clear vector files.
  2. Stream embeddings via `VectorStore::rebuild`.
  3. Validate counts and run probe queries.

## 4. Migration commands

<!-- markdownlint-disable MD013 -->

| Command                      | Purpose                                                |
| ---------------------------- | ------------------------------------------------------ |
| `rlp kb migrate [--inplace]` | Backup, migrate schema, replay views, rebuild vectors. |
| `rlp kb verify`              | Hash, referential integrity, schema version checks.    |
| `rlp kb backup`              | Consistent backup of both DBs.                         |
| `rlp kb vacuum`              | Vacuum/ANALYZE databases (requires exclusive access).  |

<!-- markdownlint-enable MD013 -->

Migration sets `meta.dirty = 1` during execution; resets to `0` after a
successful run.

## 5. Retention & archival

- Default policy retains ledger indefinitely; configure archival via automation
  (copy rows older than N days to external storage).
- Materialized views follow logical retention (inactive flags) rather than
  deletions.
- Vector index rebuilds drop entries whose source events are archived.

## 6. Future extensions (informative)

- Additional views (`policies`, `runs`, `cost_usage`) after the model broker
  lands.
- Alternate backends (e.g., `redb`) behind feature flags without changing the
  logical schema. `cap.audit` events capture per-hostcall capability decisions.
  Each payload stores the agent label, capability family (e.g., `time.now`),
  operation name, target string, the BLAKE3 hash of the serialized arguments
  (`args_hash` hex), decision (`allow|deny`), severity, and a short `reason`
  code. Events are written whenever the runtime's audit policy enables the
  corresponding decision stream (see `security.caps.audit_on_allow` /
  `audit_on_deny` in config).
