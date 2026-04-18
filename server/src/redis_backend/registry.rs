//! `RedisAgentRegistry` — Redis-backed agent registry implementing
//! `AgentRegistryProvider`.
//!
//! Combines ownership leases, inbox streams, and the connected/recent
//! metadata to provide multi-instance agent routing. The "produce
//! unconditionally" pattern means `deliver()` always writes to the
//! per-agent inbox stream; the owner's XREADGROUP consumer drains it.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::auth::Claims;
use crate::registry::agent::{
    AgentDescription, AgentRegistryProvider, DeliveryOutcome, RegistryError, TunnelLease,
};

use super::inbox;
use super::keys::{
    claims_key, connected_set_key, control_channel, inbox_key, owner_key, recent_zset_key,
};
use super::lease;

/// Serializable claims snapshot stored in Redis for `list` / `describe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaimsSnapshot {
    agent_id: Uuid,
    user: Option<Uuid>,
    connected_at: String,
}

pub struct RedisAgentRegistry {
    conn: ConnectionManager,
    /// Redis URL for creating dedicated pub/sub connections.
    redis_url: String,
    instance_id: String,
    /// Local senders for agents owned by this instance. The inbox
    /// consumer reads from the stream and forwards to these senders.
    local_senders: Arc<DashMap<Uuid, mpsc::UnboundedSender<String>>>,
    /// Inbox consumer handles — aborted when the lease drops.
    consumer_handles: Arc<DashMap<Uuid, JoinHandle<()>>>,
}

impl RedisAgentRegistry {
    pub fn new(conn: ConnectionManager, redis_url: String, instance_id: String) -> Self {
        Self {
            conn,
            redis_url,
            instance_id,
            local_senders: Arc::new(DashMap::new()),
            consumer_handles: Arc::new(DashMap::new()),
        }
    }
}

#[async_trait]
impl AgentRegistryProvider for RedisAgentRegistry {
    async fn acquire(
        &self,
        agent_id: Uuid,
        local_tx: mpsc::UnboundedSender<String>,
        claims: Claims,
    ) -> Result<TunnelLease, RegistryError> {
        // Serialize claims for the Redis metadata store.
        let snapshot = ClaimsSnapshot {
            agent_id,
            user: claims.user,
            connected_at: chrono::Utc::now().to_rfc3339(),
        };
        let claims_json = serde_json::to_string(&snapshot).unwrap_or_default();

        // Acquire the ownership lease.
        let ownership = lease::acquire_lease(&self.conn, agent_id, &self.instance_id, &claims_json)
            .await
            .map_err(|e| RegistryError::AcquireFailed {
                agent_id,
                reason: e.to_string(),
            })?;

        // Ensure the consumer group exists on the inbox stream.
        inbox::ensure_consumer_group(&self.conn, agent_id).await;

        // Store the local sender so the inbox consumer can forward.
        self.local_senders.insert(agent_id, local_tx.clone());

        // Spawn the inbox consumer.
        let consumer_id = format!("{}-{agent_id}", self.instance_id);
        let handle = inbox::spawn_consumer(self.conn.clone(), agent_id, consumer_id, local_tx);
        self.consumer_handles.insert(agent_id, handle);

        info!(%agent_id, instance = %self.instance_id, "redis tunnel lease acquired");

        // Build the TunnelLease RAII guard.
        let senders = self.local_senders.clone();
        let handles = self.consumer_handles.clone();
        Ok(TunnelLease::new(move || {
            // Abort the inbox consumer.
            if let Some((_, handle)) = handles.remove(&agent_id) {
                handle.abort();
            }
            // Remove local sender.
            senders.remove(&agent_id);
            // The OwnershipLease drop handles Redis cleanup.
            drop(ownership);
        }))
    }

    async fn deliver(&self, agent_id: Uuid, frame: String) -> DeliveryOutcome {
        let key = owner_key(agent_id);
        let mut conn = self.conn.clone();

        // Check if any instance owns the tunnel.
        let exists: bool = redis::cmd("EXISTS")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap_or(false);

        if !exists {
            return DeliveryOutcome::NoTunnel(frame);
        }

        // Produce unconditionally to the inbox stream.
        let inbox = inbox_key(agent_id);
        let result: Result<String, _> = redis::cmd("XADD")
            .arg(&inbox)
            .arg("*")
            .arg("frame")
            .arg(&frame)
            .query_async(&mut conn)
            .await;

        match result {
            Ok(_) => DeliveryOutcome::Live,
            Err(e) => {
                debug!(%agent_id, "inbox XADD failed: {e}");
                DeliveryOutcome::NoTunnel(frame)
            }
        }
    }

    async fn is_connected(&self, agent_id: Uuid) -> bool {
        let key = owner_key(agent_id);
        let mut conn = self.conn.clone();
        redis::cmd("EXISTS")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap_or(false)
    }

