pub mod memory;
pub mod types;

use std::error::Error as StdError;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

pub use memory::InMemoryStore;
pub use types::{
    Agent, AgentStatus, ComputeNode, ComputePool, Conversation, Message, NodeStatus, PoolStatus,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {what}")]
    Conflict { what: String },
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn StdError + Send + Sync>),
}

#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    // conversations
    async fn create_conversation(&self, user: &str) -> Result<Conversation, StoreError>;
    async fn get_conversation(&self, id: Uuid) -> Result<Option<Conversation>, StoreError>;

    // messages
    async fn append_message(
        &self,
        conversation_id: Uuid,
        role: &str,
        content: Value,
    ) -> Result<Message, StoreError>;
    async fn list_messages(
        &self,
        conversation_id: Uuid,
        since: Option<Uuid>,
    ) -> Result<Vec<Message>, StoreError>;

    // compute pools
    async fn create_pool(&self, pool: ComputePool) -> Result<(), StoreError>;
    async fn get_pool(&self, name: &str) -> Result<Option<ComputePool>, StoreError>;
    async fn list_pools(&self) -> Result<Vec<ComputePool>, StoreError>;
    async fn update_pool_config(&self, name: &str, config_json: Value) -> Result<(), StoreError>;
    async fn set_pool_status(&self, name: &str, status: PoolStatus) -> Result<(), StoreError>;
    async fn delete_pool(&self, name: &str) -> Result<(), StoreError>;

    // compute nodes
    async fn upsert_node(&self, node: ComputeNode) -> Result<(), StoreError>;
    async fn get_node(&self, node_id: &str) -> Result<Option<ComputeNode>, StoreError>;
    async fn list_nodes(&self, pool_name: &str) -> Result<Vec<ComputeNode>, StoreError>;
    async fn mark_node_deleted(&self, node_id: &str) -> Result<(), StoreError>;

    // agents
    async fn create_agent(&self, agent: Agent) -> Result<(), StoreError>;
    async fn get_agent(&self, id: &str) -> Result<Option<Agent>, StoreError>;
    async fn list_agents(&self, user: Option<&str>) -> Result<Vec<Agent>, StoreError>;
    async fn set_agent_status(&self, id: &str, status: AgentStatus) -> Result<(), StoreError>;
    async fn delete_agent(&self, id: &str) -> Result<(), StoreError>;
}
