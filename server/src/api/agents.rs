//! axum routes for `/agents/{id}/...` — the REST surface the
//! operator backend uses to drive connected agents.
//!
//! The full endpoint map, per `SPEC.md` § "REST surface":
//!
//! ```text
//! POST   /agents/{id}/messages    push a session/message (buffered if disconnected)
//! GET    /agents/{id}/stream      SSE feed of stream/* and turn/end
//! POST   /agents/{id}/cancel      inject a session/cancel notification
//! GET    /agents/{id}             describe — connection state, last activity, claims
//! GET    /agents                  list connected + recently disconnected agents
//! DELETE /agents/{id}             force-disconnect the tunnel
//! ```
//!
//! Every `{id}` is the agent UUID — the same value that appears as
//! `sub` in the agent's JWT and in the `/tunnel/:agent_id` path.
//!
//! These handlers are split into two sub-routers with different
//! authorization rules, both wired up in [`crate::routes`]:
//!
//! - [`admin_router`] — `GET /agents`, `GET /agents/{id}`,
//!   `DELETE /agents/{id}`. Admin JWTs only; agent JWTs are
//!   rejected at the middleware layer even when `sub == id`.
//! - [`agent_scoped_router`] — `POST /agents/{id}/messages`,
//!   `GET /agents/{id}/stream`, `POST /agents/{id}/cancel`. Admin
//!   JWTs or an agent JWT whose `sub` matches the path `{id}`.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, State};
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
use crate::registry::{AgentDescription, QueueError};
use crate::state::AppState;

/// Admin-only routes: list/describe/force-disconnect agents. Per
/// SPEC.md § "Route authorization", agent JWTs cannot reach these
/// even if they'd be scoped to the `{id}` path segment — they are
/// registry/operator concerns.
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/agents", get(list_agents))
        .route("/agents/:id", get(describe_agent).delete(disconnect_agent))
}

/// Agent-scoped routes: admin JWTs work on any agent, agent JWTs
/// work only on their own `sub`. These are the routes an operator's
/// web frontend typically holds an agent JWT for.
pub fn agent_scoped_router() -> Router<AppState> {
    Router::new()
        .route("/agents/:id/messages", post(send_message))
        .route("/agents/:id/stream", get(stream_events))
        .route("/agents/:id/cancel", post(cancel_turn))
}

// ── wire types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageRequest {
    /// Role for the message body. Defaults to `"user"`.
    #[serde(default = "default_role")]
    pub role: String,
    /// Opaque message content, shipped verbatim in the
    /// `session/message` notification.
    pub content: Value,
}

fn default_role() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct SendMessageResponse {
    /// How the push was delivered.
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Sent inline to a connected tunnel.
    Live,
    /// Buffered in the per-agent `MessageQueue` for the next
    /// reconnect.
    Queued,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentListResponse {
    pub agents: Vec<AgentDescription>,
}

// ── handlers ────────────────────────────────────────────────────

/// `GET /agents` — list currently-connected + recently-disconnected
/// agents. Admin-only (enforced by the `require_admin` middleware
/// wired up in [`crate::routes`]).
async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<AgentListResponse>, ApiError> {
    Ok(Json(AgentListResponse {
        agents: state.registry.list(),
    }))
}

/// `GET /agents/{id}` — describe one agent.
async fn describe_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AgentDescription>, ApiError> {
    state
        .registry
        .describe(id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))
}

/// `DELETE /agents/{id}` — force-disconnect the tunnel. Dropping
/// the entry from the registry closes the mpsc sender the write
/// loop is reading from, which causes `run_tunnel` to exit on its
/// next iteration.
async fn disconnect_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if !state.registry.is_connected(id) {
        return Err(ApiError::NotFound(format!("agent {id}")));
    }
    state.registry.unregister(id);
    Ok(StatusCode::ACCEPTED)
}

