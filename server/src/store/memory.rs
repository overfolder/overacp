use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use super::types::{
    Agent, AgentStatus, ComputeNode, ComputePool, Conversation, Message, NodeStatus, PoolStatus,
};
use super::{AcquireOutcome, ReleaseOutcome, SessionStore, StoreError};

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
        g.messages
            .entry(conversation_id)
            .or_default()
            .push(msg.clone());
        Ok(msg)
    }

    async fn list_messages(
        &self,
        conversation_id: Uuid,
        since: Option<Uuid>,
    ) -> Result<Vec<Message>, StoreError> {
        let g = self.inner.read().await;
        let all = g
            .messages
            .get(&conversation_id)
            .ok_or(StoreError::NotFound)?;
        let out = match since {
            None => all.clone(),
            Some(cursor) => {
                let pos = all.iter().position(|m| m.id == cursor);
                // Unknown cursor → empty slice, not a flood. A stale
                // `since` from a dropped client must not replay the
                // entire history.
                match pos {
                    Some(i) => all[i + 1..].to_vec(),
                    None => Vec::new(),
                }
            }
        };
        Ok(out)
    }

    async fn create_pool(&self, pool: ComputePool) -> Result<(), StoreError> {
        let mut g = self.inner.write().await;
        if g.pools.contains_key(&pool.name) {
            return Err(StoreError::Conflict {
                what: format!("pool {}", pool.name),
            });
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
            return Err(StoreError::Conflict {
                what: format!("agent {}", agent.id),
            });
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

    async fn acquire_node_for_agent(
        &self,
        pool_name: &str,
        mut agent: Agent,
        picker: &(dyn for<'a> Fn(&'a [ComputeNode]) -> Option<String> + Send + Sync),
        factory: &(dyn Fn() -> ComputeNode + Send + Sync),
    ) -> Result<AcquireOutcome, StoreError> {
        let mut g = self.inner.write().await;

        // 1. Pool must exist and be active.
        let pool = g.pools.get(pool_name).ok_or(StoreError::NotFound)?;
        if !matches!(pool.status, PoolStatus::Active) {
            return Err(StoreError::Conflict {
                what: format!("pool {pool_name} not active"),
            });
        }

        // 2. Agent id must be unique. Check before any node mutation
        //    so a duplicate insert can't leave a bumped refcount behind.
        if g.agents.contains_key(&agent.id) {
            return Err(StoreError::Conflict {
                what: format!("agent {}", agent.id),
            });
        }

        // 3. Snapshot of live pool nodes for the picker.
        let candidates: Vec<ComputeNode> = g
            .nodes
            .values()
            .filter(|n| n.pool_name == pool_name && n.deleted_at.is_none())
            .cloned()
            .collect();

        // 4. Pick or create.
        let (chosen_id, created) = match picker(&candidates) {
            Some(id) => (id, false),
            None => {
                let fresh = factory();
                assert_eq!(
                    fresh.pool_name, pool_name,
                    "factory minted a node for the wrong pool"
                );
                assert_eq!(
                    fresh.agent_refcount, 0,
                    "factory must mint nodes with agent_refcount = 0"
                );
                let id = fresh.node_id.clone();
                if g.nodes.contains_key(&id) {
                    return Err(StoreError::Conflict {
                        what: format!("node {id}"),
                    });
                }
                g.nodes.insert(id.clone(), fresh);
                (id, true)
            }
        };

        // 5. Bump the chosen node's refcount.
        let node = g.nodes.get_mut(&chosen_id).ok_or(StoreError::NotFound)?;
        node.agent_refcount += 1;
        let new_refcount = node.agent_refcount;
        let node_snapshot = node.clone();

        // 6. Insert the agent row, resolving its node_id.
        agent.node_id = chosen_id;
        g.agents.insert(agent.id.clone(), agent);

        Ok(AcquireOutcome {
            node: node_snapshot,
            new_refcount,
            created,
        })
    }

    async fn release_node_for_agent(&self, agent_id: &str) -> Result<ReleaseOutcome, StoreError> {
        let mut g = self.inner.write().await;

        // 1. Remove the agent row, capturing its node_id + pool.
        let agent = g.agents.remove(agent_id).ok_or(StoreError::NotFound)?;

        // 2. Read pool.node_reuse from config_json. The flag isn't yet
        //    a typed field on ComputePool — promoting it belongs with
        //    the 0.4 handler work. Default false matches §3.4.3.
        // The REST pool config is a flat string map (see
        // api/pool_config.rs), so `node_reuse` is stored as the JSON
        // string "true"/"false", not a JSON bool. Accept both shapes
        // so the flag works for pools created via the API as well as
        // ones constructed in-process with a typed bool.
        let node_reuse = g
            .pools
            .get(&agent.pool_name)
            .and_then(|p| p.config_json.get("node_reuse"))
            .map(|v| match v {
                Value::Bool(b) => *b,
                Value::String(s) => s.eq_ignore_ascii_case("true"),
                _ => false,
            })
            .unwrap_or(false);

        // 3. Decrement the node's refcount. Saturate at 0 with a
        //    debug assertion — under-decrement would mean a release
        //    without a matching acquire and is a bug.
        let node = g
            .nodes
            .get_mut(&agent.node_id)
            .ok_or(StoreError::NotFound)?;
        debug_assert!(
            node.agent_refcount > 0,
            "release on node {} with refcount {}",
            node.node_id,
            node.agent_refcount
        );
        node.agent_refcount = node.agent_refcount.saturating_sub(1);
        let new_refcount = node.agent_refcount;

        // 4. If we hit zero and the pool isn't reusing nodes, mark the
        //    row deleted in the same transaction so a crashing handler
        //    can't leave a live row pointing at a destroyed VM.
        let should_destroy = new_refcount == 0 && !node_reuse;
        if should_destroy {
            node.status = NodeStatus::Exited;
            node.deleted_at = Some(Utc::now());
        }

        Ok(ReleaseOutcome {
            node: node.clone(),
            new_refcount,
            should_destroy,
        })
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
            agent_refcount: 0,
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

        // Unknown cursor returns empty, not everything. Prevents a
        // stale client from re-downloading the full history.
        let bogus = s
            .list_messages(conv.id, Some(Uuid::new_v4()))
            .await
            .unwrap();
        assert!(bogus.is_empty());
    }

    // ---- agent_refcount lifecycle (controlplane.md §3.4.3) ---------------

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn pool_with_reuse(name: &str, node_reuse: bool) -> ComputePool {
        let now = Utc::now();
        ComputePool {
            name: name.to_string(),
            provider_type: "local-process".to_string(),
            // Stored as a JSON string to match how the REST flat
            // string-map config arrives in production.
            config_json: json!({"node_reuse": node_reuse.to_string()}),
            status: PoolStatus::Active,
            created_at: now,
            updated_at: now,
        }
    }

    fn fresh_node_factory(id: &'static str, pool_name: &'static str) -> impl Fn() -> ComputeNode {
        move || node(id, pool_name)
    }

    #[tokio::test]
    async fn acquire_creates_fresh_node_when_picker_returns_none() {
        let s = InMemoryStore::new();
        s.create_pool(pool("p1")).await.unwrap();

        let out = s
            .acquire_node_for_agent(
                "p1",
                agent("a1", "u1", "p1", ""),
                &|_| None,
                &fresh_node_factory("n1", "p1"),
            )
            .await
            .unwrap();

        assert!(out.created);
        assert_eq!(out.new_refcount, 1);
        assert_eq!(out.node.node_id, "n1");
        assert_eq!(s.get_node("n1").await.unwrap().unwrap().agent_refcount, 1);
        assert_eq!(s.get_agent("a1").await.unwrap().unwrap().node_id, "n1");
    }

    #[tokio::test]
    async fn acquire_reuses_node_via_picker() {
        let s = InMemoryStore::new();
        s.create_pool(pool("p1")).await.unwrap();
        s.upsert_node(node("n1", "p1")).await.unwrap();

        let factory_called = Arc::new(AtomicBool::new(false));
        let factory_called_c = factory_called.clone();
        let factory = move || {
            factory_called_c.store(true, Ordering::SeqCst);
            node("never", "p1")
        };

        let out = s
            .acquire_node_for_agent(
                "p1",
                agent("a1", "u1", "p1", ""),
                &|cands| cands.first().map(|n| n.node_id.clone()),
                &factory,
            )
            .await
            .unwrap();

        assert!(!out.created);
        assert_eq!(out.new_refcount, 1);
        assert_eq!(out.node.node_id, "n1");
        assert!(!factory_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn multi_agent_attach_bumps_refcount() {
        let s = InMemoryStore::new();
        s.create_pool(pool("p1")).await.unwrap();

        let pick_first = |cands: &[ComputeNode]| cands.first().map(|n| n.node_id.clone());

        let o1 = s
            .acquire_node_for_agent(
                "p1",
                agent("a1", "u1", "p1", ""),
                &pick_first,
                &fresh_node_factory("n1", "p1"),
            )
            .await
            .unwrap();
        let o2 = s
            .acquire_node_for_agent(
                "p1",
                agent("a2", "u2", "p1", ""),
                &pick_first,
                &fresh_node_factory("n1", "p1"),
            )
            .await
            .unwrap();
        let o3 = s
            .acquire_node_for_agent(
                "p1",
                agent("a3", "u3", "p1", ""),
                &pick_first,
                &fresh_node_factory("n1", "p1"),
            )
            .await
            .unwrap();

        assert_eq!(o1.new_refcount, 1);
        assert_eq!(o2.new_refcount, 2);
        assert_eq!(o3.new_refcount, 3);
        assert_eq!(s.list_nodes("p1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn release_decrements_refcount() {
        let s = InMemoryStore::new();
        s.create_pool(pool("p1")).await.unwrap();
        let pick_first = |cands: &[ComputeNode]| cands.first().map(|n| n.node_id.clone());
        for id in ["a1", "a2", "a3"] {
            s.acquire_node_for_agent(
                "p1",
                agent(id, "u", "p1", ""),
                &pick_first,
                &fresh_node_factory("n1", "p1"),
            )
            .await
            .unwrap();
        }

        let r = s.release_node_for_agent("a2").await.unwrap();
        assert_eq!(r.new_refcount, 2);
        assert!(!r.should_destroy);
        let n = s.get_node("n1").await.unwrap().unwrap();
        assert_eq!(n.agent_refcount, 2);
        assert!(n.deleted_at.is_none());
        assert!(s.get_agent("a2").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn release_to_zero_destroys_when_node_reuse_false() {
        let s = InMemoryStore::new();
        s.create_pool(pool_with_reuse("p1", false)).await.unwrap();
        s.acquire_node_for_agent(
            "p1",
            agent("a1", "u1", "p1", ""),
            &|_| None,
            &fresh_node_factory("n1", "p1"),
        )
        .await
        .unwrap();

        let r = s.release_node_for_agent("a1").await.unwrap();
        assert_eq!(r.new_refcount, 0);
        assert!(r.should_destroy);
        let n = s.get_node("n1").await.unwrap().unwrap();
        assert!(n.deleted_at.is_some());
        assert_eq!(n.status, NodeStatus::Exited);
    }

    #[tokio::test]
    async fn release_to_zero_keeps_node_when_node_reuse_true() {
        let s = InMemoryStore::new();
        s.create_pool(pool_with_reuse("p1", true)).await.unwrap();
        s.acquire_node_for_agent(
            "p1",
            agent("a1", "u1", "p1", ""),
            &|_| None,
            &fresh_node_factory("n1", "p1"),
        )
        .await
        .unwrap();

        let r = s.release_node_for_agent("a1").await.unwrap();
        assert_eq!(r.new_refcount, 0);
        assert!(!r.should_destroy);
        let n = s.get_node("n1").await.unwrap().unwrap();
        assert!(n.deleted_at.is_none());
    }

    #[tokio::test]
    async fn concurrent_acquire_on_same_pool_serializes() {
        let s = Arc::new(InMemoryStore::new());
        s.create_pool(pool("p1")).await.unwrap();

        let factory_calls = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for i in 0..8u32 {
            let s = s.clone();
            let factory_calls = factory_calls.clone();
            handles.push(tokio::spawn(async move {
                let factory = {
                    let factory_calls = factory_calls.clone();
                    move || {
                        factory_calls.fetch_add(1, Ordering::SeqCst);
                        node("n1", "p1")
                    }
                };
                s.acquire_node_for_agent(
                    "p1",
                    agent(&format!("a{i}"), "u", "p1", ""),
                    &|cands| cands.first().map(|n| n.node_id.clone()),
                    &factory,
                )
                .await
                .unwrap()
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
        let nodes = s.list_nodes("p1").await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].agent_refcount, 8);
        for i in 0..8 {
            let a = s.get_agent(&format!("a{i}")).await.unwrap().unwrap();
            assert_eq!(a.node_id, "n1");
        }
    }

    #[tokio::test]
    async fn acquire_rejects_duplicate_agent_id() {
        let s = InMemoryStore::new();
        s.create_pool(pool("p1")).await.unwrap();
        s.acquire_node_for_agent(
            "p1",
            agent("a1", "u1", "p1", ""),
            &|_| None,
            &fresh_node_factory("n1", "p1"),
        )
        .await
        .unwrap();

        let err = s
            .acquire_node_for_agent(
                "p1",
                agent("a1", "u1", "p1", ""),
                &|cands| cands.first().map(|n| n.node_id.clone()),
                &fresh_node_factory("n2", "p1"),
            )
            .await;
        assert!(matches!(err, Err(StoreError::Conflict { .. })));
        // Refcount must NOT have been bumped a second time.
        assert_eq!(s.get_node("n1").await.unwrap().unwrap().agent_refcount, 1);
    }

    #[tokio::test]
    async fn release_unknown_agent_returns_not_found() {
        let s = InMemoryStore::new();
        assert!(matches!(
            s.release_node_for_agent("ghost").await,
            Err(StoreError::NotFound)
        ));
    }
}
