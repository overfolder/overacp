//! Centralized Redis key helpers for the `redis_backend` module.
//!
//! Every Redis key used by over/ACP lives here so naming conventions
//! stay consistent and duplicates are impossible.

use uuid::Uuid;

/// Ownership lease key — `SET EX` with the owning instance ID.
pub fn owner_key(agent_id: Uuid) -> String {
    format!("overacp:owner:{agent_id}")
}

/// Tunnel directory hash — `HSET` with `instance_id`, `connected_at`.
pub fn tunnel_key(agent_id: Uuid) -> String {
    format!("overacp:tunnel:{agent_id}")
}

/// Set of currently-connected agent IDs.
pub fn connected_set_key() -> &'static str {
    "overacp:agents:connected"
}

/// Sorted set of recently-disconnected agent IDs (score = disconnect
/// timestamp in ms).
pub fn recent_zset_key() -> &'static str {
    "overacp:agents:recent"
}

/// Serialised JWT claims snapshot for an agent.
pub fn claims_key(agent_id: Uuid) -> String {
    format!("overacp:agents:claims:{agent_id}")
}

/// Pub/sub control channel for takeover / disconnect signals.
pub fn control_channel(agent_id: Uuid) -> String {
    format!("overacp:control:{agent_id}")
}

/// Per-agent inbox stream consumed via XREADGROUP.
pub fn inbox_key(agent_id: Uuid) -> String {
    format!("overacp:inbox:{agent_id}")
}

/// Dead-letter queue for undeliverable inbox entries.
pub fn dlq_key() -> &'static str {
    "overacp:inbox:dlq"
}

/// Per-agent offline message buffer (separate from the live inbox).
pub fn buffer_key(agent_id: Uuid) -> String {
    format!("overacp:buffer:{agent_id}")
}

/// Pub/sub channel for stream fan-out (SSE subscribers).
pub fn stream_channel(agent_id: Uuid) -> String {
    format!("overacp:stream:{agent_id}")
}
