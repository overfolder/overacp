//! JSON-RPC 2.0 method dispatch for the over/ACP tunnel.
//!
//! Method names are sourced from `docs/design/protocol.md` § 3.

use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::store::StoreError;
use crate::tunnel::run::TunnelContext;

const POLL_LIMIT: usize = 10;

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

    let session_id = ctx.claims.conv;

    match method {
        "initialize" => Some(jsonrpc_response(id?, handle_initialize(ctx).await)),
        "tools/list" => Some(jsonrpc_response(id?, Ok(json!({ "tools": [] })))),
        "tools/call" => Some(jsonrpc_response(
            id?,
            Err((-32601, "tools/call not yet implemented".into())),
        )),
        "turn/save" => Some(jsonrpc_response(id?, handle_turn_save(ctx, &params).await)),
        "quota/check" => Some(jsonrpc_response(id?, Ok(json!({ "allowed": true })))),
        "quota/update" => Some(jsonrpc_response(id?, Ok(json!({})))),
        "poll/newMessages" => Some(jsonrpc_response(id?, handle_poll(ctx).await)),

        // Notifications — already fanned out to the broker by the read
        // loop. Nothing to respond with.
        "stream/textDelta" | "stream/activity" | "stream/toolCall" | "stream/toolResult"
        | "session/message" | "heartbeat" => {
            debug!(method, %session_id, "tunnel notification");
            None
        }

        _ => {
            warn!(method, "unknown tunnel method");
            id.map(|id| jsonrpc_response(id, Err((-32601, format!("method not found: {method}")))))
        }
    }
}

async fn handle_initialize(ctx: &TunnelContext) -> Result<Value, (i32, String)> {
    let conv_id = ctx.claims.conv;
    let conv = ctx
        .store
        .get_conversation(conv_id)
        .await
        .map_err(store_err)?;
    if conv.is_none() {
        return Err((1404, format!("conversation {conv_id} not found")));
    }
    let messages = ctx
        .store
        .list_messages(conv_id, None)
        .await
        .map_err(store_err)?;
    let recent: Vec<_> = messages
        .iter()
        .rev()
        .take(POLL_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(json!({
        "system_prompt": "",
        "messages": recent,
        "conversation_id": conv_id,
        "tools_config": {},
    }))
}

async fn handle_turn_save(ctx: &TunnelContext, params: &Value) -> Result<Value, (i32, String)> {
    let conv_id = ctx.claims.conv;
    let messages = params
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = m.get("content").cloned().unwrap_or(Value::Null);
        ctx.store
            .append_message(conv_id, role, content)
            .await
            .map_err(store_err)?;
    }
    Ok(json!({}))
}

async fn handle_poll(ctx: &TunnelContext) -> Result<Value, (i32, String)> {
    let conv_id = ctx.claims.conv;
    let cursor = {
        let handle = ctx
            .sessions
            .get(&conv_id)
            .ok_or((1500, "no session".into()))?;
        let guard = handle.poll_cursor.lock().await;
        let cur = *guard;
        drop(guard);
        drop(handle);
        cur
    };
    let msgs = ctx
        .store
        .list_messages(conv_id, cursor)
        .await
        .map_err(store_err)?;
    let limited: Vec<_> = msgs.into_iter().take(POLL_LIMIT).collect();
    if let Some(last) = limited.last() {
        if let Some(handle) = ctx.sessions.get(&conv_id) {
            *handle.poll_cursor.lock().await = Some(last.id);
        }
    }
    Ok(json!({ "messages": limited }))
}

fn store_err(e: StoreError) -> (i32, String) {
    match e {
        StoreError::NotFound => (1404, "not found".into()),
        other => (1500, other.to_string()),
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

    fn ctx_with(conv: Uuid) -> TunnelContext {
        TunnelContext {
            claims: Claims {
                sub: Uuid::new_v4(),
                user: Uuid::new_v4(),
                conv,
                exp: 0,
                iss: "test".into(),
            },
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
}
