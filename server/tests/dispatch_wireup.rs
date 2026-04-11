//! End-to-end smoke test for the Phase 3 dispatch rewrite.
//!
//! This builds a `TunnelContext` exactly the way `routes::tunnel_upgrade`
//! does — pulling the hooks from `AppState` — and then drives every
//! request method on the dispatch table through `handle_message`. The
//! intent is to catch wire-up regressions: if a hook isn't correctly
//! plumbed from `AppState` into `TunnelContext` into `handle_message`,
//! the unit tests in `dispatch.rs` (which build the context manually)
//! won't notice but this test will.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{json, Value};

use overacp_server::api::default_registry;
use overacp_server::auth::Claims;
use overacp_server::hooks::{
    BootError, BootProvider, QuotaError, QuotaPolicy, ToolError, ToolHost,
};
use overacp_server::tunnel::dispatch::handle_message;
use overacp_server::tunnel::run::TunnelContext;
use overacp_server::tunnel::StreamBroker;
use overacp_server::{AppState, InMemoryStore, StaticJwtAuthenticator};
use uuid::Uuid;

// ── Tracing hooks so we can prove dispatch reached them ──

#[derive(Default)]
struct Counts {
    boot: AtomicUsize,
    tools_list: AtomicUsize,
    tools_call: AtomicUsize,
    quota_check: AtomicUsize,
    quota_record: AtomicUsize,
}

struct TracingBoot {
    counts: Arc<Counts>,
}
#[async_trait]
impl BootProvider for TracingBoot {
    async fn initialize(&self, claims: &Claims) -> Result<Value, BootError> {
        self.counts.boot.fetch_add(1, Ordering::SeqCst);
        Ok(json!({
            "system_prompt": "wired",
            "agent_id": claims.sub.to_string(),
        }))
    }
}

struct TracingTools {
    counts: Arc<Counts>,
}
#[async_trait]
impl ToolHost for TracingTools {
    async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
        self.counts.tools_list.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "tools": [{ "name": "wired" }] }))
    }
    async fn call(&self, _claims: &Claims, req: Value) -> Result<Value, ToolError> {
        self.counts.tools_call.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "echo": req }))
    }
}

struct TracingQuota {
    counts: Arc<Counts>,
}
#[async_trait]
impl QuotaPolicy for TracingQuota {
    async fn check(&self, _claims: &Claims) -> Result<bool, QuotaError> {
        self.counts.quota_check.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
    async fn record(&self, _claims: &Claims, _usage: Value) -> Result<(), QuotaError> {
        self.counts.quota_record.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn build_state(counts: Arc<Counts>) -> AppState {
    AppState::new(
        Arc::new(InMemoryStore::new()),
        Arc::new(default_registry()),
        Arc::new(StaticJwtAuthenticator::new("k", "overacp")),
    )
    .with_boot_provider(Arc::new(TracingBoot {
        counts: counts.clone(),
    }))
    .with_tool_host(Arc::new(TracingTools {
        counts: counts.clone(),
    }))
    .with_quota_policy(Arc::new(TracingQuota {
        counts: counts.clone(),
    }))
}

/// Mirror of how `routes::tunnel_upgrade` constructs `TunnelContext`.
/// If that handler grows fields, this helper has to grow with it —
/// which is the point: this test fails fast on plumbing drift.
fn build_ctx(state: &AppState, agent_id: Uuid) -> TunnelContext {
    TunnelContext {
        claims: Claims::agent(agent_id, Some(Uuid::new_v4()), 60, "overacp"),
        store: state.store.clone(),
        sessions: state.sessions.clone(),
        registry: state.registry.clone(),
        message_queue: state.message_queue.clone(),
        stream_broker: state.stream_broker.clone(),
        boot_provider: state.boot_provider.clone(),
        tool_host: state.tool_host.clone(),
        quota_policy: state.quota_policy.clone(),
    }
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[tokio::test]
async fn full_dispatch_round_trip_via_appstate() {
    let counts = Arc::new(Counts::default());
    let state = build_state(counts.clone());
    let agent_id = Uuid::new_v4();
    let ctx = build_ctx(&state, agent_id);

    // initialize → BootProvider
    let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, &ctx)
        .await
        .unwrap();
    let parsed = parse(&resp);
    assert_eq!(parsed["result"]["system_prompt"], "wired");
    assert_eq!(parsed["result"]["agent_id"], agent_id.to_string());

    // tools/list → ToolHost::list
    let resp = handle_message(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &ctx)
        .await
        .unwrap();
    assert_eq!(parse(&resp)["result"]["tools"][0]["name"], "wired");

    // tools/call → ToolHost::call
    let resp = handle_message(
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"wired","args":42}}"#,
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(parse(&resp)["result"]["echo"]["args"], 42);

    // quota/check → QuotaPolicy::check
    let resp = handle_message(r#"{"jsonrpc":"2.0","id":4,"method":"quota/check"}"#, &ctx)
        .await
        .unwrap();
    assert_eq!(parse(&resp)["result"]["allowed"], true);

    // quota/update → QuotaPolicy::record
    let resp = handle_message(
        r#"{"jsonrpc":"2.0","id":5,"method":"quota/update","params":{"input_tokens":7}}"#,
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(parse(&resp)["result"], json!({}));

    // turn/end and heartbeat are valid agent → server notifications
    // and produce no response.
    assert!(handle_message(r#"{"jsonrpc":"2.0","method":"turn/end"}"#, &ctx)
        .await
        .is_none());
    assert!(handle_message(r#"{"jsonrpc":"2.0","method":"heartbeat"}"#, &ctx)
        .await
        .is_none());

    // session/cancel is server → agent only. If the agent ever
    // sends one up the wire, the broker rejects it as method not
    // found rather than silently swallowing it. Use an explicit
    // `id` so we test the actual rejection path, not the
    // notification fallthrough.
    let resp = handle_message(
        r#"{"jsonrpc":"2.0","id":99,"method":"session/cancel"}"#,
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(parse(&resp)["error"]["code"], -32601);

    // All five hook entry points were exercised exactly once.
    assert_eq!(counts.boot.load(Ordering::SeqCst), 1);
    assert_eq!(counts.tools_list.load(Ordering::SeqCst), 1);
    assert_eq!(counts.tools_call.load(Ordering::SeqCst), 1);
    assert_eq!(counts.quota_check.load(Ordering::SeqCst), 1);
    assert_eq!(counts.quota_record.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn turn_end_is_fanned_out_to_stream_broker_subscribers() {
    // The read loop in `run.rs` sniffs for `"turn/end"` strings and
    // forwards them to the per-agent broadcast channel. This test
    // exercises the broker fan-out directly: if you subscribe to an
    // agent before pushing a frame, you receive it.
    let stream_broker = StreamBroker::new();
    let agent_id = Uuid::new_v4();

    let mut rx = stream_broker.subscribe(agent_id);
    let sender = stream_broker.sender_for(agent_id);
    let frame = r#"{"jsonrpc":"2.0","method":"turn/end","params":{"messages":[]}}"#;
    sender.send(frame.to_string()).unwrap();

    let received = rx.recv().await.unwrap();
    assert_eq!(received, frame);
}
