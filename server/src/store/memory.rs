use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use super::types::{
    Agent, AgentStatus, ComputeNode, ComputePool, Conversation, Message, NodeStatus, PoolStatus,
};
use super::{SessionStore, StoreError};

#[derive(Default)]
struct Inner {
    conversations: HashMap<Uuid, Conversation>,
    messages: HashMap<Uuid, Vec<Message>>,
    pools: HashMap<String, ComputePool>,
    nodes: HashMap<String, ComputeNode>,
    agents: HashMap<String, Agent>,
}

#[derive(Default)]
pub struct InMemoryStore {
    inner: RwLock<Inner>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for InMemoryStore {
    async fn create_conversation(&self, user: &str) -> Result<Conversation, StoreError> {
        let conv = Conversation {
            id: Uuid::new_v4(),
            user: user.to_string(),
            created_at: Utc::now(),
        };
        let mut g = self.inner.write().await;
        g.conversations.insert(conv.id, conv.clone());
        g.messages.insert(conv.id, Vec::new());
        Ok(conv)
    }

    async fn get_conversation(&self, id: Uuid) -> Result<Option<Conversation>, StoreError> {
        Ok(self.inner.read().await.conversations.get(&id).cloned())
    }

    async fn append_message(
        &self,
        conversation_id: Uuid,
        role: &str,
        content: Value,
    ) -> Result<Message, StoreError> {
        let mut g = self.inner.write().await;
        if !g.conversations.contains_key(&conversation_id) {
            return Err(StoreError::NotFound);
        }
        let msg = Message {
            id: Uuid::new_v4(),
            conversation_id,
            role: role.to_string(),
            content,
            created_at: Utc::now(),
        };
        g.messages.entry(conversation_id).or_default().push(msg.clone());
        Ok(msg)
    }

    async fn list_messages(
        &self,
        conversation_id: Uuid,
        since: Option<Uuid>,
    ) -> Result<Vec<Message>, StoreError> {
        let g = self.inner.read().await;
        let all = g.messages.get(&conversation_id).ok_or(StoreError::NotFound)?;
        let out = match since {
            None => all.clone(),
            Some(cursor) => {
                let pos = all.iter().position(|m| m.id == cursor);
                match pos {
                    Some(i) => all[i + 1..].to_vec(),
                    None => all.clone(),
                }
            }
        };
        Ok(out)
    }

    async fn create_pool(&self, pool: ComputePool) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        if g.pools.contains_key(&pool.name) {
            return Err(StoreError::Conflict { what: format!("pool {}", pool.name) });
        }
        g.pools.insert(pool.name.clone(), pool);
        Ok(())
    }

    async fn get_pool(&self, name: &str) -> Result<Option<ComputePool>, StoreError> {
        Ok(self.inner.read().await.pools.get(name).cloned())
    }

    async fn list_pools(&self) -> Result<Vec<ComputePool>, StoreError> {
        Ok(self.inner.read().await.pools.values().cloned().collect())
    }

