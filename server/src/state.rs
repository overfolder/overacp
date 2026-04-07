use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use overacp_compute_core::ComputeProvider;
use uuid::Uuid;

use crate::api::ProviderRegistry;
use crate::auth::Authenticator;
use crate::basic_auth::HtpasswdFile;
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
    /// Htpasswd-backed credentials for control-plane HTTP Basic auth.
    /// `None` means control-plane endpoints will return 503.
    pub basic_auth: Option<Arc<HtpasswdFile>>,
    /// User UUID attributed to control-plane writes made via HTTP
    /// Basic auth (which carries no user identity). `None` means
    /// control-plane handlers that need a user must reject the call.
    pub default_user_id: Option<Uuid>,
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
            basic_auth: None,
            default_user_id: None,
        }
    }

    /// Builder: attach a loaded htpasswd file for control-plane auth.
    pub fn with_basic_auth(mut self, file: Arc<HtpasswdFile>) -> Self {
        self.basic_auth = Some(file);
        self
    }

    /// Builder: set the default user UUID for control-plane writes.
    pub fn with_default_user_id(mut self, user: Uuid) -> Self {
        self.default_user_id = Some(user);
        self
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
