//! axum routes for `/agents` — controlplane § 3.4.
//!
//! Agents are the user-facing concept: a conversation pinned to a
//! single compute node on a single pool. `POST /agents` provisions a
//! node via the named pool's `ComputeProvider`, mints a JWT for a
//! fresh conversation, persists the agent record, and returns both.
//! `GET / DELETE / status` round-trip the record. The compute block
//! on every describe response carries `(provider_type, pool, node_id)`
//! so operators can jump from a misbehaving agent straight to its
//! underlying node via `/compute/pools/{pool}/nodes/{node_id}`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{Duration, Utc};
use overacp_compute_core::{ComputeProvider, NodeId, NodeSpec, RawConfig, ResolvedConfig};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::api::dto::{
    AgentStatusResponse, AgentView, ComputeRef, CreateAgentRequest, CreateAgentResponse,
    ListAgentsQuery,
};
use crate::api::error::ApiError;
use crate::api::pool_config::PoolConfig;
use crate::auth::Claims;
use crate::state::AppState;
use crate::store::{Agent, AgentStatus, ComputeNode, ComputePool, NodeStatus, PoolStatus};

const AGENT_JWT_TTL_HOURS: i64 = 24;

/// Mount the agent REST routes. Designed to be `.merge`d into the
/// top-level router alongside the providers/pools/nodes surfaces.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/:id", get(get_agent).delete(delete_agent))
        .route("/agents/:id/status", get(get_agent_status))
}

// ── handlers ─────────────────────────────────────────────────────

async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<CreateAgentResponse>), ApiError> {
    if req.pool.is_empty() {
        return Err(ApiError::BadRequest("'pool' must not be empty".into()));
    }

    let pool = require_pool(&state, &req.pool).await?;
    if pool.status != PoolStatus::Active {
        return Err(ApiError::BadRequest(format!(
            "pool '{}' is not active (status = {:?})",
            pool.name, pool.status
        )));
    }
    let provider_type = pool.provider_type.clone();
    let (provider, resolved) = provider_for_pool(&state, &pool).await?;

    let image = match req.image.as_deref() {
        Some(i) if !i.is_empty() => i.to_string(),
        _ => resolved
            .get("default.image")
            .map(str::to_owned)
            .ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "no 'image' in request and pool '{}' has no 'default.image'",
                    pool.name
                ))
            })?,
    };

    let conversation = state
        .store
        .create_conversation(&req.user.to_string())
        .await?;

    let agent_id = format!("ag_{}", Uuid::new_v4().simple());

    let claims = Claims {
        sub: Uuid::new_v4(),
        user: req.user,
        conv: conversation.id,
        exp: (Utc::now() + Duration::hours(AGENT_JWT_TTL_HOURS)).timestamp(),
        iss: state.authenticator.issuer().to_string(),
    };
    let jwt = state.authenticator.issue(&claims)?;

    let spec = NodeSpec {
        image: image.clone(),
        cpu: parse_opt_u32(resolved.get("default.cpu")),
        memory_gb: parse_opt_u32(resolved.get("default.memory_gb")),
        disk_gb: parse_opt_u32(resolved.get("default.disk_gb")),
        env: BTreeMap::new(),
        jwt: jwt.clone(),
        provider_overrides: Map::new(),
    };

    let handle = provider.create_node(spec).await?;

    if let Err(e) = state
        .store
        .upsert_node(ComputeNode {
            node_id: handle.id.0.clone(),
            pool_name: pool.name.clone(),
            status: NodeStatus::Running,
            provider_metadata: handle.provider_metadata.clone(),
            created_at: Utc::now(),
            deleted_at: None,
        })
        .await
    {
        // Roll back the live node so a failed metadata insert
        // doesn't leak compute. Mirrors the rollback for the
        // `create_agent` insert below.
        let _ = provider.delete_node(&handle.id).await;
        return Err(e.into());
    }

    let agent = Agent {
        id: agent_id.clone(),
        user: req.user.to_string(),
        conversation_id: conversation.id,
        pool_name: pool.name.clone(),
        node_id: handle.id.0.clone(),
        image: image.clone(),
        status: AgentStatus::Idle,
        metadata: req.metadata.unwrap_or(Value::Null),
        created_at: Utc::now(),
    };

    if let Err(e) = state.store.create_agent(agent.clone()).await {
        // Best-effort rollback so a failed agent insert doesn't
        // leak a live node.
        let _ = provider.delete_node(&handle.id).await;
        let _ = state.store.mark_node_deleted(&handle.id.0).await;
        return Err(e.into());
    }

    let view = agent_view(agent, provider_type)?;
    Ok((
        StatusCode::CREATED,
        Json(CreateAgentResponse { agent: view, jwt }),
    ))
}

