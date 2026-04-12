//! End-to-end WebSocket tunnel integration tests.
//!
//! Spins up `axum::serve` on a random port, opens a live
//! `tokio-tungstenite` WebSocket client against `/tunnel/:agent_id`
//! with a minted agent JWT, drives the dispatch table
//! (`initialize`, `tools/list`, `quota/check`, `heartbeat`,
//! `turn/end`), and verifies that:
//!
//! 1. The tunnel read/write loops run to completion over a real
//!    WebSocket connection.
//! 2. `turn/end` notifications are fanned out to the in-memory
//!    stream broker.
//! 3. `session/message` REST pushes are delivered inline to the
//!    connected agent.
//! 4. Messages buffered via `MessageQueue` while the agent was
//!    disconnected are drained on reconnect.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use overacp_server::auth::{Authenticator, Claims};
use overacp_server::{router, AppState, StaticJwtAuthenticator, ToolError, ToolHost};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;
use uuid::Uuid;

type TestWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

const SIGNING_KEY: &str = "tunnel-e2e-key";
const ISSUER: &str = "overacp";

/// Bind a router to a random port and return the bound address plus
/// a handle to the background server task.
async fn spawn_server(state: AppState) -> (SocketAddr, JoinHandle<()>) {
    let app = router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, handle)
}

fn fresh_state() -> (AppState, Arc<dyn Authenticator>) {
    let auth: Arc<dyn Authenticator> = Arc::new(StaticJwtAuthenticator::new(SIGNING_KEY, ISSUER));
    let state = AppState::new(auth.clone());
    (state, auth)
}

async fn open_tunnel(addr: SocketAddr, agent_id: Uuid, token: &str) -> TestWs {
    let url = format!("ws://{addr}/tunnel/{agent_id}");
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _resp) = connect_async(req).await.expect("ws upgrade");
    ws
}

/// Poll a receiver with a timeout so a misbehaving test doesn't
/// hang forever.
async fn recv_text(ws: &mut TestWs) -> Option<String> {
    let fut = async {
        while let Some(msg) = ws.next().await {
            match msg.ok()? {
                Message::Text(t) => return Some(t.to_string()),
                Message::Ping(_) | Message::Pong(_) => continue,
                _ => return None,
            }
        }
        None
    };
    timeout(Duration::from_secs(2), fut).await.ok().flatten()
}

#[tokio::test]
async fn tunnel_initialize_round_trip_against_live_server() {
    let (state, auth) = fresh_state();
    let (addr, _server) = spawn_server(state).await;

    let agent_id = Uuid::new_v4();
    let token = auth
        .mint(&Claims::agent(agent_id, Some(Uuid::new_v4()), 60, ISSUER))
        .unwrap();

    let mut ws = open_tunnel(addr, agent_id, &token).await;

    // Send `initialize`.
    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#.into(),
    ))
    .await
    .unwrap();

    let resp = recv_text(&mut ws).await.expect("initialize response");
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["id"], 1);
    // DefaultBootProvider returns the empty stub.
    assert_eq!(parsed["result"]["system_prompt"], "");
    assert_eq!(parsed["result"]["messages"], json!([]));

    ws.close(None).await.ok();
}

#[tokio::test]
async fn tunnel_tools_list_and_quota_check_round_trips() {
    let (state, auth) = fresh_state();
    let (addr, _server) = spawn_server(state).await;
    let agent_id = Uuid::new_v4();
    let token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let mut ws = open_tunnel(addr, agent_id, &token).await;

    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.into(),
    ))
    .await
    .unwrap();
    let resp = recv_text(&mut ws).await.unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["result"]["tools"], json!([]));

    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":2,"method":"quota/check"}"#.into(),
    ))
    .await
    .unwrap();
    let resp = recv_text(&mut ws).await.unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["result"]["allowed"], true);

    ws.close(None).await.ok();
}

#[tokio::test]
async fn tunnel_heartbeat_is_a_notification_and_gets_no_response() {
    let (state, auth) = fresh_state();
    let (addr, _server) = spawn_server(state).await;
    let agent_id = Uuid::new_v4();
    let token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let mut ws = open_tunnel(addr, agent_id, &token).await;

    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","method":"heartbeat"}"#.into(),
    ))
    .await
    .unwrap();

    // Follow-up request to prove the connection survives the
    // notification with no reply.
    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":1,"method":"quota/check"}"#.into(),
    ))
    .await
    .unwrap();
    let resp = recv_text(&mut ws).await.expect("quota response");
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["id"], 1);

    ws.close(None).await.ok();
}

