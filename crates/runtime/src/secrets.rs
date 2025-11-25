use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use parking_lot::RwLock;

/// Per-agent store that maps secret identifiers to opaque runtime handles.
///
/// On first `resolve_or_create` call for a given `secret_id`, a random,
/// non-derivable handle (`rlsec_<base32>`) is generated and cached. The real
/// secret value is stored internally so only host-side sinks (e.g., model
/// broker) can dereference it. Guests only ever see the opaque handle.
///
/// Handles are bound to the lifetime of this store (typically per-agent
/// session) and are cleared when the store is dropped.
///
/// # Future Use
///
/// This infrastructure is **not yet wired into the runtime**. Currently,
/// `resolve_secret` returns real values for backward compatibility. When we
/// add hostcalls that accept secret handles (e.g., `http_request` with auth
/// headers), this store will be integrated into `HostState` to provide the
/// opaque handle layer.
#[derive(Clone, Default)]
#[allow(dead_code)]
pub struct SecretHandleStore {
    /// Maps secret_id → opaque handle (rlsec_...)
    id_to_handle: Arc<RwLock<BTreeMap<String, String>>>,
    /// Maps opaque handle → real secret value
    handle_to_value: Arc<RwLock<BTreeMap<String, String>>>,
}

#[allow(dead_code)]
impl SecretHandleStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Generate a random opaque handle. Uses UUID v4 bytes encoded in base32.
    fn generate_handle() -> String {
        let uuid = uuid::Uuid::new_v4();
        let bytes = uuid.as_bytes();
        // Base32 encode (no padding) for a URL-safe, non-derivable handle
        let encoded = data_encoding::BASE32_NOPAD.encode(bytes);
        format!("rlsec_{}", encoded.to_ascii_lowercase())
    }

    /// Resolve a secret identifier to an opaque handle, creating one if this
    /// is the first request for this `secret_id`.
    ///
    /// Returns `Some(handle)` if the underlying `provider` can resolve the
    /// secret, storing the real value internally. Returns `None` if the
    /// provider cannot resolve the secret.
    pub fn resolve_or_create(
        &self,
        secret_id: &str,
        provider: &dyn SecretProvider,
    ) -> Option<String> {
        // Fast path: already cached
        if let Some(handle) = self.id_to_handle.read().get(secret_id) {
            return Some(handle.clone());
        }

        // Resolve the real value from the provider
        let real_value = provider.resolve(secret_id)?;

        // Generate a new handle and store mappings
        let handle = Self::generate_handle();
        self.id_to_handle
            .write()
            .insert(secret_id.to_string(), handle.clone());
        self.handle_to_value
            .write()
            .insert(handle.clone(), real_value);
        Some(handle)
    }

    /// Dereference an opaque handle to its real secret value.
    /// Only host-side code should call this (e.g., model broker HTTP auth).
    pub fn dereference(&self, handle: &str) -> Option<String> {
        self.handle_to_value.read().get(handle).cloned()
    }

    /// Check if a handle was issued by this store.
    pub fn is_valid_handle(&self, handle: &str) -> bool {
        self.handle_to_value.read().contains_key(handle)
    }

    /// Clear all mappings (called on agent teardown).
    pub fn clear(&self) {
        self.id_to_handle.write().clear();
        self.handle_to_value.write().clear();
    }
}

/// Trait for dereferencing opaque secret handles back to real values.
/// This is used by host-side sinks that need to access the actual secret.
///
/// # Future Use
///
/// This trait is **not yet used**. It will be implemented when we add
/// hostcalls that accept opaque secret handles (e.g., `http_request` with
/// auth headers that dereference handles server-side).
#[allow(dead_code)]
pub trait SecretDereferencer: Send + Sync {
    fn dereference(&self, handle: &str) -> Option<String>;
}

#[allow(dead_code)]
impl SecretDereferencer for SecretHandleStore {
    fn dereference(&self, handle: &str) -> Option<String> {
        SecretHandleStore::dereference(self, handle)
    }
}

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

    // SecretHandleStore tests
    struct MockProvider {
        values: BTreeMap<String, String>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                values: BTreeMap::new(),
            }
        }

        fn with_secret(mut self, id: &str, value: &str) -> Self {
            self.values.insert(id.to_string(), value.to_string());
            self
        }
    }

    impl SecretProvider for MockProvider {
        fn resolve(&self, secret_id: &str) -> Option<String> {
            self.values.get(secret_id).cloned()
        }
    }

    #[test]
    fn handle_store_generates_opaque_handles() {
        let store = SecretHandleStore::new();
        let provider = MockProvider::new().with_secret("api_key", "real-secret-value");

        let handle = store.resolve_or_create("api_key", &provider);
        assert!(handle.is_some());
        let handle = handle.unwrap();

        // Handle should start with rlsec_ prefix
        assert!(
            handle.starts_with("rlsec_"),
            "handle should have rlsec_ prefix"
        );
        // Handle should NOT contain the real value
        assert!(!handle.contains("real-secret-value"));
    }

    #[test]
    fn handle_store_returns_same_handle_for_same_id() {
        let store = SecretHandleStore::new();
        let provider = MockProvider::new().with_secret("api_key", "secret");

        let handle1 = store.resolve_or_create("api_key", &provider).unwrap();
        let handle2 = store.resolve_or_create("api_key", &provider).unwrap();

        assert_eq!(handle1, handle2, "same secret_id should return same handle");
    }

    #[test]
    fn handle_store_returns_different_handles_for_different_ids() {
        let store = SecretHandleStore::new();
        let provider = MockProvider::new()
            .with_secret("key1", "value1")
            .with_secret("key2", "value2");

        let handle1 = store.resolve_or_create("key1", &provider).unwrap();
        let handle2 = store.resolve_or_create("key2", &provider).unwrap();

        assert_ne!(
            handle1, handle2,
            "different secret_ids should have different handles"
        );
    }

    #[test]
    fn handle_store_dereferences_to_real_value() {
        let store = SecretHandleStore::new();
        let provider = MockProvider::new().with_secret("api_key", "super-secret");

        let handle = store.resolve_or_create("api_key", &provider).unwrap();
        let dereferenced = store.dereference(&handle);

        assert_eq!(dereferenced, Some("super-secret".to_string()));
    }

    #[test]
    fn handle_store_returns_none_for_unknown_secret() {
        let store = SecretHandleStore::new();
        let provider = MockProvider::new(); // no secrets

        let handle = store.resolve_or_create("unknown", &provider);
        assert!(handle.is_none());
    }

    #[test]
    fn handle_store_returns_none_for_invalid_handle() {
        let store = SecretHandleStore::new();
        let dereferenced = store.dereference("rlsec_invalid");
        assert!(dereferenced.is_none());
    }

    #[test]
    fn handle_store_clear_removes_all_mappings() {
        let store = SecretHandleStore::new();
        let provider = MockProvider::new().with_secret("api_key", "secret");

        let handle = store.resolve_or_create("api_key", &provider).unwrap();
        assert!(store.is_valid_handle(&handle));

        store.clear();

        assert!(!store.is_valid_handle(&handle));
        assert!(store.dereference(&handle).is_none());
    }
}
