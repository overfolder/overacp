use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Uuid,
    pub user: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub role: String,
    pub content: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolStatus {
    Active,
    Paused,
    Errored,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComputePool {
    pub name: String,
    pub provider_type: String,
    pub config_json: Value,
    pub status: PoolStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Running,
    Exited,
    Errored,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComputeNode {
    pub node_id: String,
    pub pool_name: String,
    pub status: NodeStatus,
    pub provider_metadata: Value,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub agent_refcount: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    Exited,
    Errored,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub user: String,
    pub conversation_id: Uuid,
    pub pool_name: String,
    pub node_id: String,
    pub image: String,
    pub status: AgentStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}
