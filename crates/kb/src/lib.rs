//! Knowledge base access layer backed by SQLite event logs and materialized views.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use json_canon::to_string as jcs_to_string;
use jsonschema::{JSONSchema, error::ValidationError};
use parking_lot::Mutex;
use runloop_core::config::KbConfig;
use runloop_core::ids::{AgentId, EventId, TraceId};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row, Statement, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

/// Canonical KB error type.
#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("schema not found for kind {0}")]
    UnknownSchema(String),
    #[error("payload validation failed: {0}")]
    Schema(String),
    #[error("invalid scope '{0}'")]
    InvalidScope(String),
    #[error("referential integrity failed for evidence id {0}")]
    MissingEvidence(i64),
    #[error("canonicalization error: {0}")]
    Canon(String),
    #[error("query must be read-only")]
    QueryNotReadOnly,
}

/// Shared knowledge base handle.
#[derive(Clone)]
pub struct KnowledgeBase {
    events: Arc<Mutex<Connection>>,
    views: Arc<Mutex<Connection>>,
    schemas: Arc<SchemaRegistry>,
    cap_audits: Arc<Mutex<Vec<CapAuditRecord>>>,
}

impl KnowledgeBase {
    /// Construct an in-memory knowledge base (primarily for tests).
    pub fn new() -> Self {
        Self::new_in_memory().expect("in-memory knowledge base")
    }

