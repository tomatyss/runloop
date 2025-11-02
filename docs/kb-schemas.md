# Knowledge Base Schemas (Draft)

> **Doc status:** Draft — schema tables and migration rules are normative for v0.1. Last updated: 2025-11-02.

The Personal Operations Graph (POG) comprises two SQLite databases plus derived vector artifacts.

## 1. Ledger (`events.sqlite`)

- Journal mode: `WAL`
- Synchronous: `FULL`
- Immutable append-only; corrections are emitted as new events.

### 1.1 Tables

```sql
CREATE TABLE IF NOT EXISTS events (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  ts            INTEGER NOT NULL,              -- unix ns
  actor         TEXT NOT NULL,                 -- agent:<id> or user
  kind          TEXT NOT NULL,                 -- e.g. contact.upserted
  scope         TEXT NOT NULL,                 -- user|system|agent:<id>
  payload       BLOB NOT NULL,                 -- msgpack/json
  provenance    BLOB NOT NULL,                 -- msgpack/json (model, inputs hash, etc.)
  hash          TEXT NOT NULL UNIQUE           -- blake3-256(kind|payload|prov)
);

CREATE INDEX IF NOT EXISTS idx_events_kind_ts
  ON events(kind, ts);
CREATE INDEX IF NOT EXISTS idx_events_actor_ts
  ON events(actor, ts);
```

`meta(schema_version TEXT, dirty INTEGER, ts DATETIME)` tracks migrations. `schema_version` uses SemVer.

### 1.2 Constraints

- `payload` and `provenance` must be canonical MsgPack.
- Insertions must occur within transactions; never update/delete existing rows.
- `hash` validation occurs during `rlp kb verify`.

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

- `embeddings (artifact_id INTEGER PRIMARY KEY, dim INTEGER, embedding BLOB, meta JSON)`
- `meta(schema_version TEXT, dirty INTEGER, ts DATETIME)` (mirrors ledger)
- `snapshots(id INTEGER PRIMARY KEY, ts DATETIME, events_high_watermark INTEGER, comment TEXT)`

### 2.2 Rebuild process

Rebuilds iterate over the ledger and repopulate tables; embeddings drive the vector store rebuild. Existing rows are truncated before replay.

## 3. Vector index artifacts

- Primary store lives in `pog.sqlite.embeddings`.
- Derived HNSW files reside under `~/.runloop/pog/vectors/`.
- Rebuild steps:
  1. Clear vector files.
  2. Stream embeddings via `VectorStore::rebuild`.
  3. Validate counts and run probe queries.

## 4. Migration commands

| Command | Purpose |
| ------- | ------- |
| `rlp kb migrate [--inplace]` | Backup, migrate schema, replay views, rebuild vectors. |
| `rlp kb verify` | Hash, referential integrity, schema version checks. |
| `rlp kb backup` | Consistent backup of both DBs. |
| `rlp kb vacuum` | Vacuum/ANALYZE databases (requires exclusive access). |

Migration sets `meta.dirty = 1` during execution; resets to `0` after a successful run.

## 5. Retention & archival

- Default policy retains ledger indefinitely; configure archival via automation (copy rows older than N days to external storage).
- Materialized views follow logical retention (inactive flags) rather than deletions.
- Vector index rebuilds drop entries whose source events are archived.

## 6. Future extensions (informative)

- Additional views (`policies`, `runs`, `cost_usage`) after the model broker lands.
- Alternate backends (e.g., `redb`) behind feature flags without changing the logical schema.
