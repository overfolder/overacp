//! axum routes for `/agents/{id}/...` — the REST adapters over the
//! JSON-RPC tunnel from `docs/design/controlplane.md` § 3.5.
//!
//! These sit at the root (no `/api/v1` prefix) and key off the
//! `agent_id` recorded in the agents table. Each handler resolves
//! the agent to its conversation id, which is also the tunnel
//! session id, and dispatches through:
//!
//! - the `SessionStore` (messages table) for writes/reads,
//! - the `SessionManager` (live tunnels) to poke the agent with a
//!   `session/message` or cancel notification, and
//! - the `StreamBroker` (in-memory fan-out) for SSE.
//!
//! Agent creation (§ 3.4) is a separate concern; these handlers
//! only require that `store.get_agent(id)` returns a record.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::stream::{self, Stream};
use overacp_compute_core::{NodeId, NodeSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::state::AppState;
use crate::store::{Agent, AgentStatus, ComputeNode, ComputePool, Message, NodeStatus, PoolStatus};

/// Mount the `/agents/{id}/...` § 3.5 routes plus `POST /agents`
/// (§ 3.4.3 — agent creation).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents", post(create_agent))
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

/// `POST /agents` request body — see `docs/design/controlplane.md`
/// § 3.4.1. The `user` field is *not* in the request: control-plane
/// auth is HTTP Basic, so the user is taken from
/// `AppState::default_user_id`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAgentRequest {
    pub pool: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// § 3.4.2 describe shape — pinned by the
/// `create_agent_returns_describe_shape` test.
#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub id: String,
    pub user: String,
    pub conversation_id: Uuid,
    pub compute: ComputeRef,
    pub image: String,
    pub status: AgentStatus,
    pub created_at: DateTime<Utc>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComputeRef {
    pub provider_type: String,
    pub pool: String,
    pub node_id: String,
}

/// 30 days, per `docs/design/protocol.md` § 2.4.
const AGENT_JWT_TTL_DAYS: i64 = 30;

// ── handlers ────────────────────────────────────────────────────

async fn require_agent(state: &AppState, id: &str) -> Result<Agent, ApiError> {
    state
        .store
        .get_agent(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{id}'")))
}

/// `POST /agents` — provision (or reuse) a node in the named pool,
/// mint a fresh agent JWT, persist the agent + bump the node's
/// `agent_refcount` atomically, and return the § 3.4.2 describe
/// shape. Decision tree per `docs/design/controlplane.md` § 3.4.3.
async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<AgentView>), ApiError> {
    if req.pool.is_empty() {
        return Err(ApiError::BadRequest("'pool' must not be empty".into()));
    }

    // 1. Resolve the user from control-plane defaults. HTTP Basic
    //    carries no user identity, so this is the only source.
    let user = state.default_user_id.ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "OVERACP_DEFAULT_USER_ID is not set; cannot attribute control-plane writes".into(),
        )
    })?;

    // 2. Tunnel base URL is required to populate OVERACP_TUNNEL_URL.
    let tunnel_base = state.tunnel_base_url.clone().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "OVERACP_TUNNEL_BASE_URL is not set; cannot mint OVERACP_TUNNEL_URL".into(),
        )
    })?;

    // 3. Pool must exist and be active.
    let pool = state
        .store
        .get_pool(&req.pool)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pool '{}'", req.pool)))?;
    if !matches!(pool.status, PoolStatus::Active) {
        return Err(ApiError::Conflict(format!(
            "pool '{}' is {:?}, not active",
            pool.name, pool.status
        )));
    }

    // 4. Capability flags. Accept JSON bool and string forms to
    //    match the existing tolerance in store/memory.rs.
    let multi_agent_nodes = read_bool_flag(&pool, "multi_agent_nodes");
    let node_reuse = read_bool_flag(&pool, "node_reuse");

    // 5. Live runtime for the pool.
    let provider = state.pool_runtime(&req.pool).ok_or_else(|| {
        ApiError::ServiceUnavailable(format!(
            "pool '{}' has no live compute runtime registered",
            req.pool
        ))
    })?;

    // 6. Mint identifiers and the conversation row up front so we
    //    can populate the agent JWT and the spawned-process env.
    let agent_uuid = Uuid::new_v4();
    let agent_id = format!("ag_{}", agent_uuid.simple());
    let conv = state.store.create_conversation(&user.to_string()).await?;
    let exp = (Utc::now() + ChronoDuration::days(AGENT_JWT_TTL_DAYS)).timestamp();
    let jwt = state
        .authenticator
        .mint_agent_token(agent_uuid, user, conv.id, exp)
        .map_err(|e| ApiError::ServiceUnavailable(format!("mint agent jwt: {e}")))?;

    let image = req
        .image
        .clone()
        .unwrap_or_else(|| "overacp/loop:latest".to_string());

    // 7. Optimistic pre-check: if a reuse policy applies and a
    //    candidate already exists, skip the provider call entirely.
    //    Otherwise pre-provision a fresh node *outside* the store
    //    write lock (the acquire factory closure is sync, so the
    //    async provider call cannot run inside it). The narrow race
    //    window where another acquire reuses our pre-provisioned
    //    node is handled by the leak-cleanup branch below.
    let existing = state.store.list_nodes(&req.pool).await?;
    let candidate_exists = pick_candidate(&existing, multi_agent_nodes, node_reuse).is_some();

    let preprovisioned: Option<ComputeNode> = if candidate_exists {
        None
    } else {
        let mut env = BTreeMap::<String, String>::new();
        env.insert(
            "OVERACP_TUNNEL_URL".into(),
            format!("{}/tunnel/{}", tunnel_base.trim_end_matches('/'), conv.id),
        );
        env.insert("OVERACP_JWT".into(), jwt.clone());
        env.insert("OVERACP_AGENT_ID".into(), agent_id.clone());

        let spec = NodeSpec {
            image: image.clone(),
            cpu: None,
            memory_gb: None,
            disk_gb: None,
            env,
            jwt: jwt.clone(),
            provider_overrides: Default::default(),
        };
        let handle = provider.create_node(spec).await?;
        Some(ComputeNode {
            node_id: handle.id.0.clone(),
            pool_name: req.pool.clone(),
            status: NodeStatus::Running,
            provider_metadata: handle.provider_metadata,
            created_at: Utc::now(),
            deleted_at: None,
            agent_refcount: 0,
        })
    };

    // 8. Build the agent row. `node_id` is overwritten by the store.
    let agent = Agent {
        id: agent_id.clone(),
        user: user.to_string(),
        conversation_id: conv.id,
        pool_name: req.pool.clone(),
        node_id: String::new(),
        image: image.clone(),
        status: AgentStatus::Idle,
        metadata: req.metadata.clone().unwrap_or(json!({})),
        created_at: Utc::now(),
    };

    // 9. Atomic acquire. The picker enforces the policy; the
    //    factory returns the pre-provisioned node when one exists.
    //    Both closures must be sync per the trait signature.
    let pool_name = req.pool.clone();
    let preprovisioned_for_factory = preprovisioned.clone();
    let acquire_result = state
        .store
        .acquire_node_for_agent(
            &pool_name,
            agent,
            &|nodes: &[ComputeNode]| pick_candidate(nodes, multi_agent_nodes, node_reuse),
            &|| {
                preprovisioned_for_factory
                    .clone()
                    .expect("factory invoked without a pre-provisioned node")
            },
        )
        .await;

    let outcome = match acquire_result {
        Ok(o) => o,
        Err(e) => {
            // Acquire failed after we already created a provider
            // node — best-effort cleanup so we don't leak the VM.
            if let Some(node) = &preprovisioned {
                if let Err(cleanup_err) = provider.delete_node(&NodeId(node.node_id.clone())).await
                {
                    tracing::warn!(
                        node_id = %node.node_id,
                        error = %cleanup_err,
                        "leaked compute node after acquire failure",
                    );
                }
            }
            return Err(e.into());
        }
    };

    // 10. Race cleanup: we pre-provisioned but acquire ended up
    //     reusing an existing node. Best-effort delete the orphan.
    if let Some(node) = &preprovisioned {
        if !outcome.created {
            if let Err(cleanup_err) = provider.delete_node(&NodeId(node.node_id.clone())).await {
                tracing::warn!(
                    node_id = %node.node_id,
                    error = %cleanup_err,
                    "leaked compute node after reuse race",
                );
            }
        }
    }

    let view = AgentView {
        id: agent_id,
        user: user.to_string(),
        conversation_id: conv.id,
        compute: ComputeRef {
            provider_type: pool.provider_type.clone(),
            pool: req.pool,
            node_id: outcome.node.node_id,
        },
        image,
        status: AgentStatus::Idle,
        created_at: Utc::now(),
        metadata: req.metadata.unwrap_or(json!({})),
    };
    Ok((StatusCode::CREATED, Json(view)))
}