    fn new_in_memory() -> Result<Self, Error> {
        let events = Connection::open_in_memory()?;
        configure_events(&events)?;
        apply_events_migrations(&events)?;

        let views = Connection::open_in_memory()?;
        configure_views(&views)?;
        apply_views_migrations(&views)?;

        Ok(Self {
            events: Arc::new(Mutex::new(events)),
            views: Arc::new(Mutex::new(views)),
            schemas: Arc::new(SchemaRegistry::load()?),
            cap_audits: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Open or create the knowledge base rooted at the provided configuration.
    pub fn open(config: &KbConfig) -> Result<Self, Error> {
        let (root_dir, events_path, views_path) = resolve_paths(config)?;
        fs::create_dir_all(&root_dir)?;

        let events = Connection::open_with_flags(
            &events_path,
            OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        configure_events(&events)?;
        apply_events_migrations(&events)?;

        let views = Connection::open_with_flags(
            &views_path,
            OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        configure_views(&views)?;
        apply_views_migrations(&views)?;

        Ok(Self {
            events: Arc::new(Mutex::new(events)),
            views: Arc::new(Mutex::new(views)),
            schemas: Arc::new(SchemaRegistry::load()?),
            cap_audits: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Run idempotent migrations across both databases (ledger + views).
    pub fn migrate(&self) -> Result<(), Error> {
        {
            let conn = self.events.lock();
            configure_events(&conn)?;
            apply_events_migrations(&conn)?;
        }
        {
            let conn = self.views.lock();
            configure_views(&conn)?;
            apply_views_migrations(&conn)?;
        }
        Ok(())
    }

    /// Insert an event derived from a `StateDelta` and return the created `EventId`.
    pub fn propose(&self, delta: StateDelta) -> Result<EventId, Error> {
        let scope = delta.scope();
        validate_scope(scope)?;

        let schema = self
            .schemas
            .get(&delta.kind)
            .ok_or_else(|| Error::UnknownSchema(delta.kind.clone()))?;

        if let Err(errors) = schema.validate(&delta.payload) {
            let message = errors
                .map(render_schema_error)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Schema(message));
        }
        validate_referential_integrity(&self.events.lock(), &delta)?;

        let payload_canonical = canonicalize_value(&delta.payload)?;
        let provenance_value = delta.provenance.to_value();
        let provenance_canonical = canonicalize_value(&provenance_value)?;

        let envelope_value = json!({
            "kind": delta.kind.clone(),
            "actor": delta.actor.clone(),
            "scope": scope,
            "payload": delta.payload.clone(),
            "provenance": provenance_value
        });
        let envelope_canonical = canonicalize_value(&envelope_value)?;
        let hash = blake3::hash(envelope_canonical.as_bytes());

        let ts_ms = timestamp_ms();
        let mut conn = self.events.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO events (ts_ms, actor, kind, scope, payload_json, provenance_json, hash_blake3)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ts_ms,
                &delta.actor,
                &delta.kind,
                scope,
                payload_canonical,
                provenance_canonical,
                hash.as_bytes()
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(EventId(id))
    }

    /// Execute a read-only SQL query against the materialized view database.
    pub fn query(&self, sql: &str) -> Result<QueryResult, Error> {
        let conn = self.views.lock();
        let mut stmt = conn.prepare(sql)?;
        if !stmt.readonly() {
            return Err(Error::QueryNotReadOnly);
        }
        let column_names = stmt
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let rows = collect_rows(&mut stmt, &column_names)?;
        Ok(QueryResult {
            columns: column_names,
            rows,
        })
    }

    /// Perform a keyword search across common domains (contacts, artifacts, events, runs).
    pub fn search(&self, keyword: &str) -> Result<Vec<SearchHit>, Error> {
        let like = format!("%{}%", keyword);
        let mut hits = Vec::new();

        let views = self.views.lock();
        {
            let mut stmt = views.prepare(
                "SELECT contact_key, name, email, org, trust, source_event
                 FROM contacts
                 WHERE name LIKE ?1 OR email LIKE ?1 OR org LIKE ?1",
            )?;
            let rows = stmt.query_map(params![&like], |row| {
                Ok(SearchHit::contact(
                    row.get(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?;
            for hit in rows {
                hits.push(hit?);
            }
        }

        {
            let mut stmt = views.prepare(
                "SELECT artifact_key, kind, path, summary, source_event
                 FROM artifacts
                 WHERE kind LIKE ?1 OR path LIKE ?1 OR summary LIKE ?1",
            )?;
            let rows = stmt.query_map(params![&like], |row| {
                Ok(SearchHit::artifact(
                    row.get(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?;
            for hit in rows {
                hits.push(hit?);
            }
        }

        {
            let mut stmt = views.prepare(
                "SELECT trace_id, status, opening_id, source_event
                 FROM runs
                 WHERE trace_id LIKE ?1 OR status LIKE ?1 OR opening_id LIKE ?1",
            )?;
            let rows = stmt.query_map(params![&like], |row| {
                Ok(SearchHit::run(
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for hit in rows {
                hits.push(hit?);
            }
        }
        drop(views);

        let events_conn = self.events.lock();
        {
            let mut stmt = events_conn.prepare(
                "SELECT id, kind, ts_ms
                 FROM events
                 WHERE kind LIKE ?1",
            )?;
            let rows = stmt.query_map(params![&like], |row| {
                Ok(SearchHit::event(
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for hit in rows {
                hits.push(hit?);
            }
        }

        Ok(hits)
    }

    /// Return ordered source events associated with a domain key.
    pub fn why(&self, key: &str) -> Result<Vec<EventRecord>, Error> {
        let candidate_keys = entity_key_candidates(key);

        let conn = self.views.lock();
        let mut stmt = conn.prepare(
            "SELECT event_id
             FROM entity_history
             WHERE entity_key = ?1
             ORDER BY event_id ASC",
        )?;
        let mut event_ids = Vec::new();
        for candidate in candidate_keys {
            let rows = stmt.query_map(params![&candidate], |row| row.get::<_, i64>(0))?;
            for row in rows {
                event_ids.push(row?);
            }
        }
        drop(stmt);

        event_ids.sort_unstable();
        event_ids.dedup();

        let events_conn = self.events.lock();
        let mut results = Vec::new();
        let mut stmt = events_conn.prepare(
            "SELECT id, ts_ms, kind, payload_json, provenance_json
             FROM events
             WHERE id = ?1
             ORDER BY id ASC",
        )?;
        for id in event_ids {
            if let Some(record) = load_event(&mut stmt, id)? {
                results.push(record);
            }
        }
        Ok(results)
    }

    /// Record a capability audit decision (in-memory only, for compatibility with existing tests).
    pub fn record_cap_audit(&self, record: CapAuditRecord) {
        self.cap_audits.lock().push(record);
    }

    /// Snapshot audit events for diagnostics/tests.
    pub fn cap_audits(&self) -> Vec<CapAuditRecord> {
        self.cap_audits.lock().clone()
    }
}

impl Default for KnowledgeBase {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_paths(config: &KbConfig) -> Result<(PathBuf, PathBuf, PathBuf), Error> {
    let root_dir = PathBuf::from(&config.root_dir);
    if root_dir.as_os_str().is_empty() {
        return Err(Error::Config("kb.root_dir must not be empty".into()));
    }
    let events_path = root_dir.join(&config.events_db);
    let views_path = root_dir.join(&config.view_db);
    Ok((root_dir, events_path, views_path))
}

/// Validator wrapper around embedded JSON Schemas.
struct SchemaRegistry {
    schemas: HashMap<String, Arc<JSONSchema>>,
}

impl SchemaRegistry {
    fn load() -> Result<Self, Error> {
        let mut schemas = HashMap::new();
        for (kind, raw) in EMBEDDED_SCHEMAS.iter() {
            let value: Value =
                serde_json::from_str(raw).map_err(|err| Error::Config(err.to_string()))?;
            let compiled =
                JSONSchema::compile(&value).map_err(|err| Error::Config(err.to_string()))?;
            schemas.insert(kind.to_string(), Arc::new(compiled));
        }
        Ok(Self { schemas })
    }

    fn get(&self, kind: &str) -> Option<Arc<JSONSchema>> {
        self.schemas.get(kind).cloned()
    }
}

/// Map of embedded schema documents keyed by event kind.
static EMBEDDED_SCHEMAS: &[(&str, &str)] = &[
    (
        "contact.upserted",
        include_str!("../schemas/contact.upserted.schema.json"),
    ),
    (
        "artifact.created",
        include_str!("../schemas/artifact.created.schema.json"),
    ),
    (
        "email.sent",
        include_str!("../schemas/email.sent.schema.json"),
    ),
    (
        "run.started",
        include_str!("../schemas/run.started.schema.json"),
    ),
    (
        "run.finished",
        include_str!("../schemas/run.finished.schema.json"),
    ),
];

/// Result rows returned by `query`.
#[derive(Clone, Debug, Serialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
}

/// Search hit spanning standard KB domains.
#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    pub domain: SearchDomain,
    pub key: String,
    pub label: String,
    pub source_event: EventId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl SearchHit {
    fn contact(
        key: String,
        name: Option<String>,
        email: Option<String>,
        org: Option<String>,
        trust: f64,
        source_event: i64,
    ) -> Self {
        let mut extra = Map::new();
        if let Some(email) = email {
            extra.insert("email".into(), Value::String(email));
        }
        if let Some(org) = org {
            extra.insert("org".into(), Value::String(org));
        }
        if let Some(num) = serde_json::Number::from_f64(trust) {
            extra.insert("trust".into(), Value::Number(num));
        }
        Self {
            domain: SearchDomain::Contacts,
            key,
            label: name.unwrap_or_else(|| "contact".into()),
            source_event: EventId(source_event),
            extra: Some(Value::Object(extra)),
        }
    }

    fn artifact(
        key: String,
        kind: String,
        path: String,
        summary: Option<String>,
        source_event: i64,
    ) -> Self {
        let mut extra = Map::new();
        extra.insert("kind".into(), Value::String(kind));
        extra.insert("path".into(), Value::String(path));
        if let Some(summary) = summary {
            extra.insert("summary".into(), Value::String(summary));
        }
        Self {
            domain: SearchDomain::Artifacts,
            key,
            label: "artifact".into(),
            source_event: EventId(source_event),
            extra: Some(Value::Object(extra)),
        }
    }

    fn event(id: i64, kind: String, ts_ms: i64) -> Self {
        let mut extra = Map::new();
        extra.insert("ts_ms".into(), Value::Number(ts_ms.into()));
        Self {
            domain: SearchDomain::Events,
            key: format!("event:{id}"),
            label: kind,
            source_event: EventId(id),
            extra: Some(Value::Object(extra)),
        }
    }

    fn run(trace_id: String, status: String, opening_id: String, source_event: i64) -> Self {
        let mut extra = Map::new();
        extra.insert("trace_id".into(), Value::String(trace_id.clone()));
        extra.insert("opening_id".into(), Value::String(opening_id));
        Self {
            domain: SearchDomain::Runs,
            key: format!("run:{trace_id}"),
            label: status,
            source_event: EventId(source_event),
            extra: Some(Value::Object(extra)),
        }
    }
}

/// Enumerates supported search domains.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchDomain {
    Contacts,
    Artifacts,
    Events,
    Runs,
}

/// Record returned by `kb.why`.
#[derive(Clone, Debug, Serialize)]
pub struct EventRecord {
    pub id: EventId,
    pub ts_ms: u64,
    pub kind: String,
    pub payload: Value,
    pub provenance: Value,
}

/// Applies ledger events into materialized views.
#[derive(Clone)]
pub struct Materializer {
    kb: KnowledgeBase,
    batch_size: usize,
}

impl Materializer {
    /// Create a materializer for the provided knowledge base.
    pub fn new(kb: KnowledgeBase) -> Self {
        Self {
            kb,
            batch_size: 512,
        }
    }

    /// Override the batch size (number of events processed per sync).
    #[must_use]
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// Apply any events with ids greater than the persisted watermark.
    /// Returns `true` if at least one event was materialized.
    pub fn sync(&self) -> Result<bool, Error> {
        let watermark = self.current_watermark()?;
        let pending = self.load_events_after(watermark)?;
        if pending.is_empty() {
            return Ok(false);
        }

        let mut views = self.kb.views.lock();
        let tx = views.transaction()?;
        for event in &pending {
            apply_event(&tx, event)?;
        }
        let new_watermark = pending.last().map(|ev| ev.id).unwrap_or(watermark);
        tx.execute(
            "UPDATE materializer_state SET watermark = ?1 WHERE id = 1",
            params![new_watermark],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Return the currently persisted watermark (highest applied event id).
    pub fn current_watermark(&self) -> Result<i64, Error> {
        let views = self.kb.views.lock();
        let watermark = views.query_row(
            "SELECT watermark FROM materializer_state WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(watermark)
    }

    fn load_events_after(&self, watermark: i64) -> Result<Vec<PendingEvent>, Error> {
        let events = self.kb.events.lock();
        let mut stmt = events.prepare(
            "SELECT id, kind, payload_json, provenance_json
             FROM events
             WHERE id > ?1
             ORDER BY id ASC
             LIMIT ?2",
        )?;
        let mut pending = Vec::new();
        let mut rows = stmt.query(params![watermark, self.batch_size as i64])?;
        while let Some(row) = rows.next()? {
            let payload_str: String = row.get(2)?;
            let provenance_str: String = row.get(3)?;
            let payload: Value = serde_json::from_str(&payload_str)
                .map_err(|err| Error::Config(format!("invalid payload json: {err}")))?;
            let provenance: Value = serde_json::from_str(&provenance_str)
                .map_err(|err| Error::Config(format!("invalid provenance json: {err}")))?;
            pending.push(PendingEvent {
                id: row.get(0)?,
                kind: row.get(1)?,
                payload,
                provenance,
            });
        }
        Ok(pending)
    }
}

struct PendingEvent {
    id: i64,
    kind: String,
    payload: Value,
    provenance: Value,
}

fn apply_event(tx: &Transaction<'_>, event: &PendingEvent) -> Result<(), Error> {
    match event.kind.as_str() {
        "contact.upserted" => apply_contact_event(tx, event)?,
        "artifact.created" => apply_artifact_event(tx, event)?,
        "run.started" | "run.finished" => apply_run_event(tx, event)?,
        "email.sent" => apply_email_event(tx, event)?,
        _ => {}
    }
    Ok(())
}

fn apply_contact_event(tx: &Transaction<'_>, event: &PendingEvent) -> Result<(), Error> {
    let name = event
        .payload
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string());
    let email_raw = event.payload.get("email").and_then(Value::as_str);
    let email_normalized = email_raw.map(normalize_email);
    let org = event
        .payload
        .get("org")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string());
    let trust = event
        .payload
        .get("trust")
        .and_then(Value::as_f64)
        .unwrap_or(0.5);

    let key = derive_contact_key(email_normalized.as_deref(), name.as_deref(), org.as_deref())
        .ok_or_else(|| {
            Error::Config("contact.upserted requires email or name for key derivation".into())
        })?;

    tx.execute(
        "INSERT INTO contacts (contact_key, name, email, org, trust, source_event)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(contact_key) DO UPDATE SET
            name = excluded.name,
            email = excluded.email,
            org = excluded.org,
            trust = excluded.trust,
            source_event = excluded.source_event",
        params![
            &key,
            name.as_deref(),
            email_normalized.as_deref(),
            org.as_deref(),
            trust,
            event.id,
        ],
    )?;

    record_history(tx, &format!("contact:{key}"), event.id)?;
    Ok(())
}

fn apply_artifact_event(tx: &Transaction<'_>, event: &PendingEvent) -> Result<(), Error> {
    let sha256 = event
        .payload
        .get("sha256")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string());
    let path = event
        .payload
        .get("path")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string());
    let kind = event
        .payload
        .get("kind")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .ok_or_else(|| Error::Config("artifact.created missing 'kind'".into()))?;
    let summary = event
        .payload
        .get("summary")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let key = derive_artifact_key(sha256.as_deref(), path.as_deref())
        .ok_or_else(|| Error::Config("artifact.created requires sha256 or path".into()))?;

    tx.execute(
        "INSERT INTO artifacts (artifact_key, kind, path, summary, source_event)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(artifact_key) DO UPDATE SET
            kind = excluded.kind,
            path = excluded.path,
            summary = excluded.summary,
            source_event = excluded.source_event",
        params![
            &key,
            &kind,
            path.as_deref().unwrap_or_default(),
            summary.as_deref(),
            event.id,
        ],
    )?;

    record_history(tx, &format!("artifact:{key}"), event.id)?;
    Ok(())
}

fn apply_run_event(tx: &Transaction<'_>, event: &PendingEvent) -> Result<(), Error> {
    let trace_id = event
        .provenance
        .get("trace_id")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .ok_or_else(|| Error::Config("run event missing provenance.trace_id".into()))?;
    let opening_id = event
        .payload
        .get("opening_id")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .ok_or_else(|| Error::Config("run event missing payload.opening_id".into()))?;
    let status = event
        .payload
        .get("status")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| event.kind.clone());

    tx.execute(
        "INSERT INTO runs (trace_id, status, opening_id, source_event)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(trace_id) DO UPDATE SET
            status = excluded.status,
            opening_id = excluded.opening_id,
            source_event = excluded.source_event",
        params![&trace_id, &status, &opening_id, event.id,],
    )?;

    record_history(tx, &format!("run:{trace_id}"), event.id)?;
    Ok(())
}

fn apply_email_event(tx: &Transaction<'_>, event: &PendingEvent) -> Result<(), Error> {
    if let Some(artifact_id) = event.payload.get("artifact_id").and_then(Value::as_i64) {
        let key = format!("artifact:event:{artifact_id}");
        record_history(tx, &key, event.id)?;
    }
    Ok(())
}

fn record_history(tx: &Transaction<'_>, entity_key: &str, event_id: i64) -> Result<(), Error> {
    tx.execute(
        "INSERT OR IGNORE INTO entity_history (entity_key, event_id)
         VALUES (?1, ?2)",
        params![entity_key, event_id],
    )?;
    Ok(())
}

fn load_event(stmt: &mut Statement<'_>, id: i64) -> Result<Option<EventRecord>, Error> {
    let mut rows = stmt.query(params![id])?;
    if let Some(row) = rows.next()? {
        let payload: String = row.get(3)?;
        let provenance: String = row.get(4)?;
        Ok(Some(EventRecord {
            id: EventId(row.get::<_, i64>(0)?),
            ts_ms: row.get::<_, i64>(1)? as u64,
            kind: row.get::<_, String>(2)?,
            payload: serde_json::from_str(&payload).unwrap_or(Value::String(payload)),
            provenance: serde_json::from_str(&provenance).unwrap_or(Value::String(provenance)),
        }))
    } else {
        Ok(None)
    }
}

fn collect_rows(stmt: &mut Statement<'_>, column_names: &[String]) -> Result<Vec<Value>, Error> {
    let mut rows = Vec::new();
    let mut query = stmt.query([])?;
    while let Some(row) = query.next()? {
        rows.push(row_to_object(row, column_names)?);
    }
    Ok(rows)
}

fn row_to_object(row: &Row<'_>, column_names: &[String]) -> Result<Value, Error> {
    let mut map = Map::new();
    for (idx, name) in column_names.iter().enumerate() {
        let value = match row.get_ref(idx)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(i) => Value::from(i),
            ValueRef::Real(r) => Value::from(r),
            ValueRef::Text(text) => Value::String(String::from_utf8_lossy(text).into_owned()),
            ValueRef::Blob(blob) => Value::String(hex::encode(blob)),
        };
        map.insert(name.clone(), value);
    }
    Ok(Value::Object(map))
}

fn configure_events(conn: &Connection) -> Result<(), Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", 1)?;
    Ok(())
}

fn configure_views(conn: &Connection) -> Result<(), Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", 1)?;
    Ok(())
}

fn apply_events_migrations(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts_ms INTEGER NOT NULL,
            actor TEXT NOT NULL,
            kind TEXT NOT NULL,
            scope TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            provenance_json TEXT NOT NULL,
            hash_blake3 BLOB NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS migrations (
            version INTEGER PRIMARY KEY
        );
        INSERT OR IGNORE INTO migrations(version) VALUES (1);
        ",
    )?;
    Ok(())
}

fn apply_views_migrations(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS contacts (
            contact_key TEXT PRIMARY KEY,
            name TEXT,
            email TEXT,
            org TEXT,
            trust REAL DEFAULT 0.5,
            source_event INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_contacts_email ON contacts(email);
        CREATE TABLE IF NOT EXISTS artifacts (
            artifact_key TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            summary TEXT,
            source_event INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_artifacts_kind ON artifacts(kind);
        CREATE TABLE IF NOT EXISTS runs (
            trace_id TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            opening_id TEXT NOT NULL,
            source_event INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS entity_history (
            entity_key TEXT NOT NULL,
            event_id INTEGER NOT NULL,
            PRIMARY KEY(entity_key, event_id)
        );
        CREATE TABLE IF NOT EXISTS materializer_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            watermark INTEGER NOT NULL DEFAULT 0
        );
        INSERT OR IGNORE INTO materializer_state (id, watermark) VALUES (1, 0);
        ",
    )?;
    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), Error> {
    if scope == "user" || scope == "system" || scope.starts_with("agent:") {
        Ok(())
    } else {
        Err(Error::InvalidScope(scope.into()))
    }
}

fn validate_referential_integrity(conn: &Connection, delta: &StateDelta) -> Result<(), Error> {
    if let Some(evidence) = delta.payload.get("evidence").and_then(|v| v.as_array()) {
        for value in evidence {
            if let Some(id) = value.as_i64()
                && !event_exists(conn, id)?
            {
                return Err(Error::MissingEvidence(id));
            }
        }
    }
    Ok(())
}

fn event_exists(conn: &Connection, id: i64) -> Result<bool, Error> {
    let mut stmt = conn.prepare("SELECT 1 FROM events WHERE id = ?1 LIMIT 1")?;
    let exists = stmt.exists(params![id])?;
    Ok(exists)
}

fn canonicalize_value(value: &Value) -> Result<String, Error> {
    jcs_to_string(value).map_err(|err| Error::Canon(err.to_string()))
}

fn render_schema_error(err: ValidationError<'_>) -> String {
    err.to_string()
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn entity_key_candidates(key: &str) -> Vec<String> {
    if key.contains(':') {
        vec![key.to_string()]
    } else {
        vec![
            key.to_string(),
            format!("contact:{key}"),
            format!("artifact:{key}"),
            format!("run:{key}"),
        ]
    }
}

/// Normalize an email address (lowercase + trim). Does not perform SMTP validation.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// Derive the canonical contact key from the primary email or a fallback alias.
pub fn derive_contact_key(
    email: Option<&str>,
    name: Option<&str>,
    org: Option<&str>,
) -> Option<String> {
    if let Some(email) = email.filter(|s| !s.trim().is_empty()) {
        let normalized = normalize_email(email);
        let hash = blake3::hash(normalized.as_bytes());
        return Some(hex::encode(hash.as_bytes()));
    }
    if let Some(name) = name {
        let mut hasher = blake3::Hasher::new();
        hasher.update(name.trim().as_bytes());
        if let Some(org) = org {
            hasher.update(b"|");
            hasher.update(org.trim().as_bytes());
        }
        let digest = hasher.finalize();
        return Some(hex::encode(digest.as_bytes()));
    }
    None
}

/// Derive the canonical artifact key from sha256 (preferred) or a path fallback.
pub fn derive_artifact_key(sha256: Option<&str>, path: Option<&str>) -> Option<String> {
    if let Some(hash) = sha256.filter(|s| !s.trim().is_empty()) {
        return Some(hash.to_ascii_lowercase());
    }
    if let Some(path) = path {
        let digest = blake3::hash(path.trim().as_bytes());
        return Some(hex::encode(digest.as_bytes()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ErrorCode;
    use serde_json::{Value, json};

    fn sample_provenance() -> Provenance {
        Provenance {
            trace_id: "trace:1".into(),
            opening_id: "opening:1".into(),
            agent_id: "agent:writer@v1".into(),
            inputs_hash: None,
            rationale: Some("unit-test".into()),
        }
    }

    fn provenance_with_trace(trace: &str) -> Provenance {
        let mut prov = sample_provenance();
        prov.trace_id = trace.into();
        prov
    }

    fn contact_delta(name: &str, email: &str, trust: f64) -> StateDelta {
        StateDelta::new(
            "contact.upserted",
            "agent:writer@v1",
            None,
            json!({
                "name": name,
                "email": email,
                "trust": trust
            }),
            sample_provenance(),
        )
    }

    #[test]
    fn duplicate_hash_rejected() {
        let kb = KnowledgeBase::new();
        let delta = contact_delta("Alice", "alice@example.com", 0.6);
        let id = kb.propose(delta.clone()).expect("first insert succeeds");
        assert_eq!(id.0, 1);
        let err = kb.propose(delta).expect_err("duplicate insert should fail");
        match err {
            Error::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => {
                assert_eq!(code.code, ErrorCode::ConstraintViolation);
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn invalid_payload_rejected() {
        let kb = KnowledgeBase::new();
        let delta = StateDelta::new(
            "contact.upserted",
            "agent:writer@v1",
            None,
            json!({
                "name": "Bob"
            }),
            sample_provenance(),
        );
        let err = kb
            .propose(delta)
            .expect_err("missing email should be rejected");
        match err {
            Error::Schema(msg) => assert!(
                msg.contains("email"),
                "expected schema error mentioning email, got {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn materializer_applies_contact_upserts() {
        let kb = KnowledgeBase::new();
        let delta1 = contact_delta("Alice", "alice@example.com", 0.55);
        let delta2 = contact_delta("Alice Cooper", "alice@example.com", 0.90);
        let id1 = kb.propose(delta1).expect("first contact inserted");
        let id2 = kb.propose(delta2).expect("second contact inserted");
        assert!(id1.0 < id2.0);

        let materializer = Materializer::new(kb.clone());
        assert!(materializer.sync().expect("first sync succeeds"));
        assert!(
            !materializer.sync().expect("second sync idempotent"),
            "expected no work on second sync"
        );

        let normalized_email = normalize_email("alice@example.com");
        let key = derive_contact_key(Some(normalized_email.as_str()), Some("Alice Cooper"), None)
            .expect("contact key");

        let result = kb
            .query("SELECT contact_key, name, trust, source_event FROM contacts")
            .expect("query contacts");
        assert_eq!(result.rows.len(), 1);
        let row = result.rows.first().unwrap().as_object().unwrap();
        assert_eq!(row["contact_key"], Value::String(key.clone()));
        assert_eq!(row["name"], Value::String("Alice Cooper".into()));
        assert_eq!(row["trust"], Value::from(0.90));
        assert_eq!(row["source_event"], Value::from(id2.0));

        let history = kb.why(&format!("contact:{key}")).expect("why results");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id.0, id1.0);
        assert_eq!(history[1].id.0, id2.0);

        let simple_history = kb.why(&key).expect("flexible key lookup");
        assert_eq!(simple_history.len(), history.len());
    }

    #[test]
    fn search_returns_contact() {
        let kb = KnowledgeBase::new();
        let delta = contact_delta("Eve Search", "eve@example.com", 0.42);
        kb.propose(delta).expect("insert contact");

        let materializer = Materializer::new(kb.clone());
        assert!(materializer.sync().unwrap());

        let hits = kb.search("eve").expect("search executes");
        assert!(
            hits.iter().any(|hit| hit.domain == SearchDomain::Contacts),
            "expected contact hit in search results"
        );
    }

    #[test]
    fn query_rejects_mutation() {
        let kb = KnowledgeBase::new();
        let err = kb
            .query("DELETE FROM contacts")
            .expect_err("mutations not allowed");
        assert!(matches!(err, Error::QueryNotReadOnly));
    }

    #[test]
    fn invalid_scope_rejected() {
        let kb = KnowledgeBase::new();
        let delta = StateDelta::new(
            "contact.upserted",
            "agent:writer@v1",
            Some("invalid".into()),
            json!({
                "name": "Mallory",
                "email": "mallory@example.com"
            }),
            sample_provenance(),
        );
        let err = kb.propose(delta).expect_err("invalid scope should fail");
        assert!(matches!(err, Error::InvalidScope(scope) if scope == "invalid"));
    }

    #[test]
    fn missing_evidence_rejected() {
        let kb = KnowledgeBase::new();
        let delta = StateDelta::new(
            "contact.upserted",
            "agent:writer@v1",
            None,
            json!({
                "name": "Mallory",
                "email": "mallory@example.com",
                "evidence": [999]
            }),
            sample_provenance(),
        );
        let err = kb.propose(delta).expect_err("invalid evidence should fail");
        assert!(matches!(err, Error::MissingEvidence(id) if id == 999));
    }

    #[test]
    fn run_search_returns_prefixed_key() {
        let kb = KnowledgeBase::new();
        let delta = StateDelta::new(
            "run.started",
            "agent:runtime@v1",
            Some("system".into()),
            json!({
                "opening_id": "opening-123",
                "status": "started"
            }),
            provenance_with_trace("trace-run"),
        );
        kb.propose(delta).expect("run event inserted");

        let materializer = Materializer::new(kb.clone());
        assert!(materializer.sync().unwrap());

        let hits = kb.search("trace-run").expect("search executes");
        let run_hit = hits
            .iter()
            .find(|hit| hit.domain == SearchDomain::Runs)
            .expect("run search hit");
        assert_eq!(run_hit.key, "run:trace-run");

        let history = kb.why(&run_hit.key).expect("history lookup");
        assert!(
            !history.is_empty(),
            "expected non-empty history for run key"
        );
    }
}

/// Decision enum describing whether a capability check allowed or denied an operation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditDecision {
    Allow,
    Deny,
}

/// Severity marker for audit entries.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditSeverity {
    Info,
    Warn,
    Error,
}

/// Canonical capability audit record (stored as JCS JSON in future revisions).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapAuditRecord {
    pub ts_ms: u64,
    pub trace_id: TraceId,
    pub agent_id: AgentId,
    pub cap: String,
    pub op: String,
    pub target: String,
    pub args_hash: [u8; 32],
    pub decision: AuditDecision,
    pub reason: String,
    pub severity: AuditSeverity,
}

impl CapAuditRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trace_id: TraceId,
        agent_id: AgentId,
        cap: impl Into<String>,
        op: impl Into<String>,
        target: impl Into<String>,
        args: &[u8],
        decision: AuditDecision,
        reason: impl Into<String>,
        severity: AuditSeverity,
    ) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(args);
        let digest = hasher.finalize();
        Self {
            ts_ms: timestamp_ms(),
            trace_id,
            agent_id,
            cap: cap.into(),
            op: op.into(),
            target: target.into(),
            args_hash: digest.into(),
            decision,
            reason: reason.into(),
            severity,
        }
    }
}

/// Provenance metadata attached to ledger events.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provenance {
    pub trace_id: String,
    pub opening_id: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

impl Provenance {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("provenance serializes")
    }
}

/// Immutable delta proposed by an agent before validation.
#[derive(Clone, Debug)]
pub struct StateDelta {
    pub kind: String,
    pub actor: String,
    scope: Option<String>,
    pub payload: Value,
    pub provenance: Provenance,
}

impl StateDelta {
    pub fn new(
        kind: impl Into<String>,
        actor: impl Into<String>,
        scope: Option<String>,
        payload: Value,
        provenance: Provenance,
    ) -> Self {
        Self {
            kind: kind.into(),
            actor: actor.into(),
            scope,
            payload,
            provenance,
        }
    }

    fn scope(&self) -> &str {
        self.scope.as_deref().unwrap_or("user")
    }
}
