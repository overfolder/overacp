//! JSON-RPC 2.0 method dispatch for the over/ACP tunnel.
//!
//! Method names are sourced from `docs/design/protocol.md` § 3.
//! Every request method delegates to one of the operator hooks
//! defined in [`crate::hooks`]; the dispatch table itself is just
//! routing logic.
//!
//! - `initialize` → `BootProvider::initialize`
//! - `tools/list` / `tools/call` → `ToolHost::list` / `ToolHost::call`
//! - `quota/check` / `quota/update` → `QuotaPolicy::check` /
//!   `QuotaPolicy::record`
//! - `turn/end`, `stream/*`, and `heartbeat` are agent → server
//!   notifications and produce no response. The read loop in
//!   [`crate::tunnel::run`] fans them out to the in-memory broker
//!   for SSE subscribers.
//!
//! `session/message` and `session/cancel` are server → agent only
//! per `docs/design/protocol.md` § 3 — if the agent sends them up
//! the wire we treat that as a method-not-found error (the broker
//! never accepts them in the agent → server direction).

use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::hooks::{BootError, QuotaError, ToolError};
use crate::tunnel::run::TunnelContext;

/// Dispatch a single JSON-RPC message from the agent.
///
/// Returns `Some(response)` for requests (have `id`), `None` for
/// notifications.
pub async fn handle_message(text: &str, ctx: &TunnelContext) -> Option<String> {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            warn!("invalid JSON from tunnel: {e}");
            return None;
        }
    };

    let method = parsed.get("method")?.as_str()?;
    let id = parsed.get("id").cloned();
    let params = parsed.get("params").cloned().unwrap_or(Value::Null);

    debug!(method, has_id = id.is_some(), "tunnel message");

    let agent_id = ctx.claims.sub;

    match method {
        "initialize" => Some(jsonrpc_response(id?, handle_initialize(ctx).await)),
        "tools/list" => Some(jsonrpc_response(id?, handle_tools_list(ctx).await)),
        "tools/call" => Some(jsonrpc_response(id?, handle_tools_call(ctx, params).await)),
        "quota/check" => Some(jsonrpc_response(id?, handle_quota_check(ctx).await)),
        "quota/update" => Some(jsonrpc_response(
            id?,
            handle_quota_update(ctx, params).await,
        )),

        // Notifications — agent → server, no response. The read loop
        // fans `stream/*`, `turn/end`, and `heartbeat` out to the
        // in-memory broker for SSE subscribers.
        "stream/textDelta" | "stream/activity" | "stream/toolCall" | "stream/toolResult"
        | "turn/end" | "heartbeat" => {
            debug!(method, %agent_id, "tunnel notification");
            None
        }

        _ => {
            warn!(method, "unknown tunnel method");
            id.map(|id| jsonrpc_response(id, Err((-32601, format!("method not found: {method}")))))
        }
    }
}

// ── Request handlers — each delegates to exactly one hook ──

async fn handle_initialize(ctx: &TunnelContext) -> Result<Value, (i32, String)> {
    ctx.boot_provider
        .initialize(&ctx.claims)
        .await
        .map_err(boot_err)
}

async fn handle_tools_list(ctx: &TunnelContext) -> Result<Value, (i32, String)> {
    ctx.tool_host.list(&ctx.claims).await.map_err(tool_err)
}

async fn handle_tools_call(ctx: &TunnelContext, params: Value) -> Result<Value, (i32, String)> {
    ctx.tool_host
        .call(&ctx.claims, params)
        .await
        .map_err(tool_err)
}

async fn handle_quota_check(ctx: &TunnelContext) -> Result<Value, (i32, String)> {
    ctx.quota_policy
        .check(&ctx.claims)
        .await
        .map(|allowed| json!({ "allowed": allowed }))
        .map_err(quota_err)
}

async fn handle_quota_update(ctx: &TunnelContext, params: Value) -> Result<Value, (i32, String)> {
    ctx.quota_policy
        .record(&ctx.claims, params)
        .await
        .map(|()| json!({}))
        .map_err(quota_err)
}