#[tokio::test]
async fn rest_push_delivers_session_message_inline_to_live_tunnel() {
    let (state, auth) = fresh_state();
    let app = router(state.clone());
    let (addr, _server) = spawn_server(state).await;

    let agent_id = Uuid::new_v4();
    let agent_token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let admin_token = auth
        .mint(&Claims::admin(Uuid::new_v4(), 60, ISSUER))
        .unwrap();

    // Open the agent's tunnel first.
    let mut ws = open_tunnel(addr, agent_id, &agent_token).await;

    // Give the server a beat to register the tunnel. Without this,
    // the subsequent REST push can race the registration and land
    // in the queue instead of going inline.
    use tokio::time::sleep;
    sleep(Duration::from_millis(50)).await;

    // Push a `session/message` via the admin REST handler. We use
    // `app.oneshot` against the router directly rather than a real
    // HTTP request, since the state is the same AppState the live
    // server is serving.
    let req = HttpRequest::builder()
        .method("POST")
        .uri(format!("/agents/{agent_id}/messages"))
        .header("authorization", format!("Bearer {admin_token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "role": "user", "content": "live-push" }).to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    use axum::http::StatusCode;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(parsed["delivery"], "live");

    // The agent receives the inline `session/message` notification.
    let frame = recv_text(&mut ws).await.expect("session/message");
    let parsed: Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(parsed["method"], "session/message");
    assert_eq!(parsed["params"]["content"], "live-push");

    ws.close(None).await.ok();
}

#[tokio::test]
async fn reconnect_drains_buffered_session_messages() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();

    // Buffer two notifications ahead of time, before the agent has
    // ever connected. `send_message` enqueues into
    // `state.message_queue` because the registry is empty.
    let admin_token = auth
        .mint(&Claims::admin(Uuid::new_v4(), 60, ISSUER))
        .unwrap();
    {
        let app = router(state.clone());
        for n in ["first", "second"] {
            let req = HttpRequest::builder()
                .method("POST")
                .uri(format!("/agents/{agent_id}/messages"))
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "role": "user", "content": n }).to_string(),
                ))
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            let body = res.into_body().collect().await.unwrap().to_bytes();
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(parsed["delivery"], "queued");
        }
    }
    assert_eq!(state.message_queue.len(agent_id), 2);

    // Now start the server and open the tunnel. The write loop
    // drains the queue before serving live traffic.
    let (addr, _server) = spawn_server(state.clone()).await;
    let agent_token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let mut ws = open_tunnel(addr, agent_id, &agent_token).await;

    let first = recv_text(&mut ws).await.expect("first");
    let second = recv_text(&mut ws).await.expect("second");
    assert!(first.contains(r#""content":"first""#));
    assert!(second.contains(r#""content":"second""#));

    // And the queue is empty now.
    assert_eq!(state.message_queue.len(agent_id), 0);

    ws.close(None).await.ok();
}

#[tokio::test]
async fn turn_end_fans_out_to_sse_subscribers() {
    // The read loop sniffs incoming `turn/end` frames and forwards
    // them to the per-agent `StreamBroker`, which the REST SSE
    // handler subscribes to. Here we subscribe directly through
    // the public broker surface; the tunnel read loop does the
    // forwarding for us.
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();

    let mut rx = state.stream_broker.subscribe(agent_id);

    let (addr, _server) = spawn_server(state).await;
    let token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let mut ws = open_tunnel(addr, agent_id, &token).await;

    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","method":"turn/end","params":{"messages":[]}}"#.into(),
    ))
    .await
    .unwrap();

    let fanned = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("broker fan-out timeout")
        .expect("broker fan-out closed");
    let parsed: Value = serde_json::from_str(&fanned).unwrap();
    assert_eq!(parsed["method"], "turn/end");

    ws.close(None).await.ok();
}

#[tokio::test]
async fn multimodal_content_survives_rest_push_to_tunnel() {
    // Verify that multimodal content blocks (text + image_url) arrive
    // at the agent tunnel intact — the broker must not inspect, modify,
    // or drop any content block.
    let (state, auth) = fresh_state();
    let app = router(state.clone());
    let (addr, _server) = spawn_server(state).await;

    let agent_id = Uuid::new_v4();
    let agent_token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let admin_token = auth
        .mint(&Claims::admin(Uuid::new_v4(), 60, ISSUER))
        .unwrap();

    let mut ws = open_tunnel(addr, agent_id, &agent_token).await;
    use tokio::time::sleep;
    sleep(Duration::from_millis(50)).await;

    let multimodal_content = json!([
        {"type": "text", "text": "describe this image"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
        {"type": "future_block", "payload": [1,2,3]}
    ]);

    let req = HttpRequest::builder()
        .method("POST")
        .uri(format!("/agents/{agent_id}/messages"))
        .header("authorization", format!("Bearer {admin_token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "role": "user", "content": multimodal_content }).to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    use axum::http::StatusCode;
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let frame = recv_text(&mut ws).await.expect("session/message");
    let parsed: Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(parsed["method"], "session/message");

    let content = &parsed["params"]["content"];
    assert!(content.is_array(), "content should be an array of blocks");
    let blocks = content.as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], "describe this image");
    assert_eq!(blocks[1]["type"], "image_url");
    assert!(blocks[1]["image_url"]["url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png"));
    // Unknown block type preserved verbatim
    assert_eq!(blocks[2]["type"], "future_block");
    assert_eq!(blocks[2]["payload"], json!([1, 2, 3]));

    ws.close(None).await.ok();
}

#[tokio::test]
async fn multimodal_turn_end_fans_out_to_sse_with_blocks_intact() {
    // Verify that a turn/end notification with multimodal content
    // blocks is forwarded to SSE subscribers without modification.
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();

    let mut rx = state.stream_broker.subscribe(agent_id);

    let (addr, _server) = spawn_server(state).await;
    let token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let mut ws = open_tunnel(addr, agent_id, &token).await;

    let turn_end = json!({
        "jsonrpc": "2.0",
        "method": "turn/end",
        "params": {
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "what's this?"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}
                    ]
                },
                {"role": "assistant", "content": "A red pixel."}
            ],
            "usage": {"input_tokens": 100, "output_tokens": 10}
        }
    });

    ws.send(Message::Text(turn_end.to_string().into()))
        .await
        .unwrap();

    let fanned = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("fan-out timeout")
        .expect("fan-out closed");
    let parsed: Value = serde_json::from_str(&fanned).unwrap();
    assert_eq!(parsed["method"], "turn/end");

    // Verify multimodal user content preserved
    let user_msg = &parsed["params"]["messages"][0];
    let user_content = user_msg["content"].as_array().unwrap();
    assert_eq!(user_content.len(), 2);
    assert_eq!(user_content[0]["type"], "text");
    assert_eq!(user_content[1]["type"], "image_url");
    assert!(user_content[1]["image_url"]["url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png"));

    // Verify assistant plain string content preserved
    assert_eq!(parsed["params"]["messages"][1]["content"], "A red pixel.");

    ws.close(None).await.ok();
}

