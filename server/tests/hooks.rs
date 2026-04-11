//! Integration coverage for the operator hook surface introduced in
//! Phase 2 of the broker refactor: `BootProvider`, `ToolHost`,
//! `QuotaPolicy`. Confirms that:
//!
//! 1. The default implementations boot the reference server with
//!    sensible no-op behaviour.
//! 2. Operator-supplied hooks can be plugged in via the `AppState`
//!    builders, and the broker holds them via the trait object.
//! 3. The hook traits are object-safe and `Send + Sync` (verified
//!    implicitly by storing them in `Arc<dyn ...>`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use overacp_server::auth::Claims;
use overacp_server::hooks::{
    BootError, BootProvider, DefaultBootProvider, DefaultQuotaPolicy, DefaultToolHost, QuotaError,
    QuotaPolicy, ToolError, ToolHost,
};
use overacp_server::{AppState, StaticJwtAuthenticator};
use uuid::Uuid;

fn fresh_state() -> AppState {
    AppState::new(Arc::new(StaticJwtAuthenticator::new("k", "overacp")))
}

fn agent_claims() -> Claims {
    Claims::agent(Uuid::new_v4(), Some(Uuid::new_v4()), 60, "overacp")
}

#[tokio::test]
async fn default_hooks_run_through_appstate() {
    let state = fresh_state();
    let claims = agent_claims();

    // BootProvider — empty bootstrap.
    let boot = state.boot_provider.initialize(&claims).await.unwrap();
    assert_eq!(boot["system_prompt"], "");
    assert_eq!(boot["messages"], json!([]));

    // ToolHost — empty list, NotFound on call.
    let tools = state.tool_host.list(&claims).await.unwrap();
    assert_eq!(tools["tools"], json!([]));
    let err = state
        .tool_host
        .call(&claims, json!({ "name": "echo" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(name) if name == "echo"));

    // QuotaPolicy — allow everything, record is a no-op.
    assert!(state.quota_policy.check(&claims).await.unwrap());
    state
        .quota_policy
        .record(&claims, json!({ "input_tokens": 100 }))
        .await
        .unwrap();
}

// ── Custom hook implementations for the swap test ──

struct CountingBoot {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl BootProvider for CountingBoot {
    async fn initialize(&self, claims: &Claims) -> Result<Value, BootError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({
            "system_prompt": "operator-supplied",
            "agent": claims.sub.to_string(),
        }))
    }
}

struct EchoTools;
#[async_trait]
impl ToolHost for EchoTools {
    async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
        Ok(json!({ "tools": [{ "name": "echo" }] }))
    }
    async fn call(&self, _claims: &Claims, req: Value) -> Result<Value, ToolError> {
        Ok(json!({ "echo": req }))
    }
}

struct DenyAll;
#[async_trait]
impl QuotaPolicy for DenyAll {
    async fn check(&self, _claims: &Claims) -> Result<bool, QuotaError> {
        Ok(false)
    }
    async fn record(&self, _claims: &Claims, _usage: Value) -> Result<(), QuotaError> {
        Err(QuotaError::Internal("no recording".into()))
    }
}

#[tokio::test]
async fn operator_hooks_replace_defaults_via_builders() {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = fresh_state()
        .with_boot_provider(Arc::new(CountingBoot {
            calls: calls.clone(),
        }))
        .with_tool_host(Arc::new(EchoTools))
        .with_quota_policy(Arc::new(DenyAll));

    let claims = agent_claims();

    // BootProvider — counted custom impl.
    let boot = state.boot_provider.initialize(&claims).await.unwrap();
    assert_eq!(boot["system_prompt"], "operator-supplied");
    assert_eq!(boot["agent"], claims.sub.to_string());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // ToolHost — echo back whatever was sent.
    let tools = state.tool_host.list(&claims).await.unwrap();
    assert_eq!(tools["tools"][0]["name"], "echo");
    let result = state
        .tool_host
        .call(&claims, json!({ "name": "echo", "args": "hi" }))
        .await
        .unwrap();
    assert_eq!(result["echo"]["args"], "hi");

    // QuotaPolicy — refuses, recording errors.
    assert!(!state.quota_policy.check(&claims).await.unwrap());
    let err = state
        .quota_policy
        .record(&claims, json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, QuotaError::Internal(_)));
}

#[tokio::test]
async fn defaults_remain_when_only_one_hook_is_swapped() {
    // Swap only the boot provider; tool/quota should still be the
    // installed defaults.
    let state = fresh_state().with_boot_provider(Arc::new(DefaultBootProvider));
    let claims = agent_claims();

    let tools = state.tool_host.list(&claims).await.unwrap();
    assert_eq!(tools["tools"], json!([]));
    assert!(state.quota_policy.check(&claims).await.unwrap());
}

#[test]
fn hook_traits_are_object_safe_and_send_sync() {
    // If any of these go away the test won't compile — we don't need
    // a runtime assertion.
    fn _accept_boot(_: Arc<dyn BootProvider>) {}
    fn _accept_tools(_: Arc<dyn ToolHost>) {}
    fn _accept_quota(_: Arc<dyn QuotaPolicy>) {}

    _accept_boot(Arc::new(DefaultBootProvider));
    _accept_tools(Arc::new(DefaultToolHost));
    _accept_quota(Arc::new(DefaultQuotaPolicy));
}
