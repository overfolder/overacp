//! Ownership lease for a tunnel — Redis `SET EX` + Lua CAS heartbeat.
//!
//! Ported from `overfolder/agent-runner/src/session_lock.rs`. Key
//! differences from the overfolder version:
//!
//! - **Force-takeover, not NX.** over/ACP allows reconnection from a
//!   different instance (matches current in-memory behavior where
//!   `register()` overwrites). On acquire we `SET EX` unconditionally
//!   and notify the previous owner via pub/sub.
//! - **30s TTL, 10s heartbeat** (vs 5s/2s in overfolder). Tunnel
//!   sessions are longer-lived than agentic loop locks.
//! - **Tunnel directory.** We also maintain `overacp:tunnel:{agent_id}`
//!   (HASH with instance_id, connected_at) and the connected-agents
//!   set + claims record.

use std::sync::Arc;
use std::time::Duration;

use redis::aio::ConnectionManager;
use redis::Script;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::keys::{
    claims_key, connected_set_key, control_channel, owner_key, recent_zset_key, tunnel_key,
};

/// How long the ownership lease lives without a heartbeat renewal.
pub const LEASE_TTL_SECS: u64 = 30;

/// How often the heartbeat renews the lease (must be < TTL).
const HEARTBEAT_INTERVAL_SECS: u64 = 10;

/// Lua script: atomic compare-and-expire (heartbeat).
const HEARTBEAT_SCRIPT: &str = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("EXPIRE", KEYS[1], ARGV[2])
else
    return 0
end
"#;

/// Lua script: atomic compare-and-delete (release).
const RELEASE_SCRIPT: &str = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("DEL", KEYS[1])
else
    return 0
end
"#;

/// Maximum entries in the recently-disconnected sorted set.
const RECENT_CAPACITY: i64 = 64;

/// A held ownership lease with background heartbeat.
pub struct OwnershipLease {
    agent_id: Uuid,
    instance_id: String,
    conn: ConnectionManager,
    stop: Arc<Notify>,
    heartbeat_handle: JoinHandle<()>,
}

impl OwnershipLease {
    /// Release the lease explicitly (async). Also called best-effort
    /// from Drop.
    pub async fn release(self) {
        self.stop.notify_one();
        self.heartbeat_handle.abort();
        release_lease(&self.conn, self.agent_id, &self.instance_id).await;
    }
}

impl Drop for OwnershipLease {
    fn drop(&mut self) {
        self.stop.notify_one();
        self.heartbeat_handle.abort();
        // Spawn a fire-and-forget cleanup. If the runtime is shutting
        // down this may not complete — the TTL will clean up.
        let conn = self.conn.clone();
        let agent_id = self.agent_id;
        let instance_id = self.instance_id.clone();
        tokio::spawn(async move {
            release_lease(&conn, agent_id, &instance_id).await;
        });
    }
}