    async fn update_pool_config(&self, name: &str, config_json: Value) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        let p = g.pools.get_mut(name).ok_or(StoreError::NotFound)?;
        p.config_json = config_json;
        p.updated_at = Utc::now();
        Ok(())
    }

    async fn set_pool_status(&self, name: &str, status: PoolStatus) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        let p = g.pools.get_mut(name).ok_or(StoreError::NotFound)?;
        p.status = status;
        p.updated_at = Utc::now();
        Ok(())
    }

    async fn delete_pool(&self, name: &str) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        g.pools.remove(name).ok_or(StoreError::NotFound)?;
        Ok(())
    }

    async fn upsert_node(&self, node: ComputeNode) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        g.nodes.insert(node.node_id.clone(), node);
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<ComputeNode>, StoreError> {
        Ok(self.inner.read().await.nodes.get(node_id).cloned())
    }

    async fn list_nodes(&self, pool_name: &str) -> Result<Vec<ComputeNode>, StoreError> {
        Ok(self
            .inner
            .read()
            .await
            .nodes
            .values()
            .filter(|n| n.pool_name == pool_name)
            .cloned()
            .collect())
    }

    async fn mark_node_deleted(&self, node_id: &str) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        let n = g.nodes.get_mut(node_id).ok_or(StoreError::NotFound)?;
        n.deleted_at = Some(Utc::now());
        n.status = NodeStatus::Exited;
        Ok(())
    }

    async fn create_agent(&self, agent: Agent) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        if g.agents.contains_key(&agent.id) {
            return Err(StoreError::Conflict { what: format!("agent {}", agent.id) });
        }
        g.agents.insert(agent.id.clone(), agent);
        Ok(())
    }

    async fn get_agent(&self, id: &str) -> Result<Option<Agent>, StoreError> {
        Ok(self.inner.read().await.agents.get(id).cloned())
    }

    async fn list_agents(&self, user: Option<&str>) -> Result<Vec<Agent>, StoreError> {
        Ok(self
            .inner
            .read()
            .await
            .agents
            .values()
            .filter(|a| user.is_none_or(|u| a.user == u))
            .cloned()
            .collect())
    }

    async fn set_agent_status(&self, id: &str, status: AgentStatus) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        let a = g.agents.get_mut(id).ok_or(StoreError::NotFound)?;
        a.status = status;
        Ok(())
    }

    async fn delete_agent(&self, id: &str) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        g.agents.remove(id).ok_or(StoreError::NotFound)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pool(name: &str) -> ComputePool {
        let now = Utc::now();
        ComputePool {
            name: name.to_string(),
            provider_type: "local-process".to_string(),
            config_json: json!({"provider.class": "local-process"}),
            status: PoolStatus::Active,
            created_at: now,
            updated_at: now,
        }
    }

    fn node(id: &str, pool_name: &str) -> ComputeNode {
        ComputeNode {
            node_id: id.to_string(),
            pool_name: pool_name.to_string(),
            status: NodeStatus::Running,
            provider_metadata: json!({}),
            created_at: Utc::now(),
            deleted_at: None,
        }
    }

    fn agent(id: &str, user: &str, pool_name: &str, node_id: &str) -> Agent {
        Agent {
            id: id.to_string(),
            user: user.to_string(),
            conversation_id: Uuid::new_v4(),
            pool_name: pool_name.to_string(),
            node_id: node_id.to_string(),
            image: "img".to_string(),
            status: AgentStatus::Idle,
            metadata: json!({}),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn pool_crud_and_conflict() {
        let s = InMemoryStore::new();
        s.create_pool(pool("p1")).await.unwrap();
        assert!(s.get_pool("p1").await.unwrap().is_some());
        assert_eq!(s.list_pools().await.unwrap().len(), 1);
        assert!(matches!(
            s.create_pool(pool("p1")).await,
            Err(StoreError::Conflict { .. })
        ));
        s.update_pool_config("p1", json!({"x": 1})).await.unwrap();
        assert_eq!(
            s.get_pool("p1").await.unwrap().unwrap().config_json,
            json!({"x": 1})
        );
        s.set_pool_status("p1", PoolStatus::Paused).await.unwrap();
        assert_eq!(
            s.get_pool("p1").await.unwrap().unwrap().status,
            PoolStatus::Paused
        );
        assert!(matches!(
            s.update_pool_config("missing", json!({})).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn agents_and_messages() {
        let s = InMemoryStore::new();
        s.create_pool(pool("p1")).await.unwrap();
        s.upsert_node(node("n1", "p1")).await.unwrap();
        s.create_agent(agent("a1", "u1", "p1", "n1")).await.unwrap();
        s.create_agent(agent("a2", "u2", "p1", "n1")).await.unwrap();
        assert_eq!(s.list_agents(Some("u1")).await.unwrap().len(), 1);
        assert_eq!(s.list_agents(None).await.unwrap().len(), 2);
        assert_eq!(s.list_nodes("p1").await.unwrap().len(), 1);

        let conv = s.create_conversation("u1").await.unwrap();
        let m1 = s
            .append_message(conv.id, "user", json!("hi"))
            .await
            .unwrap();
        let _m2 = s
            .append_message(conv.id, "assistant", json!("hello"))
            .await
            .unwrap();
        let all = s.list_messages(conv.id, None).await.unwrap();
        assert_eq!(all.len(), 2);
        let after = s.list_messages(conv.id, Some(m1.id)).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].role, "assistant");
    }
}