/// `POST /agents/{id}/messages` — push a `session/message`
/// notification with the body inline, per protocol.md § 3.1.
///
/// If the agent's tunnel is connected the notification is delivered
/// inline and the response reports `delivery: "live"`. Otherwise it
/// is enqueued in the per-agent `MessageQueue` and delivered on the
/// next reconnect (`delivery: "queued"`). If the queue is at
/// capacity the handler returns 503.
async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageResponse>), ApiError> {
    if req.role.is_empty() {
        return Err(ApiError::BadRequest("'role' must not be empty".into()));
    }

    let notif = json!({
        "jsonrpc": "2.0",
        "method": "session/message",
        "params": {
            "role": req.role,
            "content": req.content,
        },
    })
    .to_string();

    let delivery = if let Some(entry) = state.registry.get(id) {
        // Live tunnel — deliver inline. If the channel has been
        // closed between our lookup and the send (a disconnect
        // race), `SendError` hands us the original frame back, so
        // we fall through to the queue without rebuilding it. We
        // deliberately do NOT call `registry.unregister(id)` here:
        // `run_tunnel`'s own cleanup owns that, and reaching in
        // from the REST handler risks racing a fresh reconnect
        // that slotted in at the same `agent_id` between our
        // failed send and the cleanup call.
        match entry.tx.send(notif) {
            Ok(()) => Delivery::Live,
            Err(send_err) => {
                let recovered = send_err.0;
                match state.message_queue.push(id, recovered) {
                    Ok(()) => Delivery::Queued,
                    Err(QueueError::Full { capacity, .. }) => {
                        return Err(queue_full(id, capacity));
                    }
                }
            }
        }
    } else {
        // No live tunnel — buffer for the next reconnect.
        match state.message_queue.push(id, notif) {
            Ok(()) => Delivery::Queued,
            Err(QueueError::Full { capacity, .. }) => {
                return Err(queue_full(id, capacity));
            }
        }
    };

    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageResponse { delivery }),
    ))
}

fn queue_full(agent_id: Uuid, capacity: usize) -> ApiError {
    ApiError::ServiceUnavailable(format!(
        "agent {agent_id} message queue is full ({capacity}); retry later"
    ))
}