// ── Hook-error → JSON-RPC error mapping ──
//
// Application error codes start at 1000 per protocol.md § 1.2.
// The exact assignments below are an implementation detail of the
// reference broker, not part of the wire contract.

const CODE_NOT_FOUND: i32 = 1404;
const CODE_INTERNAL: i32 = 1500;
const CODE_EXECUTION_FAILED: i32 = 1502;
const CODE_DENIED: i32 = 1603;

fn boot_err(e: BootError) -> (i32, String) {
    match e {
        BootError::NotFound(_) => (CODE_NOT_FOUND, e.to_string()),
        BootError::Internal(_) => (CODE_INTERNAL, e.to_string()),
    }
}

fn tool_err(e: ToolError) -> (i32, String) {
    match e {
        ToolError::NotFound(_) => (CODE_NOT_FOUND, e.to_string()),
        ToolError::Denied(_) => (CODE_DENIED, e.to_string()),
        ToolError::Execution(_) => (CODE_EXECUTION_FAILED, e.to_string()),
        ToolError::Internal(_) => (CODE_INTERNAL, e.to_string()),
    }
}

fn quota_err(e: QuotaError) -> (i32, String) {
    match e {
        QuotaError::Internal(_) => (CODE_INTERNAL, e.to_string()),
    }
}