#[tokio::test]
async fn rejects_mismatched_agent_jwt_on_upgrade() {
    let (state, auth) = fresh_state();
    let (addr, _server) = spawn_server(state).await;

    let path_id = Uuid::new_v4();
    let other = Uuid::new_v4();
    // Token minted for `other`, path is `path_id` — upgrade should
    // fail with 403 before the WS handshake completes.
    let token = auth.mint(&Claims::agent(other, None, 60, ISSUER)).unwrap();

    let url = format!("ws://{addr}/tunnel/{path_id}");
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let err = connect_async(req).await.expect_err("should reject");
    // tungstenite surfaces upgrade rejections as `Http(response)`
    // with the status code. We just assert the error exists.
    let msg = err.to_string();
    assert!(
        msg.contains("403") || msg.contains("Forbidden"),
        "msg = {msg}"
    );
}

// ── Operator-provided ToolHost over a real WebSocket ──────────────

/// Trivial ToolHost that exposes one tool and echoes call arguments.
struct EchoToolHost;

#[async_trait]
impl ToolHost for EchoToolHost {
    async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
        Ok(json!({
            "tools": [{
                "name": "echo",
                "description": "echo input back",
                "inputSchema": {
                    "type": "object",
                    "properties": { "msg": { "type": "string" } },
                    "required": ["msg"]
                }
            }]
        }))
    }
    async fn call(&self, _claims: &Claims, req: Value) -> Result<Value, ToolError> {
        let msg = req["arguments"]["msg"].as_str().unwrap_or("?");
        Ok(json!({
            "content": [{"type": "text", "text": format!("echo: {msg}")}],
            "isError": false
        }))
    }
}

#[tokio::test]
async fn tunnel_operator_tools_list_and_call_over_real_ws() {
    let auth: Arc<dyn Authenticator> = Arc::new(StaticJwtAuthenticator::new(SIGNING_KEY, ISSUER));
    let state = AppState::new(auth.clone()).with_tool_host(Arc::new(EchoToolHost));
    let (addr, _server) = spawn_server(state).await;

    let agent_id = Uuid::new_v4();
    let token = auth
        .mint(&Claims::agent(agent_id, None, 60, ISSUER))
        .unwrap();
    let mut ws = open_tunnel(addr, agent_id, &token).await;

    // tools/list should return the operator-provided tool.
    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.into(),
    ))
    .await
    .unwrap();
    let resp = recv_text(&mut ws).await.unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["result"]["tools"][0]["name"], "echo");

    // tools/call should execute and return the result.
    ws.send(Message::Text(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "echo",
                "arguments": { "msg": "hello" }
            }
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let resp = recv_text(&mut ws).await.unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["id"], 2);
    assert_eq!(parsed["result"]["content"][0]["text"], "echo: hello");

    ws.close(None).await.ok();
}
