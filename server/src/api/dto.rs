//! Wire DTOs for the `/compute/{providers,pools}` surface.
//!
//! These are the JSON shapes documented in
//! `docs/design/controlplane.md` § 3.1–3.2. They live separately
//! from the persistence types in `crate::store::types` so the
//! REST contract can evolve independently of the storage schema.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use uuid::Uuid;

use crate::store::{AgentStatus, PoolStatus};

/// `GET /compute/providers/{type}` and the entries in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub provider_type: String,
    pub display_name: String,
    pub version: String,
}

/// `GET /compute/providers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersList {
    pub providers: Vec<ProviderInfo>,
}

/// One field-level error from a provider's validation hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationFieldError {
    pub key: String,
    pub messages: Vec<String>,
}

/// `POST /compute/providers/{type}/config/validate` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationResult {
    pub provider_type: String,
    pub valid: bool,
    pub errors: Vec<ValidationFieldError>,
}

/// `POST /compute/pools` body. The `config` field is the
/// Kafka-Connect-style flat object; we keep it as a raw `Value`
/// here so the handler can parse it into a typed `PoolConfig`
/// at the boundary and surface a precise error if the shape is
/// wrong.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreatePoolRequest {
    pub name: String,
    pub config: Value,
}

/// `PUT /compute/pools/{name}/config` body. Same shape as the
/// `config` slot in [`CreatePoolRequest`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolConfigBody {
    pub config: Value,
}

/// `GET /compute/pools` element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSummary {
    pub name: String,
    pub provider_type: String,
    pub status: PoolStatus,
}

/// `GET /compute/pools/{name}`. Mirrors the `compute_pools` row
/// in § 6 of the design doc. `config` is echoed verbatim — secret
/// references stay in their `${...}` form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolView {
    pub name: String,
    pub provider_type: String,
    pub config: Value,
    pub status: PoolStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /compute/pools/{name}/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStatusResponse {
    pub name: String,
    pub provider_type: String,
    pub state: PoolStatus,
}

// ── § 3.4 — agents ──────────────────────────────────────────────

/// `POST /agents` request body.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAgentRequest {
    pub pool: String,
    pub user: Uuid,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// `GET /agents?user=...` query.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListAgentsQuery {
    #[serde(default)]
    pub user: Option<String>,
}

/// `compute = { provider_type, pool, node_id }` block on agent
/// describe responses (design doc § 3.4.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeRef {
    pub provider_type: String,
    pub pool: String,
    pub node_id: String,
}

/// `GET /agents/{id}` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentView {
    pub id: String,
    pub user: Uuid,
    pub conversation_id: Uuid,
    pub compute: ComputeRef,
    pub image: String,
    pub status: AgentStatus,
    pub created_at: DateTime<Utc>,
    pub metadata: Value,
}

/// `POST /agents` response — agent record plus the freshly minted
/// JWT scoped to its conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAgentResponse {
    #[serde(flatten)]
    pub agent: AgentView,
    pub jwt: String,
}

/// `GET /agents/{id}/status` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusResponse {
    pub id: String,
    pub status: AgentStatus,
}