/// Read a boolean capability flag from a pool's `config_json`.
/// Tolerates both JSON bool and string `"true"`/`"false"` forms.
fn read_bool_flag(pool: &ComputePool, key: &str) -> bool {
    pool.config_json
        .get(key)
        .map(|v| match v {
            Value::Bool(b) => *b,
            Value::String(s) => s.eq_ignore_ascii_case("true"),
            _ => false,
        })
        .unwrap_or(false)
}

/// Picker for `acquire_node_for_agent`. Encodes the § 3.4.3 policy:
/// - `multi_agent_nodes`: any healthy (Running) node.
/// - `node_reuse` only: any healthy node with `agent_refcount == 0`.
/// - neither: never reuse.
fn pick_candidate(
    nodes: &[ComputeNode],
    multi_agent_nodes: bool,
    node_reuse: bool,
) -> Option<String> {
    if multi_agent_nodes {
        return nodes
            .iter()
            .find(|n| matches!(n.status, NodeStatus::Running))
            .map(|n| n.node_id.clone());
    }
    if node_reuse {
        return nodes
            .iter()
            .find(|n| matches!(n.status, NodeStatus::Running) && n.agent_refcount == 0)
            .map(|n| n.node_id.clone());
    }
    None
}

/// `POST /agents/{id}/messages` — write a user message into the
/// conversation table and poke the agent's tunnel with
/// `session/message`. The agent fetches the body via
/// `poll/newMessages` exactly as in protocol.md § 3.1.
async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageResponse>), ApiError> {
    if req.role.is_empty() {
        return Err(ApiError::BadRequest("'role' must not be empty".into()));
    }
    let agent = require_agent(&state, &id).await?;
    let message = state
        .store
        .append_message(agent.conversation_id, &req.role, req.content)
        .await?;

    // Best-effort notify: if the tunnel is currently connected we
    // emit `session/message`; if it isn't the agent will pick the
    // message up on its next `poll/newMessages` after reconnect.
    if let Some(handle) = state.sessions.get(&agent.conversation_id) {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "session/message",
            "params": { "id": message.id },
        });
        let _ = handle.tx.send(notif.to_string());
    }

    Ok((StatusCode::CREATED, Json(SendMessageResponse { message })))
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
    use overacp_compute_core::config::ResolvedConfig;
    use overacp_compute_core::{
        ComputeProvider, ConfigError, ExecRequest, ExecResult, LogStream, NodeDescription,
        NodeHandle, ProviderError,
    };
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
        let conv = state
            .store
            .create_conversation("user-1")
            .await
            .expect("create_conversation");
        let agent = Agent {
            id: "ag_test".into(),
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

    fn create_state_with_runtime(provider: Arc<dyn ComputeProvider>) -> AppState {
        let state = AppState::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test", "overacp")),
        )
        .with_default_user_id(Uuid::new_v4())
        .with_tunnel_base_url("wss://test.local");
        state.register_pool_runtime("pool-a", provider);
        state
    }

    /// In-test ComputeProvider that records every `create_node`
    /// call (so we can assert the protocol § 2.4 env contract) and
    /// hands back deterministic node ids.
    struct RecordingProvider {
        inner: Mutex<RecordingState>,
    }

    #[derive(Default)]
    struct RecordingState {
        next_id: u32,
        created: Vec<NodeSpec>,
        deleted: Vec<String>,
    }

    impl RecordingProvider {
        fn new() -> Self {
            Self {
                inner: Mutex::new(RecordingState::default()),
            }
        }
        async fn snapshot(&self) -> RecordingState {
            let g = self.inner.lock().await;
            RecordingState {
                next_id: g.next_id,
                created: g.created.clone(),
                deleted: g.deleted.clone(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ComputeProvider for RecordingProvider {
        fn provider_type() -> &'static str
        where
            Self: Sized,
        {
            "fake"
        }
        fn from_config(_: ResolvedConfig) -> Result<Self, ProviderError>
        where
            Self: Sized,
        {
            Ok(Self::new())
        }
        async fn create_node(&self, spec: NodeSpec) -> Result<NodeHandle, ProviderError> {
            let mut g = self.inner.lock().await;
            g.next_id += 1;
            let id = format!("node-{}", g.next_id);
            g.created.push(spec);
            Ok(NodeHandle {
                id: NodeId(id),
                provider_metadata: json!({}),
            })
        }
        async fn list_nodes(&self) -> Result<Vec<NodeHandle>, ProviderError> {
            Ok(vec![])
        }
        async fn describe_node(&self, id: &NodeId) -> Result<NodeDescription, ProviderError> {
            Err(ProviderError::NotFound(id.clone()))
        }
        async fn delete_node(&self, id: &NodeId) -> Result<(), ProviderError> {
            self.inner.lock().await.deleted.push(id.0.clone());
            Ok(())
        }
        async fn exec(&self, _: &NodeId, _: ExecRequest) -> Result<ExecResult, ProviderError> {
            unimplemented!()
        }
        async fn stream_logs(&self, _: &NodeId) -> Result<LogStream, ProviderError> {
            unimplemented!()
        }
        fn validate_config(_: &serde_json::Map<String, Value>) -> Result<(), ConfigError>
        where
            Self: Sized,
        {
            Ok(())
        }
    }

    async fn seed_pool(state: &AppState, name: &str, config: Value) {
        state
            .store
            .create_pool(ComputePool {
                name: name.into(),
                provider_type: "fake".into(),
                config_json: config,
                status: PoolStatus::Active,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .await
            .expect("create_pool");
    }

    fn install_fake_tunnel(state: &AppState, conv: Uuid) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        state.sessions.insert(
            conv,
            TunnelHandle {
                tx,
                claims: Claims {
                    sub: Uuid::new_v4(),
                    user: Uuid::new_v4(),
                    conv,
                    exp: 0,
                    iss: "test".into(),
                },
                last_activity: Instant::now(),
                poll_cursor: Mutex::new(None),
            },
        );
        rx
    }

    #[tokio::test]
    async fn create_agent_provisions_fresh_node() {
        let provider = Arc::new(RecordingProvider::new());
        let state = create_state_with_runtime(provider.clone() as _);
        seed_pool(&state, "pool-a", json!({})).await;

        let (status, Json(view)) = create_agent(
            State(state.clone()),
            Json(CreateAgentRequest {
                pool: "pool-a".into(),
                image: Some("img:1".into()),
                metadata: Some(json!({"k":"v"})),
            }),
        )
        .await
        .expect("create_agent");

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(view.compute.pool, "pool-a");
        assert_eq!(view.compute.provider_type, "fake");
        assert_eq!(view.compute.node_id, "node-1");
        assert_eq!(view.image, "img:1");
        assert_eq!(view.metadata, json!({"k":"v"}));
        assert!(view.id.starts_with("ag_"));

        // Provider was called once with the protocol § 2.4 env.
        let snap = provider.snapshot().await;
        assert_eq!(snap.created.len(), 1);
        let spec = &snap.created[0];
        assert_eq!(spec.image, "img:1");
        assert!(!spec.jwt.is_empty());
        let url = spec.env.get("OVERACP_TUNNEL_URL").expect("tunnel url");
        assert!(url.starts_with("wss://test.local/tunnel/"));
        assert_eq!(spec.env.get("OVERACP_JWT").unwrap(), &spec.jwt);
        assert_eq!(spec.env.get("OVERACP_AGENT_ID").unwrap(), &view.id);

        // Store has 1 agent + 1 node with refcount 1.
        let nodes = state.store.list_nodes("pool-a").await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].agent_refcount, 1);
        let agents = state.store.list_agents(None).await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].node_id, "node-1");
        assert_eq!(agents[0].conversation_id, view.conversation_id);
    }

    #[tokio::test]
    async fn create_agent_reuses_node_when_node_reuse_set() {
        let provider = Arc::new(RecordingProvider::new());
        let state = create_state_with_runtime(provider.clone() as _);
        seed_pool(&state, "pool-a", json!({"node_reuse": "true"})).await;

        // First call provisions a fresh node (no candidates yet).
        let (_, Json(first)) = create_agent(
            State(state.clone()),
            Json(CreateAgentRequest {
                pool: "pool-a".into(),
                image: None,
                metadata: None,
            }),
        )
        .await
        .expect("first create");

        // Drop the agent so the existing node falls back to refcount 0.
        let release = state
            .store
            .release_node_for_agent(&first.id)
            .await
            .expect("release");
        assert_eq!(release.new_refcount, 0);
        // node_reuse=true keeps the node alive (no destroy).
        assert!(!release.should_destroy);

        // Second call should reuse the same node — no new provider call.
        let (_, Json(second)) = create_agent(
            State(state.clone()),
            Json(CreateAgentRequest {
                pool: "pool-a".into(),
                image: None,
                metadata: None,
            }),
        )
        .await
        .expect("second create");

        assert_eq!(second.compute.node_id, first.compute.node_id);
        let snap = provider.snapshot().await;
        assert_eq!(snap.created.len(), 1, "no second create_node call");
    }

    #[tokio::test]
    async fn create_agent_attaches_to_existing_with_multi_agent_nodes() {
        let provider = Arc::new(RecordingProvider::new());
        let state = create_state_with_runtime(provider.clone() as _);
        seed_pool(&state, "pool-a", json!({"multi_agent_nodes": true})).await;

        let (_, Json(first)) = create_agent(
            State(state.clone()),
            Json(CreateAgentRequest {
                pool: "pool-a".into(),
                image: None,
                metadata: None,
            }),
        )
        .await
        .expect("first");
        let (_, Json(second)) = create_agent(
            State(state.clone()),
            Json(CreateAgentRequest {
                pool: "pool-a".into(),
                image: None,
                metadata: None,
            }),
        )
        .await
        .expect("second");

        assert_eq!(first.compute.node_id, second.compute.node_id);
        let nodes = state.store.list_nodes("pool-a").await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].agent_refcount, 2);
        assert_eq!(provider.snapshot().await.created.len(), 1);
    }

    #[tokio::test]
    async fn create_agent_404_for_unknown_pool() {
        let provider = Arc::new(RecordingProvider::new());
        let state = create_state_with_runtime(provider as _);
        let err = create_agent(
            State(state),
            Json(CreateAgentRequest {
                pool: "nope".into(),
                image: None,
                metadata: None,
            }),
        )
        .await
        .expect_err("missing pool");
        assert!(matches!(err, ApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn create_agent_409_for_paused_pool() {
        let provider = Arc::new(RecordingProvider::new());
        let state = create_state_with_runtime(provider as _);
        state
            .store
            .create_pool(ComputePool {
                name: "pool-a".into(),
                provider_type: "fake".into(),
                config_json: json!({}),
                status: PoolStatus::Paused,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .await
            .unwrap();
        let err = create_agent(
            State(state),
            Json(CreateAgentRequest {
                pool: "pool-a".into(),
                image: None,
                metadata: None,
            }),
        )
        .await
        .expect_err("paused pool");
        assert!(matches!(err, ApiError::Conflict(_)));
    }

    #[tokio::test]
    async fn create_agent_503_when_tunnel_base_url_unset() {
        let provider = Arc::new(RecordingProvider::new());
        let state = AppState::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test", "overacp")),
        )
        .with_default_user_id(Uuid::new_v4());
        state.register_pool_runtime("pool-a", provider as _);
        seed_pool(&state, "pool-a", json!({})).await;

        let err = create_agent(
            State(state),
            Json(CreateAgentRequest {
                pool: "pool-a".into(),
                image: None,
                metadata: None,
            }),
        )
        .await
        .expect_err("missing tunnel base");
        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
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
