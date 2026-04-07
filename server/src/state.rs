use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use overacp_compute_core::ComputeProvider;

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
}
