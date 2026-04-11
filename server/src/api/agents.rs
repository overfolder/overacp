//! axum routes for `/agents/{id}/...` — the REST adapters over the
//! JSON-RPC tunnel.
//!
//! These handlers will be rewritten in Phase 4b of the broker refactor
//! to push `session/message` directly down the tunnel and key the
//! session table on `agent_id` (the JWT `sub`). Right now they still
//! call into the controlplane-era `SessionStore`; the in-place
//! `Claims` shape change in Phase 1 left this surface compiling but
//! transitional. See `SPEC.md` for the target architecture.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::registry::QueueError;
use crate::state::AppState;
use crate::store::{Agent, Message};

/// Mount the `/agents/{id}/...` § 3.5 routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/agents/:id/messages",
            post(send_message).get(list_messages),
        )
        .route("/agents/:id/stream", get(stream_events))
        .route("/agents/:id/cancel", post(cancel_turn))
}

// ── wire types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageRequest {
    /// Role for the appended message. Defaults to `"user"`.
    #[serde(default = "default_role")]
    pub role: String,
    /// Opaque message content, persisted verbatim.
    pub content: Value,
}

fn default_role() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct SendMessageResponse {
    pub message: Message,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesQuery {
    /// If present, only messages created after this id are returned.
    pub since: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesListResponse {
    pub messages: Vec<Message>,
}

// ── handlers ────────────────────────────────────────────────────

async fn require_agent(state: &AppState, id: &str) -> Result<Agent, ApiError> {
    state
        .store
        .get_agent(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{id}'")))
}

/// `POST /agents/{id}/messages` — write the message into the
/// conversation table (legacy, still here until Phase 5 deletes the
/// store) and push a `session/message` notification down the agent's
/// tunnel with the body inline, as in protocol.md § 3.1.
///
/// If the agent's tunnel is currently disconnected, the notification
/// is buffered in the per-agent `MessageQueue` and drained when the
/// tunnel next reconnects (`run_tunnel` calls `MessageQueue::drain`
/// before yielding to the read loop).
async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageResponse>), ApiError> {
    if req.role.is_empty() {
        return Err(ApiError::BadRequest("'role' must not be empty".into()));
    }
    let agent = require_agent(&state, &id).await?;

    let notif = json!({
        "jsonrpc": "2.0",
        "method": "session/message",
        "params": {
            "role": req.role,
            "content": req.content,
        },
    })
    .to_string();

    // Decide + execute the delivery plan BEFORE touching the store.
    // If the plan fails (e.g. queue at capacity), we want to return
    // 503 without leaving a persisted-but-undelivered message in the
    // conversation log that a client retry would duplicate.
    if let Some(handle) = state.sessions.get(&agent.conversation_id) {
        // Live tunnel — deliver inline.
        let _ = handle.tx.send(notif);
    } else if let Some(jwt_sub) = uuid_from_agent_id(&agent.id) {
        // No live tunnel — buffer for the next reconnect. The
        // routing key here is the agent's UUID (the JWT `sub`),
        // not the legacy `conversation_id`; once Phase 4b lands
        // the registry will be the only routing key.
        //
        // If the per-agent queue is at capacity we surface that as
        // 503 SERVICE_UNAVAILABLE so the operator's REST client
        // sees real back-pressure rather than a phantom success.
        if let Err(QueueError::Full { capacity, .. }) =
            state.message_queue.push(jwt_sub, notif)
        {
            return Err(ApiError::ServiceUnavailable(format!(
                "agent {jwt_sub} message queue is full ({capacity}); retry later"
            )));
        }
    }
    // else: no live tunnel AND the legacy agent row has a non-UUID
    // id, so we can't route through the new queue either. The
    // notification is silently dropped — this path only exists for
    // legacy rows that predate Phase 4a and will be removed in
    // Phase 4b alongside the legacy `Agent` type.

    // Delivery (or buffering) succeeded — now persist. The store
    // still owns the conversation log until Phase 5 strips it.
    let message = state
        .store
        .append_message(agent.conversation_id, &req.role, req.content)
        .await?;

    Ok((StatusCode::CREATED, Json(SendMessageResponse { message })))
}

/// Try to parse `agent.id` (a `String` in the legacy `Agent` row)
/// as a `Uuid` for use as a `MessageQueue` routing key. Returns
/// `None` for non-UUID legacy ids; those agents simply lose their
/// disconnected pushes — the legacy schema predates Phase 4a.
fn uuid_from_agent_id(id: &str) -> Option<Uuid> {
    Uuid::parse_str(id).ok()
}

/// `GET /agents/{id}/messages?since=…` — poll conversation history.
async fn list_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<MessagesListResponse>, ApiError> {
    let agent = require_agent(&state, &id).await?;
    let messages = state
        .store
        .list_messages(agent.conversation_id, query.since)
        .await?;
    Ok(Json(MessagesListResponse { messages }))
}

/// `GET /agents/{id}/stream` — SSE fan-out of the agent's
/// `stream/*` notifications from the in-memory broker.
async fn stream_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let agent = require_agent(&state, &id).await?;
    let rx = state.stream_broker.subscribe(agent.conversation_id);
    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(text) => {
                    return Some((Ok::<Event, Infallible>(Event::default().data(text)), rx));
                }
                // Slow consumer — skip the missed frames and keep going.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// `POST /agents/{id}/cancel` — inject a cancel notification down
/// the tunnel. No-op (but still 202) if the tunnel isn't currently
/// connected: there's nothing in flight to cancel.
async fn cancel_turn(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let agent = require_agent(&state, &id).await?;
    if let Some(handle) = state.sessions.get(&agent.conversation_id) {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {},
        });
        let _ = handle.tx.send(notif.to_string());
    }
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use chrono::Utc;
    use serde_json::json;
    use tokio::sync::{mpsc, Mutex};
    use uuid::Uuid;

    use super::*;
    use crate::api::default_registry;
    use crate::auth::{Claims, StaticJwtAuthenticator};
    use crate::state::AppState;
    use crate::store::{Agent, AgentStatus, InMemoryStore};
    use crate::tunnel::session_manager::TunnelHandle;

    async fn seed_agent(state: &AppState) -> Agent {
        // Use a UUID id so the buffer-on-disconnect path can route
        // by it. Legacy non-UUID ids still work, they just don't
        // get queue buffering.
        seed_agent_with_id(state, Uuid::new_v4().to_string()).await
    }

    async fn seed_agent_with_id(state: &AppState, id: String) -> Agent {
        let conv = state
            .store
            .create_conversation("user-1")
            .await
            .expect("create_conversation");
        let agent = Agent {
            id,
            user: "user-1".into(),
            conversation_id: conv.id,
            pool_name: "pool-a".into(),
            node_id: "node-1".into(),
            image: "img".into(),
            status: AgentStatus::Running,
            metadata: json!({}),
            created_at: Utc::now(),
        };
        state
            .store
            .create_agent(agent.clone())
            .await
            .expect("create_agent");
        agent
    }

    fn test_state() -> AppState {
        AppState::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test", "overacp")),
        )
    }

    fn install_fake_tunnel(state: &AppState, conv: Uuid) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        state.sessions.insert(
            conv,
            TunnelHandle {
                tx,
                claims: Claims::agent(Uuid::new_v4(), Some(Uuid::new_v4()), 60, "test"),
                last_activity: Instant::now(),
                poll_cursor: Mutex::new(None),
            },
        );
        rx
    }

    #[tokio::test]
    async fn send_message_persists_and_notifies() {
        let state = test_state();
        let agent = seed_agent(&state).await;
        let mut tunnel_rx = install_fake_tunnel(&state, agent.conversation_id);

        let (status, Json(resp)) = send_message(
            State(state.clone()),
            Path(agent.id.clone()),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!({ "text": "hello" }),
            }),
        )
        .await
        .expect("send_message");

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(resp.message.role, "user");

        // Persisted.
        let listed = state
            .store
            .list_messages(agent.conversation_id, None)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);

        // Notified.
        let frame = tunnel_rx.recv().await.expect("tunnel frame");
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["method"], "session/message");
    }

    #[tokio::test]
    async fn list_messages_honours_since_cursor() {
        let state = test_state();
        let agent = seed_agent(&state).await;

        let m1 = state
            .store
            .append_message(agent.conversation_id, "user", json!("one"))
            .await
            .unwrap();
        let _m2 = state
            .store
            .append_message(agent.conversation_id, "user", json!("two"))
            .await
            .unwrap();

        let Json(resp) = list_messages(
            State(state),
            Path(agent.id),
            Query(MessagesQuery { since: Some(m1.id) }),
        )
        .await
        .expect("list_messages");

        assert_eq!(resp.messages.len(), 1);
        assert_eq!(resp.messages[0].content, json!("two"));
    }

    #[tokio::test]
    async fn cancel_emits_notification_when_connected() {
        let state = test_state();
        let agent = seed_agent(&state).await;
        let mut tunnel_rx = install_fake_tunnel(&state, agent.conversation_id);

        let status = cancel_turn(State(state), Path(agent.id))
            .await
            .expect("cancel_turn");
        assert_eq!(status, StatusCode::ACCEPTED);

        let frame = tunnel_rx.recv().await.expect("tunnel frame");
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["method"], "session/cancel");
    }

    #[tokio::test]
    async fn cancel_without_tunnel_still_accepted() {
        let state = test_state();
        let agent = seed_agent(&state).await;
        let status = cancel_turn(State(state), Path(agent.id))
            .await
            .expect("cancel_turn");
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn send_message_returns_503_when_queue_full() {
        use crate::registry::MessageQueue;

        // Build state with a tiny per-agent queue capacity so we can
        // overflow it deterministically. The default capacity is 64.
        let state = AppState::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test", "overacp")),
        );
        let state = AppState {
            message_queue: MessageQueue::new(1),
            ..state
        };
        let agent_uuid = Uuid::new_v4();
        let agent = seed_agent_with_id(&state, agent_uuid.to_string()).await;

        // First message buffers fine.
        let _ = send_message(
            State(state.clone()),
            Path(agent.id.clone()),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!("a"),
            }),
        )
        .await
        .expect("first push");

        // Second message overflows the queue → 503.
        let err = send_message(
            State(state.clone()),
            Path(agent.id.clone()),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!("b"),
            }),
        )
        .await
        .expect_err("queue full");
        assert!(matches!(err, ApiError::ServiceUnavailable(_)));

        // Invariant: the rejected push must NOT leave an orphan
        // persisted message in the conversation log. Only the
        // first (successful) push should be visible.
        let persisted = state
            .store
            .list_messages(agent.conversation_id, None)
            .await
            .unwrap();
        assert_eq!(
            persisted.len(),
            1,
            "503 back-pressure leaked a persisted message"
        );
        assert_eq!(persisted[0].content, json!("a"));
    }

    #[tokio::test]
    async fn send_message_buffers_in_message_queue_when_disconnected() {
        // No live tunnel installed, but the agent_id is a valid UUID
        // (the new buffer-on-disconnect path requires this), so the
        // notification should land in the per-agent message queue
        // instead of being silently dropped.
        let state = test_state();
        let agent_uuid = Uuid::new_v4();
        let agent = seed_agent_with_id(&state, agent_uuid.to_string()).await;

        let (status, _) = send_message(
            State(state.clone()),
            Path(agent.id.clone()),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!({ "text": "while you were out" }),
            }),
        )
        .await
        .expect("send_message");
        assert_eq!(status, StatusCode::CREATED);

        // The notification is buffered keyed by the parsed UUID.
        assert_eq!(state.message_queue.len(agent_uuid), 1);
        let drained = state.message_queue.drain(agent_uuid);
        let parsed: Value = serde_json::from_str(&drained[0]).unwrap();
        assert_eq!(parsed["method"], "session/message");
        assert_eq!(parsed["params"]["content"]["text"], "while you were out");
    }

    #[tokio::test]
    async fn unknown_agent_is_404() {
        let state = test_state();
        let err = list_messages(
            State(state),
            Path("ag_missing".into()),
            Query(MessagesQuery { since: None }),
        )
        .await
        .expect_err("unknown agent");
        assert!(matches!(err, ApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn stream_handler_resolves_agent() {
        // The SSE stream itself is exercised via the broker unit
        // tests; here we just confirm the handler accepts a known
        // agent and rejects an unknown one.
        let state = test_state();
        let agent = seed_agent(&state).await;
        let _ = stream_events(State(state.clone()), Path(agent.id))
            .await
            .expect("stream_events");

        let err = stream_events(State(state), Path("ag_missing".into()))
            .await
            .expect_err("unknown agent");
        assert!(matches!(err, ApiError::NotFound(_)));
    }
}