    async fn list_agents(&self) -> Vec<AgentDescription> {
        let mut conn = self.conn.clone();
        let mut out = Vec::new();

        // Connected agents.
        let connected: Vec<String> = conn.smembers(connected_set_key()).await.unwrap_or_default();

        for id_str in &connected {
            if let Ok(agent_id) = Uuid::parse_str(id_str) {
                if let Some(desc) = hydrate_description(&mut conn, agent_id, true).await {
                    out.push(desc);
                }
            }
        }

        // Recently-disconnected agents.
        let recent: Vec<(String, f64)> = redis::cmd("ZRANGE")
            .arg(recent_zset_key())
            .arg(0)
            .arg(-1)
            .arg("WITHSCORES")
            .query_async(&mut conn)
            .await
            .unwrap_or_default();

        for (id_str, disconnect_ts) in &recent {
            if let Ok(agent_id) = Uuid::parse_str(id_str) {
                // Skip if also in connected (race).
                if connected.contains(id_str) {
                    continue;
                }
                let idle_secs = {
                    let now_ms = chrono::Utc::now().timestamp_millis() as f64;
                    ((now_ms - disconnect_ts) / 1000.0).max(0.0) as u64
                };
                out.push(AgentDescription {
                    agent_id,
                    connected: false,
                    uptime_secs: None,
                    idle_secs: Some(idle_secs),
                    user: None,
                });
            }
        }

        out
    }

    async fn describe_agent(&self, agent_id: Uuid) -> Option<AgentDescription> {
        let mut conn = self.conn.clone();

        // Check connected first.
        let is_member: bool = conn
            .sismember(connected_set_key(), agent_id.to_string())
            .await
            .unwrap_or(false);

        if is_member {
            return hydrate_description(&mut conn, agent_id, true).await;
        }

        // Check recently-disconnected.
        let score: Option<f64> = redis::cmd("ZSCORE")
            .arg(recent_zset_key())
            .arg(agent_id.to_string())
            .query_async(&mut conn)
            .await
            .ok();

        score.map(|disconnect_ts| {
            let now_ms = chrono::Utc::now().timestamp_millis() as f64;
            let idle_secs = ((now_ms - disconnect_ts) / 1000.0).max(0.0) as u64;
            AgentDescription {
                agent_id,
                connected: false,
                uptime_secs: None,
                idle_secs: Some(idle_secs),
                user: None,
            }
        })
    }

    async fn disconnect(&self, agent_id: Uuid) {
        let mut conn = self.conn.clone();
        let channel = control_channel(agent_id);
        let _: Result<(), _> = redis::cmd("PUBLISH")
            .arg(&channel)
            .arg("disconnect")
            .query_async(&mut conn)
            .await;
    }

    async fn touch(&self, _agent_id: Uuid) {
        // No-op in Redis mode. Activity is tracked via the lease
        // heartbeat timestamp. The `idle_secs` field in
        // `AgentDescription` is derived from the connected_at
        // or heartbeat time.
    }

    fn control_receiver(&self, agent_id: Uuid) -> Option<mpsc::UnboundedReceiver<String>> {
        let channel = control_channel(agent_id);
        let redis_url = self.redis_url.clone();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let client = match redis::Client::open(redis_url.as_str()) {
                Ok(c) => c,
                Err(e) => {
                    warn!("control subscriber: redis client error: {e}");
                    return;
                }
            };
            let mut pubsub = match client.get_async_pubsub().await {
                Ok(ps) => ps,
                Err(e) => {
                    warn!("control subscriber: pubsub connect error: {e}");
                    return;
                }
            };
            if let Err(e) = pubsub.subscribe(&channel).await {
                warn!("control subscriber: subscribe error: {e}");
                return;
            }

            let mut stream = pubsub.on_message();
            loop {
                tokio::select! {
                    msg = stream.next() => {
                        match msg {
                            Some(msg) => {
                                let payload: String = match msg.get_payload() {
                                    Ok(p) => p,
                                    Err(_) => continue,
                                };
                                if tx.send(payload).is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = tx.closed() => {
                        break;
                    }
                }
            }
        });

        Some(rx)
    }
}

/// Hydrate an `AgentDescription` from the claims snapshot in Redis.
async fn hydrate_description(
    conn: &mut ConnectionManager,
    agent_id: Uuid,
    connected: bool,
) -> Option<AgentDescription> {
    let claims_json: Option<String> = conn.get(claims_key(agent_id)).await.ok()?;

    let snapshot: Option<ClaimsSnapshot> = claims_json.and_then(|j| serde_json::from_str(&j).ok());

    let (uptime_secs, idle_secs, user) = if let Some(snap) = snapshot {
        let connected_at = chrono::DateTime::parse_from_rfc3339(&snap.connected_at)
            .map(|dt| dt.timestamp())
            .unwrap_or(0);
        let now = chrono::Utc::now().timestamp();
        let uptime = (now - connected_at).max(0) as u64;
        (Some(uptime), Some(0), snap.user)
    } else {
        (None, None, None)
    };

    Some(AgentDescription {
        agent_id,
        connected,
        uptime_secs,
        idle_secs,
        user,
    })
}
