//! JSON-RPC 2.0 method dispatch for the over/ACP tunnel.
//!
//! Method names are sourced from `docs/design/protocol.md` § 3.
//!
//! `initialize`, `tools/call`, `turn/save`, and `poll/newMessages`
//! currently return a 1503 "awaiting hook integration" error: the
//! `Claims` shape no longer carries the controlplane-era conversation
//! id these handlers depended on, and the operator hooks
//! (`BootProvider`, `ToolHost`, `QuotaPolicy`) that will replace them
//! have not landed yet.

use serde_json::{json, Value};
use tracing::{debug, warn};

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

    debug!(method, has_id = id.is_some(), "tunnel message");

    let agent_id = ctx.claims.sub;

    match method {
        "initialize" => Some(jsonrpc_response(id?, awaiting_hooks("initialize"))),
        "tools/list" => Some(jsonrpc_response(id?, Ok(json!({ "tools": [] })))),
        "tools/call" => Some(jsonrpc_response(id?, awaiting_hooks("tools/call"))),
        "turn/save" => Some(jsonrpc_response(id?, awaiting_hooks("turn/save"))),
        "quota/check" => Some(jsonrpc_response(id?, Ok(json!({ "allowed": true })))),
        "quota/update" => Some(jsonrpc_response(id?, Ok(json!({})))),
        "poll/newMessages" => Some(jsonrpc_response(id?, awaiting_hooks("poll/newMessages"))),

        // Notifications — already fanned out to the broker by the read
        // loop. Nothing to respond with.
        "stream/textDelta" | "stream/activity" | "stream/toolCall" | "stream/toolResult"
        | "session/message" | "heartbeat" => {
            debug!(method, %agent_id, "tunnel notification");
            None
        }

        _ => {
            warn!(method, "unknown tunnel method");
            id.map(|id| jsonrpc_response(id, Err((-32601, format!("method not found: {method}")))))
        }
    }
}

/// Error returned by handlers whose operator hook is not yet wired
/// up. Code 1503 maps to HTTP 503-ish "service in transitional state".
fn awaiting_hooks(method: &str) -> Result<Value, (i32, String)> {
    Err((
        1503,
        format!("{method} awaiting hook integration"),
    ))
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
    use super::*;

    #[test]
    fn jsonrpc_response_success() {
        let resp = jsonrpc_response(json!(1), Ok(json!({"hello": "world"})));
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["result"]["hello"], "world");
        assert!(parsed.get("error").is_none());
    }

    #[test]
    fn jsonrpc_response_error() {
        let resp = jsonrpc_response(json!(2), Err((-32601, "not found".into())));
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["id"], 2);
        assert_eq!(parsed["error"]["code"], -32601);
        assert_eq!(parsed["error"]["message"], "not found");
    }

    use std::sync::Arc;

    use uuid::Uuid;

    use crate::auth::Claims;
    use crate::store::InMemoryStore;
    use crate::tunnel::broker::StreamBroker;
    use crate::tunnel::run::TunnelContext;
    use crate::tunnel::session_manager::new_session_manager;

    fn ctx_with(agent_id: Uuid) -> TunnelContext {
        TunnelContext {
            claims: Claims::agent(agent_id, Some(Uuid::new_v4()), 60, "test"),
            store: Arc::new(InMemoryStore::new()),
            sessions: new_session_manager(),
            stream_broker: StreamBroker::new(),
        }
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let ctx = ctx_with(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#, &ctx)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn quota_check_always_allows() {
        let ctx = ctx_with(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":7,"method":"quota/check"}"#, &ctx)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["result"]["allowed"], true);
    }

    #[tokio::test]
    async fn heartbeat_is_a_notification() {
        let ctx = ctx_with(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","method":"heartbeat"}"#, &ctx).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn initialize_returns_transitional_error() {
        let ctx = ctx_with(Uuid::new_v4());
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, &ctx)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], 1503);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("initialize"));
    }
}
