use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use overacp_compute_core::{ComputeProvider, ConfigResolver};

use crate::api::ProviderRegistry;
use crate::auth::Authenticator;
use crate::store::SessionStore;
use crate::tunnel::broker::StreamBroker;
use crate::tunnel::session_manager::{new_session_manager, SessionManager};

/// Pool name → live `ComputeProvider` instance.
///
/// Populated as pools are loaded; the REST node routes
/// (`/compute/pools/{pool}/nodes/...`) look up the running provider
/// here and dispatch through it.
pub type PoolRuntimes = RwLock<HashMap<String, Arc<dyn ComputeProvider>>>;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn SessionStore>,
    pub providers: Arc<ProviderRegistry>,
    pub pool_runtimes: Arc<PoolRuntimes>,
    /// Resolver for `${...}` secret references in pool configs.
    /// One process-lifetime instance shared across pools so the
    /// `env`/`file`/... providers initialise just once.
    pub resolver: Arc<ConfigResolver>,
    pub authenticator: Arc<dyn Authenticator>,
    pub sessions: SessionManager,
    pub stream_broker: Arc<StreamBroker>,
}

impl AppState {
    pub fn new(
        store: Arc<dyn SessionStore>,
        providers: Arc<ProviderRegistry>,
        authenticator: Arc<dyn Authenticator>,
    ) -> Self {
        Self {
            store,
            providers,
            pool_runtimes: Arc::new(RwLock::new(HashMap::new())),
            resolver: Arc::new(ConfigResolver::with_defaults()),
            authenticator,
            sessions: new_session_manager(),
            stream_broker: StreamBroker::new(),
        }
    }

    pub fn register_pool_runtime(
        &self,
        pool: impl Into<String>,
        provider: Arc<dyn ComputeProvider>,
    ) {
        self.pool_runtimes
            .write()
            .unwrap()
            .insert(pool.into(), provider);
    }

    pub fn pool_runtime(&self, pool: &str) -> Option<Arc<dyn ComputeProvider>> {
        self.pool_runtimes.read().unwrap().get(pool).cloned()
    }

    /// Atomic "get or instantiate" for pool runtimes. Holds the
    /// write lock across the existence check + insert so concurrent
    /// callers can't both run `make()` and overwrite each other —
    /// see the TOCTOU note in `agents::provider_for_pool`.
    ///
    /// `make` only runs if no entry exists yet for `pool`. If it
    /// returns an error the map is left untouched.
    pub fn pool_runtime_get_or_try_insert<F, E>(
        &self,
        pool: &str,
        make: F,
    ) -> Result<Arc<dyn ComputeProvider>, E>
    where
        F: FnOnce() -> Result<Arc<dyn ComputeProvider>, E>,
    {
        let mut guard = self.pool_runtimes.write().unwrap();
        if let Some(existing) = guard.get(pool) {
            return Ok(existing.clone());
        }
        let provider = make()?;
        guard.insert(pool.to_owned(), provider.clone());
        Ok(provider)
    }
}
