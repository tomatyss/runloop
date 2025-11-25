use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use parking_lot::RwLock;

/// Interface for resolving opaque secret identifiers.
/// Implementations should avoid persisting or logging raw secret material.
pub trait SecretProvider: Send + Sync {
    /// Resolve a secret identifier into an opaque token for the guest.
    /// Callers MUST avoid logging or persisting returned values.
    fn resolve(&self, secret_id: &str) -> Option<String>;

    /// Optionally pre-register a secret identifier so lookups can succeed
    /// without leaking the underlying value. Defaults to a no-op.
    fn allow(&self, _secret_id: &str) {}

    /// Optional existence check. Defaults to deriving from `resolve`.
    fn exists(&self, secret_id: &str) -> bool {
        self.resolve(secret_id).is_some()
    }
}

/// Default in-memory store that keeps an allow-list and optional values.
#[derive(Clone, Default)]
pub struct SecretStore {
    allowed: Arc<RwLock<BTreeSet<String>>>,
    values: Arc<RwLock<BTreeMap<String, String>>>,
}

impl SecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow runtime wiring to pre-register opaque secret identifiers.
    /// If no explicit value is provided, lookups will still fail until `put` is called.
    pub fn allow_id(&self, secret_id: impl Into<String>) {
        let id = secret_id.into();
        self.allowed.write().insert(id);
    }

    /// Store a specific opaque value for a secret identifier.
    pub fn put(&self, secret_id: impl Into<String>, value: impl Into<String>) {
        let id = secret_id.into();
        self.allowed.write().insert(id.clone());
        self.values.write().insert(id, value.into());
    }
}

impl SecretProvider for SecretStore {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        if !self.allowed.read().contains(secret_id) {
            return None;
        }
        self.values.read().get(secret_id).cloned()
    }

    fn allow(&self, secret_id: &str) {
        self.allow_id(secret_id.to_string());
    }

    fn exists(&self, secret_id: &str) -> bool {
        self.allowed.read().contains(secret_id)
    }
}

/// Environment-backed provider. Reads from exact and normalized (upper,
/// non-alphanumeric → `_`) variants of the identifier. Writes are not supported.
#[derive(Clone, Default)]
pub struct EnvSecretProvider;

fn normalize_secret_env_key(secret_id: &str) -> String {
    secret_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn resolve_secret_from_env(secret_id: &str) -> Option<String> {
    std::env::var(secret_id)
        .or_else(|_| std::env::var(normalize_secret_env_key(secret_id)))
        .ok()
}

impl SecretProvider for EnvSecretProvider {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        resolve_secret_from_env(secret_id)
    }

    fn exists(&self, secret_id: &str) -> bool {
        resolve_secret_from_env(secret_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(unsafe_code)]
    fn with_env_var(key: &str, val: &str, f: impl FnOnce()) {
        unsafe { std::env::set_var(key, val) };
        f();
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn allow_without_value_does_not_resolve() {
        let store = SecretStore::new();
        store.allow_id("alpha");
        assert!(store.resolve("alpha").is_none());
    }

    #[test]
    fn put_round_trips_value() {
        let store = SecretStore::new();
        store.put("alpha", "opaque-token");
        assert_eq!(store.resolve("alpha"), Some("opaque-token".into()));
    }

    #[test]
    fn exists_tracks_allowed_ids() {
        let store = SecretStore::new();
        store.allow_id("alpha");
        assert!(store.exists("alpha"));
        assert!(!store.exists("beta"));
    }

    #[test]
    fn env_provider_reads_normalized_keys() {
        let provider = EnvSecretProvider;
        with_env_var("RUNLOOP_TEST_SECRET", "value123", || {
            assert_eq!(
                provider.resolve("runloop.test-secret"),
                Some("value123".into())
            );
        });
    }
}
