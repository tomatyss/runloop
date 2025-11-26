use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use runloop_core::Config;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, warn};
use uuid::Uuid;

/// Per-agent store that maps secret identifiers to opaque runtime handles.
///
/// On first `resolve_or_create` call for a given `secret_id`, a random,
/// non-derivable handle (`rlsec_<base32>`) is generated and cached. The real
/// secret value is stored internally so only host-side sinks (e.g., model
/// broker) can dereference it. Guests only ever see the opaque handle.
///
/// Handles are bound to the lifetime of this store (typically per-agent
/// session) and are cleared when the store is dropped.
#[derive(Clone, Default)]
pub struct SecretHandleStore {
    /// Maps secret_id → opaque handle (rlsec_...)
    id_to_handle: Arc<RwLock<BTreeMap<String, String>>>,
    /// Maps opaque handle → (secret_id, value, refreshed_at)
    handle_to_record: Arc<RwLock<BTreeMap<String, HandleRecord>>>,
}

#[derive(Clone)]
struct HandleRecord {
    secret_id: String,
    value: String,
    refreshed_at: Instant,
}

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
    /// is the first request for this `secret_id`. On subsequent calls, always
    /// refresh the value from the provider so rotations are observed.
    ///
    /// Returns `Some(handle)` if the underlying `provider` can resolve the
    /// secret, storing the real value internally. Returns `None` if the
    /// provider cannot resolve the secret.
    pub fn resolve_or_create(
        &self,
        secret_id: &str,
        provider: &dyn SecretProvider,
    ) -> Option<String> {
        // Refresh path: if handle exists, update value from provider
        if let Some(handle) = self.id_to_handle.read().get(secret_id).cloned() {
            let value = provider.resolve(secret_id)?;
            self.handle_to_record.write().insert(
                handle.clone(),
                HandleRecord {
                    secret_id: secret_id.to_string(),
                    value,
                    refreshed_at: Instant::now(),
                },
            );
            return Some(handle);
        }

        // Resolve the real value from the provider
        let real_value = provider.resolve(secret_id)?;

        // Generate a new handle and store mappings
        let handle = Self::generate_handle();
        self.id_to_handle
            .write()
            .insert(secret_id.to_string(), handle.clone());
        self.handle_to_record.write().insert(
            handle.clone(),
            HandleRecord {
                secret_id: secret_id.to_string(),
                value: real_value,
                refreshed_at: Instant::now(),
            },
        );
        Some(handle)
    }

    /// Dereference an opaque handle to its real secret value.
    /// Only host-side code should call this (e.g., model broker HTTP auth).
    pub fn dereference_with(&self, handle: &str, provider: &dyn SecretProvider) -> Option<String> {
        let mut guard = self.handle_to_record.write();
        if let Some(record) = guard.get_mut(handle) {
            // Refresh from provider to avoid serving stale secrets.
            if let Some(fresh) = provider.resolve(&record.secret_id) {
                record.value = fresh;
                record.refreshed_at = Instant::now();
                return Some(record.value.clone());
            }
            // Missing from provider: drop mapping to avoid stale reuse.
            let id = record.secret_id.clone();
            drop(guard);
            self.id_to_handle.write().remove(&id);
            self.handle_to_record.write().remove(handle);
            return None;
        }
        None
    }

    /// Non-refreshing dereference (used only by legacy trait impls / tests).
    pub fn dereference(&self, handle: &str) -> Option<String> {
        self.handle_to_record
            .read()
            .get(handle)
            .map(|rec| rec.value.clone())
    }

    /// Check if a handle was issued by this store.
    pub fn is_valid_handle(&self, handle: &str) -> bool {
        self.handle_to_record.read().contains_key(handle)
    }

    /// Clear all mappings (called on agent teardown).
    pub fn clear(&self) {
        self.id_to_handle.write().clear();
        self.handle_to_record.write().clear();
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn slugify_secret_id(secret_id: &str) -> String {
    secret_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

impl SecretProvider for EnvSecretProvider {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        resolve_secret_from_env(secret_id)
    }

    fn exists(&self, secret_id: &str) -> bool {
        resolve_secret_from_env(secret_id).is_some()
    }
}

/// Env fallback + in-memory store, matching the prior "stub" behavior.
#[derive(Clone, Default)]
pub struct EnvThenStore {
    env: EnvSecretProvider,
    store: Arc<SecretStore>,
}

impl EnvThenStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            env: EnvSecretProvider,
            store: Arc::new(SecretStore::new()),
        }
    }
}