/// `GET /agents/{id}/stream` — SSE fan-out of the agent's
/// `stream/*` and `turn/end` notifications from the in-memory
/// broker. Returns even if the agent is currently disconnected;
/// the subscriber simply waits until the tunnel reconnects and
/// starts producing frames.
async fn stream_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let rx = state.stream_broker.subscribe(id);
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
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if let Some(entry) = state.registry.get(id) {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {},
        });
        let _ = entry.tx.send(notif.to_string());
    }
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::api::default_registry;
    use crate::auth::{Claims, StaticJwtAuthenticator};
    use crate::registry::{AgentEntry, MessageQueue};
    use crate::state::AppState;
    use crate::store::InMemoryStore;
    use tokio::sync::mpsc;

    fn fresh_state() -> AppState {
        AppState::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test", "overacp")),
        )
    }

    fn state_with_queue_capacity(cap: usize) -> AppState {
        let base = fresh_state();
        AppState {
            message_queue: MessageQueue::new(cap),
            ..base
        }
    }

    /// Register a fake agent in the new registry and return the
    /// receiving side of its tunnel channel.
    fn register_fake(state: &AppState, agent_id: Uuid) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let claims = Claims::agent(agent_id, Some(Uuid::new_v4()), 60, "test");
        state.registry.register(agent_id, AgentEntry::new(tx, claims));
        rx
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    // ── POST /agents/{id}/messages ──

    #[tokio::test]
    async fn send_message_delivers_inline_when_connected() {
        let state = fresh_state();
        let agent_id = Uuid::new_v4();
        let mut rx = register_fake(&state, agent_id);

        let (status, Json(resp)) = send_message(
            State(state),
            Path(agent_id),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!({ "text": "hello" }),
            }),
        )
        .await
        .expect("send_message");

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(resp.delivery, Delivery::Live);

        let frame = rx.recv().await.expect("tunnel frame");
        let parsed = parse(&frame);
        assert_eq!(parsed["method"], "session/message");
        assert_eq!(parsed["params"]["role"], "user");
        assert_eq!(parsed["params"]["content"]["text"], "hello");
    }

    #[tokio::test]
    async fn send_message_buffers_when_disconnected() {
        let state = fresh_state();
        let agent_id = Uuid::new_v4();

        let (status, Json(resp)) = send_message(
            State(state.clone()),
            Path(agent_id),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!("pending"),
            }),
        )
        .await
        .expect("send_message");

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(resp.delivery, Delivery::Queued);
        assert_eq!(state.message_queue.len(agent_id), 1);

        let drained = state.message_queue.drain(agent_id);
        assert_eq!(parse(&drained[0])["params"]["content"], "pending");
    }

    #[tokio::test]
    async fn send_message_falls_back_to_queue_when_receiver_dropped() {
        // Reconnect race: the registry still has a live `AgentEntry`
        // (because `run_tunnel` hasn't finished its teardown) but
        // the receiving side of the channel has already been
        // dropped. The handler must recover the frame from the
        // `SendError`, push it onto the `MessageQueue`, and report
        // `delivery: "queued"`. Importantly, it must NOT unregister
        // the entry itself — that's `run_tunnel`'s responsibility
        // and reaching in would race a fresh reconnect at the
        // same agent_id.
        let state = fresh_state();
        let agent_id = Uuid::new_v4();
        let rx = register_fake(&state, agent_id);
        drop(rx); // close the receiver without unregistering

        let (status, Json(resp)) = send_message(
            State(state.clone()),
            Path(agent_id),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!("reconnect-race"),
            }),
        )
        .await
        .expect("send_message");

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(resp.delivery, Delivery::Queued);

        // The frame landed in the queue (not silently dropped).
        assert_eq!(state.message_queue.len(agent_id), 1);

        // The registry entry is still present — the handler did
        // not reach in and unregister it.
        assert!(state.registry.is_connected(agent_id));
    }

    #[tokio::test]
    async fn send_message_empty_role_is_400() {
        let state = fresh_state();
        let err = send_message(
            State(state),
            Path(Uuid::new_v4()),
            Json(SendMessageRequest {
                role: "".into(),
                content: json!("x"),
            }),
        )
        .await
        .expect_err("empty role");
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[tokio::test]
    async fn send_message_returns_503_when_queue_full() {
        let state = state_with_queue_capacity(1);
        let agent_id = Uuid::new_v4();

        // First push buffers.
        let _ = send_message(
            State(state.clone()),
            Path(agent_id),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!("a"),
            }),
        )
        .await
        .expect("first");

        // Second overflows.
        let err = send_message(
            State(state.clone()),
            Path(agent_id),
            Json(SendMessageRequest {
                role: "user".into(),
                content: json!("b"),
            }),
        )
        .await
        .expect_err("full");
        assert!(matches!(err, ApiError::ServiceUnavailable(_)));

        // And we still have exactly one frame buffered — no ghost
        // state from the rejected push.
        assert_eq!(state.message_queue.len(agent_id), 1);
    }

    // ── GET /agents, GET /agents/{id}, DELETE /agents/{id} ──

    #[tokio::test]
    async fn list_agents_is_empty_when_nothing_connected() {
        let state = fresh_state();
        let Json(resp) = list_agents(State(state)).await.unwrap();
        assert!(resp.agents.is_empty());
    }

    #[tokio::test]
    async fn list_agents_surfaces_connected_and_recent() {
        let state = fresh_state();
        let live = Uuid::new_v4();
        let dead = Uuid::new_v4();
        let _live_rx = register_fake(&state, live);
        let _dead_rx = register_fake(&state, dead);
        state.registry.unregister(dead);

        let Json(resp) = list_agents(State(state)).await.unwrap();
        assert_eq!(resp.agents.len(), 2);
        assert!(resp.agents.iter().any(|d| d.agent_id == live && d.connected));
        assert!(resp.agents.iter().any(|d| d.agent_id == dead && !d.connected));
    }

    #[tokio::test]
    async fn describe_agent_returns_connected_shape() {
        let state = fresh_state();
        let id = Uuid::new_v4();
        let _rx = register_fake(&state, id);

        let Json(desc) = describe_agent(State(state), Path(id)).await.unwrap();
        assert_eq!(desc.agent_id, id);
        assert!(desc.connected);
        assert!(desc.uptime_secs.is_some());
    }

    #[tokio::test]
    async fn describe_unknown_agent_is_404() {
        let state = fresh_state();
        let err = describe_agent(State(state), Path(Uuid::new_v4()))
            .await
            .expect_err("unknown");
        assert!(matches!(err, ApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn disconnect_unregisters_and_returns_202() {
        let state = fresh_state();
        let id = Uuid::new_v4();
        let _rx = register_fake(&state, id);

        let status = disconnect_agent(State(state.clone()), Path(id))
            .await
            .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(!state.registry.is_connected(id));
    }

    #[tokio::test]
    async fn disconnect_unknown_agent_is_404() {
        let state = fresh_state();
        let err = disconnect_agent(State(state), Path(Uuid::new_v4()))
            .await
            .expect_err("unknown");
        assert!(matches!(err, ApiError::NotFound(_)));
    }

    // ── POST /agents/{id}/cancel ──

    #[tokio::test]
    async fn cancel_emits_notification_when_connected() {
        let state = fresh_state();
        let id = Uuid::new_v4();
        let mut rx = register_fake(&state, id);

        let status = cancel_turn(State(state), Path(id)).await.unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);

        let frame = rx.recv().await.expect("frame");
        assert_eq!(parse(&frame)["method"], "session/cancel");
    }

    #[tokio::test]
    async fn cancel_without_tunnel_still_202() {
        let state = fresh_state();
        let status = cancel_turn(State(state), Path(Uuid::new_v4()))
            .await
            .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    // ── GET /agents/{id}/stream ──

    #[tokio::test]
    async fn stream_handler_always_accepts() {
        // The SSE fan-out is keyed on agent_id and does not require
        // the agent to be currently connected. The subscriber just
        // waits until frames start flowing.
        let state = fresh_state();
        let _sse = stream_events(State(state), Path(Uuid::new_v4()))
            .await
            .expect("sse handle");
    }
}
