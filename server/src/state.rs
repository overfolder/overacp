use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use overacp_compute_core::ComputeProvider;
use uuid::Uuid;

use crate::api::ProviderRegistry;
use crate::auth::Authenticator;
use crate::basic_auth::HtpasswdFile;
use crate::hooks::{
    BootProvider, DefaultBootProvider, DefaultQuotaPolicy, DefaultToolHost, QuotaPolicy, ToolHost,
};
use crate::registry::{AgentRegistry, MessageQueue};
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
    /// New broker-shaped per-agent routing table. Replaces
    /// `sessions` once the legacy `/agents/{id}/...` REST surface
    /// is rewritten in Phase 4b.
    pub registry: AgentRegistry,
    /// Bounded per-agent buffer for `session/message` pushes that
    /// arrive while the agent's tunnel is disconnected.
    pub message_queue: MessageQueue,
    pub stream_broker: Arc<StreamBroker>,
    /// Operator hook for `initialize` — see `hooks::BootProvider`.
    pub boot_provider: Arc<dyn BootProvider>,
    /// Operator hook for `tools/list` and `tools/call`.
    pub tool_host: Arc<dyn ToolHost>,
    /// Operator hook for `quota/check` and `quota/update`.
    pub quota_policy: Arc<dyn QuotaPolicy>,
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
            registry: AgentRegistry::new(),
            message_queue: MessageQueue::default(),
            stream_broker: StreamBroker::new(),
            boot_provider: Arc::new(DefaultBootProvider),
            tool_host: Arc::new(DefaultToolHost),
            quota_policy: Arc::new(DefaultQuotaPolicy),
            basic_auth: None,
            default_user_id: None,
        }
    }

    /// Builder: replace the default `BootProvider` with an
    /// operator-supplied implementation.
    pub fn with_boot_provider(mut self, provider: Arc<dyn BootProvider>) -> Self {
        self.boot_provider = provider;
        self
    }

    /// Builder: replace the default `ToolHost` with an
    /// operator-supplied implementation.
    pub fn with_tool_host(mut self, host: Arc<dyn ToolHost>) -> Self {
        self.tool_host = host;
        self
    }

    /// Builder: replace the default `QuotaPolicy` with an
    /// operator-supplied implementation.
    pub fn with_quota_policy(mut self, policy: Arc<dyn QuotaPolicy>) -> Self {
        self.quota_policy = policy;
        self
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};

    use crate::api::default_registry;
    use crate::auth::{Claims, StaticJwtAuthenticator};
    use crate::hooks::{BootError, BootProvider, QuotaError, QuotaPolicy, ToolError, ToolHost};
    use crate::store::InMemoryStore;

    fn base() -> AppState {
        AppState::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("k", "overacp")),
        )
    }

    #[test]
    fn with_default_user_id_sets_field() {
        let state = base();
        assert!(state.default_user_id.is_none());
        let uid = Uuid::new_v4();
        let state = state.with_default_user_id(uid);
        assert_eq!(state.default_user_id, Some(uid));
    }

    #[tokio::test]
    async fn defaults_for_hooks_are_installed() {
        // `AppState::new` must install the stub hooks so the reference
        // server boots end-to-end without an operator stack. We
        // verify this by *behaviour*: invoking each hook and checking
        // we get back the documented stub responses.
        let state = base();
        let claims = Claims::agent(Uuid::new_v4(), None, 60, "test");

        let boot = state.boot_provider.initialize(&claims).await.unwrap();
        assert_eq!(boot["system_prompt"], "");
        assert_eq!(boot["messages"], json!([]));

        let tools = state.tool_host.list(&claims).await.unwrap();
        assert_eq!(tools["tools"], json!([]));

        assert!(state.quota_policy.check(&claims).await.unwrap());
    }

    // ── tiny mock hooks for the swap-builder integration test ──

    struct StubBoot;
    #[async_trait]
    impl BootProvider for StubBoot {
        async fn initialize(&self, _claims: &Claims) -> Result<Value, BootError> {
            Ok(json!({ "system_prompt": "stubbed" }))
        }
    }

    struct StubTools;
    #[async_trait]
    impl ToolHost for StubTools {
        async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
            Ok(json!({ "tools": [{ "name": "stub" }] }))
        }
        async fn call(&self, _claims: &Claims, _req: Value) -> Result<Value, ToolError> {
            Ok(json!({ "ok": true }))
        }
    }

    struct StubQuota;
    #[async_trait]
    impl QuotaPolicy for StubQuota {
        async fn check(&self, _claims: &Claims) -> Result<bool, QuotaError> {
            Ok(false)
        }
        async fn record(&self, _claims: &Claims, _usage: Value) -> Result<(), QuotaError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn builder_methods_swap_in_custom_hooks() {
        let state = base()
            .with_boot_provider(Arc::new(StubBoot))
            .with_tool_host(Arc::new(StubTools))
            .with_quota_policy(Arc::new(StubQuota));

        let claims = Claims::agent(Uuid::new_v4(), None, 60, "test");

        let boot = state.boot_provider.initialize(&claims).await.unwrap();
        assert_eq!(boot["system_prompt"], "stubbed");

        let tools = state.tool_host.list(&claims).await.unwrap();
        assert_eq!(tools["tools"][0]["name"], "stub");

        let allowed = state.quota_policy.check(&claims).await.unwrap();
        assert!(!allowed, "stub quota should refuse");
    }
}