impl SecretProvider for EnvThenStore {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        self.env
            .resolve(secret_id)
            .or_else(|| self.store.resolve(secret_id))
    }

    fn allow(&self, secret_id: &str) {
        self.store.allow(secret_id);
    }

    fn exists(&self, secret_id: &str) -> bool {
        self.env.exists(secret_id) || self.store.exists(secret_id)
    }
}

/// Pass (password-store) backend. Best-effort: shells out to `pass show` and
/// reads the first line of the secret. Only used when explicitly selected or
/// auto-detected.
#[derive(Clone, Default)]
struct PassProvider;

impl PassProvider {
    fn available() -> bool {
        Command::new("pass")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            && home_dir()
                .map(|d| d.join(".password-store"))
                .is_some_and(|p| p.exists())
    }

    fn read_secret(&self, secret_id: &str) -> Option<String> {
        let entry = format!("runloop/{secret_id}");
        let output = Command::new("pass").arg("show").arg(entry).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8(output.stdout).ok()?;
        let line = stdout.lines().next()?.trim();
        if line.is_empty() {
            None
        } else {
            Some(line.to_string())
        }
    }
}

impl SecretProvider for PassProvider {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        self.read_secret(secret_id)
    }

    fn exists(&self, secret_id: &str) -> bool {
        self.read_secret(secret_id).is_some()
    }
}

/// Stub Secret Service backend. Logs a warning once; returns None so callers
/// can fall back.
#[derive(Clone, Default)]
struct SecretServiceProvider;

impl SecretServiceProvider {
    #[allow(dead_code)]
    fn available() -> bool {
        std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok()
    }
}

impl SecretProvider for SecretServiceProvider {
    fn resolve(&self, _secret_id: &str) -> Option<String> {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            warn!("secret-service provider not wired; returning None (falling back)");
        });
        None
    }

    fn exists(&self, _secret_id: &str) -> bool {
        false
    }
}

/// Age-backed file store (stub). Ensures key file permissions but does not yet
/// implement real encryption; returns None so upstream callers can fall back.
#[derive(Clone)]
struct AgeProvider {
    root: PathBuf,
}

impl AgeProvider {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn available(root: &Path) -> bool {
        root.exists() && Self::ensure_master_key(root).is_some()
    }

    fn ensure_master_key(root: &Path) -> Option<()> {
        let key_path = root.join("master.agekey");
        if !key_path.exists() {
            let random = Uuid::new_v4().to_string();
            std::fs::write(&key_path, format!("AGE-SECRET-KEY-1-{random}")).ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
            }
            return Some(());
        }
        let meta = key_path.metadata().ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                warn!("master.agekey permissions too loose; refusing to use");
                return None;
            }
        }
        Some(())
    }
}

impl SecretProvider for AgeProvider {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        std::fs::create_dir_all(&self.root).ok()?;
        let _ = Self::ensure_master_key(&self.root)?;

        static WARN: std::sync::Once = std::sync::Once::new();
        WARN.call_once(|| {
            warn!("age provider currently stores secrets as plaintext within .age files; encryption TODO");
        });

        let slug = slugify_secret_id(secret_id);
        let path = self.root.join(format!("{slug}.age"));
        let content = std::fs::read_to_string(path).ok()?;
        let value = content.trim().to_string();
        if value.is_empty() { None } else { Some(value) }
    }
}

struct CacheEntry {
    value: String,
    expires_at: Option<Instant>,
}

/// TTL-aware caching wrapper.
struct CachingSecretProvider {
    inner: Arc<dyn SecretProvider>,
    ttl: Duration,
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
}

