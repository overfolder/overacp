use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use overacp_compute_core::ComputeProvider;

use crate::api::ProviderRegistry;
use crate::store::SessionStore;

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
}

impl AppState {
    pub fn new(store: Arc<dyn SessionStore>, providers: Arc<ProviderRegistry>) -> Self {
        Self {
            store,
            providers,
            pool_runtimes: Arc::new(RwLock::new(HashMap::new())),
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
}
