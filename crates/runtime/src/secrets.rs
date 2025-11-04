use std::collections::BTreeSet;
use std::sync::Arc;

use parking_lot::RwLock;

/// Interface for resolving opaque secret identifiers.
pub trait SecretProvider: Send + Sync {
    /// Resolve a secret identifier into an opaque token for the guest.
    /// Implementations must never return raw secret material.
    fn resolve(&self, secret_id: &str) -> Option<String>;
}

/// Default in-memory store that simply vends back opaque secret identifiers.
#[derive(Clone, Default)]
pub struct SecretStore {
    allowed: Arc<RwLock<BTreeSet<String>>>,
}

impl SecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow runtime wiring to pre-register opaque secret identifiers.
    pub fn allow(&self, secret_id: impl Into<String>) {
        self.allowed.write().insert(secret_id.into());
    }
}

impl SecretProvider for SecretStore {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        if self.allowed.read().contains(secret_id) {
            Some(secret_id.to_string())
        } else {
            None
        }
    }
}
