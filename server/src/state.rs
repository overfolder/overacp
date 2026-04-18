use std::sync::Arc;

use crate::auth::Authenticator;
use crate::hooks::{
    BootProvider, DefaultBootProvider, DefaultQuotaPolicy, DefaultToolHost, QuotaPolicy, ToolHost,
};
use crate::registry::agent::{AgentRegistryProvider, InMemoryAgentRegistry};
use crate::registry::queue::{InMemoryMessageQueue, MessageQueueProvider};
use crate::tunnel::broker::{InMemoryStreamBroker, StreamBrokerProvider};

#[cfg(feature = "redis")]
use crate::redis_backend;

#[derive(Clone)]
pub struct AppState {
    pub authenticator: Arc<dyn Authenticator>,
    /// Per-agent routing table. The source of truth for every REST
    /// handler in `api/agents.rs` and the tunnel write path.
    pub registry: Arc<dyn AgentRegistryProvider>,
    /// Bounded per-agent buffer for `session/message` pushes that
    /// arrive while the agent's tunnel is disconnected.
    pub message_queue: Arc<dyn MessageQueueProvider>,
    pub stream_broker: Arc<dyn StreamBrokerProvider>,
    /// Operator hook for `initialize` — see `hooks::BootProvider`.
    pub boot_provider: Arc<dyn BootProvider>,
    /// Operator hook for `tools/list` and `tools/call`.
    pub tool_host: Arc<dyn ToolHost>,
    /// Operator hook for `quota/check` and `quota/update`.
    pub quota_policy: Arc<dyn QuotaPolicy>,
}

impl AppState {
    pub fn new(authenticator: Arc<dyn Authenticator>) -> Self {
        Self {
            authenticator,
            registry: Arc::new(InMemoryAgentRegistry::new()),
            message_queue: Arc::new(InMemoryMessageQueue::default()),
            stream_broker: InMemoryStreamBroker::new(),
            boot_provider: Arc::new(DefaultBootProvider),
            tool_host: Arc::new(DefaultToolHost),
            quota_policy: Arc::new(DefaultQuotaPolicy),
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

    /// Builder: swap all three routing providers to Redis-backed
    /// implementations for multi-instance HA.
    #[cfg(feature = "redis")]
    pub fn with_redis_providers(mut self, providers: redis_backend::RedisProviders) -> Self {
        self.registry = providers.registry;
        self.message_queue = providers.message_queue;
        self.stream_broker = providers.stream_broker;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use uuid::Uuid;

    use crate::auth::{Claims, StaticJwtAuthenticator};
    use crate::hooks::{BootError, BootProvider, QuotaError, QuotaPolicy, ToolError, ToolHost};

    fn base() -> AppState {
        AppState::new(Arc::new(StaticJwtAuthenticator::new("k", "overacp")))
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
        let call_result = state
            .tool_host
            .call(&claims, json!({ "name": "stub" }))
            .await
            .unwrap();
        assert_eq!(call_result["ok"], true);

        let allowed = state.quota_policy.check(&claims).await.unwrap();
        assert!(!allowed, "stub quota should refuse");
        state.quota_policy.record(&claims, json!({})).await.unwrap();
    }
}
