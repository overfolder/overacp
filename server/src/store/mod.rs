pub mod memory;
pub mod types;

use std::error::Error as StdError;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

pub use memory::InMemoryStore;
// Outcome structs are defined below; re-exported here for convenience.
pub use types::{
    Agent, AgentStatus, ComputeNode, ComputePool, Conversation, Message, NodeStatus, PoolStatus,
};

/// Outcome of an atomic `acquire_node_for_agent` call.
#[derive(Clone, Debug)]
pub struct AcquireOutcome {
    /// Post-bump snapshot of the chosen node.
    pub node: ComputeNode,
    /// New `agent_refcount` after the bump.
    pub new_refcount: i32,
    /// `true` if the factory was invoked to mint a fresh node;
    /// `false` if an existing pool node was reused.
    pub created: bool,
}

/// Outcome of an atomic `release_node_for_agent` call.
#[derive(Clone, Debug)]
pub struct ReleaseOutcome {
    /// Post-decrement snapshot of the node.
    pub node: ComputeNode,
    /// New `agent_refcount` after the decrement.
    pub new_refcount: i32,
    /// `true` iff `new_refcount == 0` and the pool has
    /// `node_reuse = false`. The store has already marked the node
    /// row as deleted in this case; the caller still owns the actual
    /// `provider.delete_node()` side-effect.
    pub should_destroy: bool,
}

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

    /// Atomically pick-or-create a node in `pool_name`, insert
    /// `agent`, and bump the chosen node's `agent_refcount` by 1.
    ///
    /// `picker` receives the live in-transaction list of non-deleted
    /// nodes in the pool and returns the chosen `node_id` or `None`.
    /// If `None`, `factory` is invoked to mint a fresh `ComputeNode`
    /// (which must have `agent_refcount = 0`); the row is inserted and
    /// then bumped to 1. The agent row's `node_id` field is overwritten
    /// with the resolved id, so callers may pass an empty string.
    ///
    /// Implementations must run all of pool lookup, picker/factory,
    /// node row mutation, and agent insert inside a single write
    /// transaction so the refcount cannot drift on crash.
    async fn acquire_node_for_agent(
        &self,
        pool_name: &str,
        agent: Agent,
        picker: &(dyn for<'a> Fn(&'a [ComputeNode]) -> Option<String> + Send + Sync),
        factory: &(dyn Fn() -> ComputeNode + Send + Sync),
    ) -> Result<AcquireOutcome, StoreError>;

    /// Atomically remove `agent_id` and decrement its node's
    /// `agent_refcount`. Returns the post-decrement node snapshot and
    /// `should_destroy = (new_refcount == 0 && !pool.node_reuse)`. The
    /// store also marks the node row deleted when `should_destroy` is
    /// true; the caller still owns the actual `provider.delete_node()`
    /// side-effect.
    async fn release_node_for_agent(&self, agent_id: &str) -> Result<ReleaseOutcome, StoreError>;
}