async fn list_agents(
    State(state): State<AppState>,
    Query(q): Query<ListAgentsQuery>,
) -> Result<Json<Vec<AgentView>>, ApiError> {
    let agents = state.store.list_agents(q.user.as_deref()).await?;
    let mut out = Vec::with_capacity(agents.len());
    for agent in agents {
        let provider_type = state
            .store
            .get_pool(&agent.pool_name)
            .await?
            .map(|p| p.provider_type)
            .unwrap_or_else(|| "unknown".to_string());
        out.push(agent_view(agent, provider_type)?);
    }
    Ok(Json(out))
}

async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AgentView>, ApiError> {
    let agent = require_agent(&state, &id).await?;
    let provider_type = state
        .store
        .get_pool(&agent.pool_name)
        .await?
        .map(|p| p.provider_type)
        .unwrap_or_else(|| "unknown".to_string());
    Ok(Json(agent_view(agent, provider_type)?))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let agent = require_agent(&state, &id).await?;
    // Best-effort node teardown — `delete_node` is idempotent per the
    // trait contract, so an unknown node is not an error.
    if let Some(pool) = state.store.get_pool(&agent.pool_name).await? {
        if let Ok((provider, _)) = provider_for_pool(&state, &pool).await {
            let _ = provider.delete_node(&NodeId(agent.node_id.clone())).await;
        }
    }
    state.store.mark_node_deleted(&agent.node_id).await?;
    state.store.delete_agent(&agent.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_agent_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AgentStatusResponse>, ApiError> {
    let agent = require_agent(&state, &id).await?;
    Ok(Json(AgentStatusResponse {
        id: agent.id,
        status: agent.status,
    }))
}

// ── helpers ──────────────────────────────────────────────────────

async fn require_pool(state: &AppState, name: &str) -> Result<ComputePool, ApiError> {
    state
        .store
        .get_pool(name)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pool '{name}'")))
}

async fn require_agent(state: &AppState, id: &str) -> Result<Agent, ApiError> {
    state
        .store
        .get_agent(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent '{id}'")))
}

/// Look up (or lazily instantiate) the live `ComputeProvider` for a
/// pool. Caches the instance on `state.pool_runtimes` so subsequent
/// requests reuse it. Also returns the `ResolvedConfig` because the
/// caller usually needs to read pool defaults like `default.image`
/// out of it right after.
async fn provider_for_pool(
    state: &AppState,
    pool: &ComputePool,
) -> Result<(Arc<dyn ComputeProvider>, ResolvedConfig), ApiError> {
    let raw = pool_config_to_raw(&pool.config_json)?;
    let resolved = state.resolver.resolve(raw).await?;

    // Fast path: provider already cached, no lock contention.
    if let Some(existing) = state.pool_runtime(&pool.name) {
        return Ok((existing, resolved));
    }

    // Slow path: instantiate under the write lock so two concurrent
    // callers can't both build a provider and orphan one of them.
    // `pool_runtime_get_or_try_insert` re-checks under the lock.
    let plugin = state.providers.get(&pool.provider_type).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "no compiled-in provider plugin for type '{}'",
            pool.provider_type
        ))
    })?;
    let resolved_for_make = resolved.clone();
    let provider = state.pool_runtime_get_or_try_insert(&pool.name, || {
        plugin
            .instantiate(resolved_for_make)
            .map_err(ApiError::from)
    })?;
    Ok((provider, resolved))
}

fn pool_config_to_raw(config_json: &Value) -> Result<RawConfig, ApiError> {
    // The stored config_json is the round-trip output of `PoolConfig`,
    // so this should always succeed; parse defensively anyway so a
    // hand-edited store row produces a clean 400 instead of a panic.
    let parsed = PoolConfig::parse(config_json)?;
    Ok(parsed
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect())
}

fn agent_view(agent: Agent, provider_type: String) -> Result<AgentView, ApiError> {
    let user = Uuid::parse_str(&agent.user)
        .map_err(|e| ApiError::BadRequest(format!("stored agent user is not a uuid: {e}")))?;
    Ok(AgentView {
        id: agent.id,
        user,
        conversation_id: agent.conversation_id,
        compute: ComputeRef {
            provider_type,
            pool: agent.pool_name,
            node_id: agent.node_id,
        },
        image: agent.image,
        status: agent.status,
        created_at: agent.created_at,
        metadata: agent.metadata,
    })
}

fn parse_opt_u32(value: Option<&str>) -> Option<u32> {
    value.and_then(|v| v.parse::<u32>().ok())
}
