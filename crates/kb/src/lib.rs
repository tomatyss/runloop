use std::convert::TryInto;
use std::fmt;
use std::path::Path;

use blake3::Hasher;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

const DIGEST_LEN: usize = 32;

#[derive(Debug)]
pub struct KbStore {
    conn: Connection,
}

impl KbStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, KbError> {
        let conn = Connection::open(path)?;
        initialise(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, KbError> {
        let conn = Connection::open_in_memory()?;
        initialise(&conn)?;
        Ok(Self { conn })
    }

    pub fn insert_event(&self, event: Event) -> Result<EventId, KbError> {
        ensure_no_cleartext_secrets(&event.payload)?;
        ensure_no_cleartext_secrets(&event.provenance)?;

        let canonical_payload = canonicalise_json(&event.payload)?;
        let canonical_provenance = canonicalise_json(&event.provenance)?;
        let digest = compute_digest(
            &event.kind,
            &event.actor,
            &event.scope,
            &canonical_payload,
            &canonical_provenance,
        );

        let ts_ms = event.ts_ms.unwrap_or_else(|| Utc::now().timestamp_millis());

        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO events (hash_blake3, ts_ms, kind, actor, scope, payload_json, provenance_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;

        let result = stmt.execute(params![
            digest.as_bytes(),
            ts_ms,
            event.kind,
            event.actor,
            event.scope,
            canonical_payload,
            canonical_provenance,
        ]);

        match result {
            Ok(_) => Ok(EventId(self.conn.last_insert_rowid())),
            Err(rusqlite::Error::SqliteFailure(ref failure, _))
                if failure.code == ErrorCode::ConstraintViolation
                    && failure.extended_code == 2067 =>
            {
                Err(KbError::Duplicate {
                    digest: Digest::from_bytes(*digest.as_bytes()),
                })
            }
            Err(err) => Err(KbError::from(err)),
        }
    }

    pub fn fetch_by_hash(&self, digest: &Digest) -> Result<Option<PersistedEvent>, KbError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, hash_blake3, ts_ms, kind, actor, scope, payload_json, provenance_json
             FROM events WHERE hash_blake3 = ?1",
        )?;
        let row = stmt
            .query_row([digest.as_bytes()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .optional()
            .map_err(KbError::from)?;

        match row {
            Some((id, hash_vec, ts_ms, kind, actor, scope, payload_json, provenance_json)) => {
                let digest_bytes: [u8; DIGEST_LEN] =
                    hash_vec.try_into().map_err(|_| KbError::CorruptDigest)?;
                Ok(Some(PersistedEvent {
                    id: EventId(id),
                    digest: Digest::from_bytes(digest_bytes),
                    ts_ms,
                    kind,
                    actor,
                    scope,
                    payload_json,
                    provenance_json,
                }))
            }
            None => Ok(None),
        }
    }
}

fn initialise(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\n         PRAGMA foreign_keys=ON;\n         PRAGMA synchronous=NORMAL;",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            hash_blake3 BLOB NOT NULL UNIQUE,
            ts_ms INTEGER NOT NULL,
            kind TEXT NOT NULL,
            actor TEXT NOT NULL,
            scope TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            provenance_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts_ms);
        CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope);",
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub kind: String,
    pub actor: String,
    pub scope: String,
    pub payload: Value,
    pub provenance: Value,
    #[serde(default)]
    pub ts_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedEvent {
    pub id: EventId,
    pub digest: Digest,
    pub ts_ms: i64,
    pub kind: String,
    pub actor: String,
    pub scope: String,
    pub payload_json: String,
    pub provenance_json: String,
}

impl PersistedEvent {
    pub fn ts(&self) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(self.ts_ms)
            .single()
            .unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest([u8; DIGEST_LEN]);

impl Digest {
    pub fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

fn compute_digest(
    kind: &str,
    actor: &str,
    scope: &str,
    canonical_payload: &str,
    canonical_provenance: &str,
) -> blake3::Hash {
    let mut hasher = Hasher::new();
    hasher.update(kind.as_bytes());
    hasher.update(actor.as_bytes());
    hasher.update(scope.as_bytes());
    hasher.update(canonical_payload.as_bytes());
    hasher.update(canonical_provenance.as_bytes());
    hasher.finalize()
}

fn canonicalise_json(value: &Value) -> Result<String, KbError> {
    let canonical_value = canonical_value(value);
    serde_json::to_string(&canonical_value).map_err(|err| KbError::Canonical(err.to_string()))
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut canonical = Map::new();
            for (key, val) in entries {
                canonical.insert(key.clone(), canonical_value(val));
            }
            Value::Object(canonical)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        _ => value.clone(),
    }
}

fn ensure_no_cleartext_secrets(value: &Value) -> Result<(), KbError> {
    fn check(value: &Value, key_hint: Option<&str>, inherited_sensitive: bool) -> Result<(), KbError> {
        let key_sensitive = key_hint
            .map(|k| k.to_ascii_lowercase().contains("secret"))
            .unwrap_or(false);
        let sensitive = inherited_sensitive || key_sensitive;
        match value {
            Value::String(s) => {
                let lower = s.to_ascii_lowercase();
                let allowed = lower.starts_with("secret://") || lower.starts_with("secret_id:");
                if (lower.contains("secret") || sensitive) && !allowed {
                    return Err(KbError::CleartextSecret(s.clone()));
                }
                Ok(())
            }
            Value::Array(items) => {
                for item in items {
                    check(item, None, sensitive)?;
                }
                Ok(())
            }
            Value::Object(map) => {
                for (key, val) in map.iter() {
                    check(val, Some(key), sensitive)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    check(value, None, false)
}

#[derive(Debug, Error)]
pub enum KbError {
    #[error("sql error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("duplicate event digest {digest}")]
    Duplicate { digest: Digest },
    #[error("cleartext secret detected: {0}")]
    CleartextSecret(String),
    #[error("canonicalisation error: {0}")]
    Canonical(String),
    #[error("corrupt digest payload in storage")]
    CorruptDigest,
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl std::fmt::Display for PersistedEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event {} ({})", self.id.0, self.digest.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicate_hash_rejected() {
        let store = KbStore::open_in_memory().unwrap();
        let payload_a = json!({"a": 1, "b": 2});
        let payload_b = json!({"b": 2, "a": 1});
        let provenance = json!({"source": "unit"});

        let event_a = Event {
            kind: "observation".into(),
            actor: "agent:foo@1".into(),
            scope: "contacts".into(),
            payload: payload_a,
            provenance: provenance.clone(),
            ts_ms: Some(1000),
        };
        let event_b = Event {
            kind: "observation".into(),
            actor: "agent:foo@1".into(),
            scope: "contacts".into(),
            payload: payload_b,
            provenance,
            ts_ms: Some(1001),
        };

        let id_one = store.insert_event(event_a).unwrap();
        assert_eq!(id_one.0, 1);
        let err = store.insert_event(event_b).unwrap_err();
        assert!(matches!(err, KbError::Duplicate { .. }));
    }

    #[test]
    fn different_payload_changes_hash() {
        let store = KbStore::open_in_memory().unwrap();
        let base_payload = json!({"a": 1});
        let event_a = Event {
            kind: "observation".into(),
            actor: "agent:foo@1".into(),
            scope: "contacts".into(),
            payload: base_payload.clone(),
            provenance: json!({"source": "unit"}),
            ts_ms: Some(1000),
        };
        let event_b = Event {
            kind: "observation".into(),
            actor: "agent:foo@1".into(),
            scope: "contacts".into(),
            payload: json!({"a": 2}),
            provenance: json!({"source": "unit"}),
            ts_ms: Some(1001),
        };

        store.insert_event(event_a).unwrap();
        store.insert_event(event_b).unwrap();
    }

    #[test]
    fn rejects_cleartext_secret() {
        let store = KbStore::open_in_memory().unwrap();
        let event = Event {
            kind: "observation".into(),
            actor: "agent:foo@1".into(),
            scope: "contacts".into(),
            payload: json!({"secret": "password123"}),
            provenance: json!({"source": "unit"}),
            ts_ms: Some(1000),
        };
        let err = store.insert_event(event).unwrap_err();
        assert!(matches!(err, KbError::CleartextSecret(_)));
    }

    #[test]
    fn rejects_secret_nested_under_sensitive_key() {
        let store = KbStore::open_in_memory().unwrap();
        let event = Event {
            kind: "observation".into(),
            actor: "agent:foo@1".into(),
            scope: "contacts".into(),
            payload: json!({"secrets": ["abcd1234"]}),
            provenance: json!({"source": "unit"}),
            ts_ms: Some(1000),
        };
        let err = store.insert_event(event).unwrap_err();
        assert!(matches!(err, KbError::CleartextSecret(_)));
    }
}