impl CachingSecretProvider {
    fn new(inner: Arc<dyn SecretProvider>, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl SecretProvider for CachingSecretProvider {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        let now = Instant::now();
        if let Some(entry) = self.cache.read().get(secret_id) {
            if entry.expires_at.map(|t| t > now).unwrap_or(false) {
                return Some(entry.value.clone());
            }
        }

        // Expired or missing: try to refresh.
        self.cache.write().remove(secret_id);
        let value = self.inner.resolve(secret_id)?;
        let expires_at = if self.ttl.is_zero() {
            None
        } else {
            Some(now + self.ttl)
        };
        self.cache.write().insert(
            secret_id.to_string(),
            CacheEntry {
                value: value.clone(),
                expires_at,
            },
        );
        Some(value)
    }

    fn allow(&self, secret_id: &str) {
        self.inner.allow(secret_id);
        self.cache.write().remove(secret_id);
    }

    fn exists(&self, secret_id: &str) -> bool {
        if let Some(entry) = self.cache.read().get(secret_id) {
            if entry
                .expires_at
                .map(|t| t > Instant::now())
                .unwrap_or(false)
            {
                return true;
            }
        }
        self.inner.exists(secret_id)
    }
}

fn env_then_store() -> Arc<dyn SecretProvider> {
    Arc::new(EnvThenStore::new())
}

fn expand_root(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(path)
}

fn select_provider_backend(config: &Config) -> Arc<dyn SecretProvider> {
    let root = config
        .security
        .secrets
        .root
        .clone()
        .unwrap_or_else(|| "~/.runloop/secrets".into());
    let root = expand_root(&root);

    match config.security.secrets.provider.as_str() {
        "env" => Arc::new(EnvSecretProvider),
        "secret-service" => Arc::new(SecretServiceProvider::default()),
        "pass" => Arc::new(PassProvider::default()),
        "age" => Arc::new(AgeProvider::new(root)),
        "auto" => {
            if PassProvider::available() {
                Arc::new(PassProvider::default())
            } else if AgeProvider::available(&root) {
                Arc::new(AgeProvider::new(root))
            } else {
                debug!("auto secret provider falling back to env+store");
                env_then_store()
            }
        }
        "stub" => env_then_store(),
        other => {
            warn!(
                provider = other,
                "unknown secret provider; using stub fallback"
            );
            env_then_store()
        }
    }
}

/// Build a SecretProvider based on configuration, honoring TTL and provider
/// selection. Exposed so executors/daemons share the same mapping.
#[must_use]
pub fn secret_provider_from_config(config: &Config) -> Arc<dyn SecretProvider> {
    let base = select_provider_backend(config);
    let ttl_secs = config.security.secrets.default_ttl;
    if ttl_secs == 0 {
        base
    } else {
        Arc::new(CachingSecretProvider::new(
            base,
            Duration::from_secs(ttl_secs),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::sleep;
    use std::time::Duration;

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
        let dereferenced = store.dereference_with(&handle, &provider);

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
        let provider = MockProvider::new();
        let dereferenced = store.dereference_with("rlsec_invalid", &provider);
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
        let provider = MockProvider::new();
        assert!(store.dereference_with(&handle, &provider).is_none());
    }

    #[test]
    fn env_then_store_prefers_env_fallback() {
        let provider = EnvThenStore::new();
        with_env_var("RUNLOOP_TEST_ENV_ONLY", "env-secret", || {
            assert_eq!(
                provider.resolve("RUNLOOP_TEST_ENV_ONLY"),
                Some("env-secret".into())
            );
        });
    }

    #[derive(Default)]
    struct CountingProvider {
        value: RwLock<String>,
        calls: AtomicUsize,
    }

    impl CountingProvider {
        fn with(value: &str) -> Self {
            Self {
                value: RwLock::new(value.to_string()),
                calls: AtomicUsize::new(0),
            }
        }

        fn set(&self, value: &str) {
            *self.value.write() = value.to_string();
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl SecretProvider for CountingProvider {
        fn resolve(&self, _secret_id: &str) -> Option<String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Some(self.value.read().clone())
        }
    }

    #[test]
    fn caching_secret_provider_refreshes_after_ttl() {
        let base = Arc::new(CountingProvider::with("one"));
        let caching = CachingSecretProvider::new(base.clone(), Duration::from_millis(10));

        let first = caching.resolve("alpha").unwrap();
        assert_eq!(first, "one");
        assert_eq!(base.calls(), 1);

        // Within TTL: cached
        let second = caching.resolve("alpha").unwrap();
        assert_eq!(second, "one");
        assert_eq!(base.calls(), 1);

        // After TTL: refresh
        sleep(Duration::from_millis(15));
        base.set("two");
        let third = caching.resolve("alpha").unwrap();
        assert_eq!(third, "two");
        assert_eq!(base.calls(), 2);
    }
}