/// Acquire the ownership lease for `agent_id`. Force-takes over from
/// any existing owner (publishes a `takeover` signal on the control
/// channel so the previous owner can close its WebSocket).
pub async fn acquire_lease(
    conn: &ConnectionManager,
    agent_id: Uuid,
    instance_id: &str,
    claims_json: &str,
) -> Result<OwnershipLease, redis::RedisError> {
    let key = owner_key(agent_id);
    let mut c = conn.clone();

    // Check for existing owner and notify takeover if different.
    let existing: Option<String> = redis::cmd("GET").arg(&key).query_async(&mut c).await?;

    if let Some(ref prev) = existing {
        if prev != instance_id {
            info!(
                %agent_id,
                prev_owner = %prev,
                new_owner = %instance_id,
                "taking over tunnel ownership"
            );
            let _: Result<(), _> = redis::cmd("PUBLISH")
                .arg(control_channel(agent_id))
                .arg("takeover")
                .query_async(&mut c)
                .await;
        }
    }

    // Force-set the ownership lease (SET EX, no NX).
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(instance_id)
        .arg("EX")
        .arg(LEASE_TTL_SECS)
        .query_async(&mut c)
        .await?;

    // Populate the tunnel directory.
    let tunnel = tunnel_key(agent_id);
    let now = chrono::Utc::now().to_rfc3339();
    redis::pipe()
        .cmd("HSET")
        .arg(&tunnel)
        .arg("instance_id")
        .arg(instance_id)
        .arg("connected_at")
        .arg(&now)
        .ignore()
        .cmd("EXPIRE")
        .arg(&tunnel)
        .arg(LEASE_TTL_SECS * 2)
        .ignore()
        // Add to connected set.
        .cmd("SADD")
        .arg(connected_set_key())
        .arg(agent_id.to_string())
        .ignore()
        // Remove from recently-disconnected.
        .cmd("ZREM")
        .arg(recent_zset_key())
        .arg(agent_id.to_string())
        .ignore()
        // Store serialized claims.
        .cmd("SET")
        .arg(claims_key(agent_id))
        .arg(claims_json)
        .arg("EX")
        .arg(LEASE_TTL_SECS * 2)
        .ignore()
        .query_async::<()>(&mut c)
        .await?;

    // Start heartbeat.
    let stop = Arc::new(Notify::new());
    let stop_clone = stop.clone();
    let mut hb_conn = conn.clone();
    let hb_key = key.clone();
    let hb_tunnel = tunnel_key(agent_id);
    let hb_claims_key = claims_key(agent_id);
    let hb_value = instance_id.to_string();

    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        interval.tick().await; // skip immediate first tick
        loop {
            tokio::select! {
                () = stop_clone.notified() => break,
                _ = interval.tick() => {
                    // CAS-renew the ownership lease.
                    let renewed: i32 = Script::new(HEARTBEAT_SCRIPT)
                        .key(&hb_key)
                        .arg(&hb_value)
                        .arg(LEASE_TTL_SECS)
                        .invoke_async(&mut hb_conn)
                        .await
                        .unwrap_or(0);
                    if renewed == 0 {
                        warn!(%agent_id, "ownership heartbeat: key missing or stolen");
                        break;
                    }
                    // Refresh tunnel directory + claims TTL.
                    let _: Result<(), _> = redis::pipe()
                        .cmd("EXPIRE").arg(&hb_tunnel).arg(LEASE_TTL_SECS * 2).ignore()
                        .cmd("EXPIRE").arg(&hb_claims_key).arg(LEASE_TTL_SECS * 2).ignore()
                        .query_async::<()>(&mut hb_conn)
                        .await;
                }
            }
        }
    });

    debug!(%agent_id, %instance_id, ttl = LEASE_TTL_SECS, "ownership lease acquired");

    Ok(OwnershipLease {
        agent_id,
        instance_id: instance_id.to_string(),
        conn: conn.clone(),
        stop,
        heartbeat_handle,
    })
}

/// Release ownership — CAS-DEL the lease key, update connected set
/// and recently-disconnected log.
async fn release_lease(conn: &ConnectionManager, agent_id: Uuid, instance_id: &str) {
    let key = owner_key(agent_id);
    let mut c = conn.clone();

    // Atomic compare-and-delete.
    let deleted: i32 = Script::new(RELEASE_SCRIPT)
        .key(&key)
        .arg(instance_id)
        .invoke_async(&mut c)
        .await
        .unwrap_or(0);

    if deleted == 1 {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let _: Result<(), _> = redis::pipe()
            // Remove from connected set.
            .cmd("SREM")
            .arg(connected_set_key())
            .arg(agent_id.to_string())
            .ignore()
            // Add to recently-disconnected sorted set.
            .cmd("ZADD")
            .arg(recent_zset_key())
            .arg(now_ms)
            .arg(agent_id.to_string())
            .ignore()
            // Trim recent set to capacity.
            .cmd("ZREMRANGEBYRANK")
            .arg(recent_zset_key())
            .arg(0)
            .arg(-(RECENT_CAPACITY + 1))
            .ignore()
            // Clean up tunnel directory.
            .cmd("DEL")
            .arg(tunnel_key(agent_id))
            .ignore()
            .query_async::<()>(&mut c)
            .await;

        debug!(%agent_id, "ownership lease released");
    }
}