/// Format a JSON-RPC 2.0 response.
pub fn jsonrpc_response(id: Value, result: Result<Value, (i32, String)>) -> String {
    let resp = match result {
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    };
    serde_json::to_string(&resp).unwrap()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use uuid::Uuid;

    use super::*;
    use crate::auth::Claims;
    use crate::hooks::{
        BootProvider, DefaultBootProvider, DefaultQuotaPolicy, DefaultToolHost, QuotaPolicy,
        ToolHost,
    };
    use crate::registry::{AgentRegistry, MessageQueue};
    use crate::tunnel::broker::StreamBroker;
    use crate::tunnel::run::TunnelContext;

    fn ctx_default(agent_id: Uuid) -> TunnelContext {
        TunnelContext {
            claims: Claims::agent(agent_id, Some(Uuid::new_v4()), 60, "test"),
            registry: AgentRegistry::new(),
            message_queue: MessageQueue::default(),
            stream_broker: StreamBroker::new(),
            boot_provider: Arc::new(DefaultBootProvider),
            tool_host: Arc::new(DefaultToolHost),
            quota_policy: Arc::new(DefaultQuotaPolicy),
        }
    }

    fn parse(resp: &str) -> Value {
        serde_json::from_str(resp).unwrap()
    }

    // ── jsonrpc_response framing ──

    #[test]
    fn jsonrpc_response_success() {
        let resp = jsonrpc_response(json!(1), Ok(json!({"hello": "world"})));
        let parsed = parse(&resp);
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["result"]["hello"], "world");
        assert!(parsed.get("error").is_none());
    }

    #[test]
    fn jsonrpc_response_error() {
        let resp = jsonrpc_response(json!(2), Err((-32601, "not found".into())));
        let parsed = parse(&resp);
        assert_eq!(parsed["id"], 2);
        assert_eq!(parsed["error"]["code"], -32601);
        assert_eq!(parsed["error"]["message"], "not found");
    }

    // ── default-hook dispatch behaviour ──

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn initialize_delegates_to_boot_provider() {
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, &ctx)
            .await
            .unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["result"]["system_prompt"], "");
        assert_eq!(parsed["result"]["messages"], json!([]));
    }

    #[tokio::test]
    async fn tools_list_delegates_to_tool_host() {
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["result"]["tools"], json!([]));
    }

    #[tokio::test]
    async fn tools_call_default_returns_not_found() {
        let ctx = ctx_default(Uuid::new_v4());
        let frame = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo"}}"#;
        let resp = handle_message(frame, &ctx).await.unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["error"]["code"], 1404);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("echo"));
    }

    #[tokio::test]
    async fn quota_check_default_allows() {
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"quota/check"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["result"]["allowed"], true);
    }

    #[tokio::test]
    async fn quota_update_default_returns_empty_object() {
        let ctx = ctx_default(Uuid::new_v4());
        let frame =
            r#"{"jsonrpc":"2.0","id":1,"method":"quota/update","params":{"input_tokens":10}}"#;
        let resp = handle_message(frame, &ctx).await.unwrap();
        assert_eq!(parse(&resp)["result"], json!({}));
    }

    #[tokio::test]
    async fn turn_save_is_unknown_method() {
        // turn/save is not in the dispatch table. The dispatch
        // table currently understands turn/end (notification) for
        // end-of-turn signalling.
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"turn/save"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn poll_new_messages_is_unknown_method() {
        // poll/newMessages is not in the dispatch table. Message
        // bodies are delivered inline in session/message
        // notifications.
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"poll/newMessages"}"#,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(parse(&resp)["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn turn_end_is_a_notification() {
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","method":"turn/end","params":{"messages":[]}}"#,
            &ctx,
        )
        .await;
        assert!(resp.is_none(), "turn/end is fire-and-forget");
    }

    #[tokio::test]
    async fn session_cancel_from_agent_is_method_not_found() {
        // session/cancel is server → agent only. If the agent ever
        // sends one up the wire, treat it as method not found
        // rather than silently accepting a malformed direction.
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"session/cancel"}"#,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(parse(&resp)["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn session_message_from_agent_is_method_not_found() {
        // session/message is server → agent only.
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"session/message"}"#,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(parse(&resp)["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn heartbeat_is_a_notification() {
        let ctx = ctx_default(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","method":"heartbeat"}"#, &ctx).await;
        assert!(resp.is_none());
    }

    // ── operator-hook integration ──

    struct CountingBoot {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl BootProvider for CountingBoot {
        async fn initialize(&self, _claims: &Claims) -> Result<Value, BootError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "system_prompt": "operator" }))
        }
    }

    struct FlakyTools;
    #[async_trait]
    impl ToolHost for FlakyTools {
        async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
            Ok(json!({ "tools": [{ "name": "fail" }] }))
        }
        async fn call(&self, _claims: &Claims, _req: Value) -> Result<Value, ToolError> {
            Err(ToolError::Execution("network down".into()))
        }
    }

    struct ForbiddenTools;
    #[async_trait]
    impl ToolHost for ForbiddenTools {
        async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
            Ok(json!({ "tools": [] }))
        }
        async fn call(&self, _claims: &Claims, _req: Value) -> Result<Value, ToolError> {
            Err(ToolError::Denied("not authorised".into()))
        }
    }

    struct MissingBoot;
    #[async_trait]
    impl BootProvider for MissingBoot {
        async fn initialize(&self, claims: &Claims) -> Result<Value, BootError> {
            Err(BootError::NotFound(claims.sub.to_string()))
        }
    }

    struct BrokenBoot;
    #[async_trait]
    impl BootProvider for BrokenBoot {
        async fn initialize(&self, _claims: &Claims) -> Result<Value, BootError> {
            Err(BootError::Internal("boot db down".into()))
        }
    }

    struct BrokenTools;
    #[async_trait]
    impl ToolHost for BrokenTools {
        async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
            Err(ToolError::Internal("tool db down".into()))
        }
        async fn call(&self, _claims: &Claims, _req: Value) -> Result<Value, ToolError> {
            Err(ToolError::Internal("tool db down".into()))
        }
    }

    struct DenyAll;
    #[async_trait]
    impl QuotaPolicy for DenyAll {
        async fn check(&self, _claims: &Claims) -> Result<bool, QuotaError> {
            Ok(false)
        }
        async fn record(&self, _claims: &Claims, _usage: Value) -> Result<(), QuotaError> {
            Err(QuotaError::Internal("recording disabled".into()))
        }
    }

    fn ctx_with(
        boot: Arc<dyn BootProvider>,
        tools: Arc<dyn ToolHost>,
        quota: Arc<dyn QuotaPolicy>,
    ) -> TunnelContext {
        TunnelContext {
            claims: Claims::agent(Uuid::new_v4(), None, 60, "test"),
            registry: AgentRegistry::new(),
            message_queue: MessageQueue::default(),
            stream_broker: StreamBroker::new(),
            boot_provider: boot,
            tool_host: tools,
            quota_policy: quota,
        }
    }

    #[tokio::test]
    async fn initialize_uses_operator_boot_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ctx = ctx_with(
            Arc::new(CountingBoot {
                calls: calls.clone(),
            }),
            Arc::new(DefaultToolHost),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["result"]["system_prompt"], "operator");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tools_call_propagates_execution_error_as_1502() {
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(FlakyTools),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fail"}}"#,
            &ctx,
        )
        .await
        .unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["error"]["code"], 1502);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("network down"));
    }

    #[tokio::test]
    async fn flaky_tools_list_returns_the_stub_entry() {
        // The list side of the FlakyTools mock exposes a single
        // stub tool named "fail"; the call side (tested above)
        // returns an Execution error.
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(FlakyTools),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["result"]["tools"][0]["name"], "fail");
    }

    #[tokio::test]
    async fn forbidden_tools_list_returns_empty() {
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(ForbiddenTools),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["result"]["tools"], json!([]));
    }

    #[tokio::test]
    async fn broken_tools_call_internal_error_maps_to_1500() {
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(BrokenTools),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x"}}"#,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(parse(&resp)["error"]["code"], 1500);
    }

    #[tokio::test]
    async fn quota_check_can_refuse() {
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(DefaultToolHost),
            Arc::new(DenyAll),
        );
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"quota/check"}"#, &ctx)
            .await
            .unwrap();
        assert_eq!(parse(&resp)["result"]["allowed"], false);
    }

    #[tokio::test]
    async fn tools_call_denied_maps_to_1603() {
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(ForbiddenTools),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x"}}"#,
            &ctx,
        )
        .await
        .unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["error"]["code"], 1603);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not authorised"));
    }

    #[tokio::test]
    async fn initialize_not_found_maps_to_1404() {
        let agent_id = Uuid::new_v4();
        let ctx = ctx_with(
            Arc::new(MissingBoot),
            Arc::new(DefaultToolHost),
            Arc::new(DefaultQuotaPolicy),
        );
        // Override claims so the message names a known agent.
        let ctx = TunnelContext {
            claims: Claims::agent(agent_id, None, 60, "test"),
            ..ctx
        };
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, &ctx)
            .await
            .unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["error"]["code"], 1404);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&agent_id.to_string()));
    }

    #[tokio::test]
    async fn initialize_internal_error_maps_to_1500() {
        let ctx = ctx_with(
            Arc::new(BrokenBoot),
            Arc::new(DefaultToolHost),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, &ctx)
            .await
            .unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["error"]["code"], 1500);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("boot db down"));
    }

    #[tokio::test]
    async fn tools_list_internal_error_maps_to_1500() {
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(BrokenTools),
            Arc::new(DefaultQuotaPolicy),
        );
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, &ctx)
            .await
            .unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["error"]["code"], 1500);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("tool db down"));
    }

    #[tokio::test]
    async fn quota_update_propagates_internal_error_as_1500() {
        let ctx = ctx_with(
            Arc::new(DefaultBootProvider),
            Arc::new(DefaultToolHost),
            Arc::new(DenyAll),
        );
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"quota/update"}"#, &ctx)
            .await
            .unwrap();
        let parsed = parse(&resp);
        assert_eq!(parsed["error"]["code"], 1500);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("recording disabled"));
    }

    #[tokio::test]
    async fn invalid_json_returns_none() {
        let ctx = ctx_default(Uuid::new_v4());
        assert!(handle_message("not json", &ctx).await.is_none());
    }

    #[tokio::test]
    async fn missing_method_returns_none() {
        let ctx = ctx_default(Uuid::new_v4());
        assert!(handle_message(r#"{"jsonrpc":"2.0","id":1}"#, &ctx)
            .await
            .is_none());
    }
}
