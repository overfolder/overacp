//! `RedisMessageQueue` — Redis-backed per-agent buffer for
//! `session/message` pushes that arrive while the agent is offline.
//!
//! Uses a separate key `overacp:buffer:{agent_id}` from the inbox
//! stream (which is for live delivery to a connected agent). The
//! buffer is drained atomically on reconnect.

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::Script;
use uuid::Uuid;

use crate::registry::queue::{MessageQueueProvider, QueueError};

use super::keys::buffer_key;

/// Lua script: atomic XRANGE + DEL. Reads all entries and deletes the
/// stream in one round-trip so no push can slip in between.
const DRAIN_SCRIPT: &str = r#"
local entries = redis.call("XRANGE", KEYS[1], "-", "+", "COUNT", ARGV[1])
if #entries > 0 then
    redis.call("DEL", KEYS[1])
end
return entries
"#;

/// Lua script: atomic check-length-then-add. Returns 1 on success,
/// 0 if the stream already has >= capacity entries.
const PUSH_SCRIPT: &str = r#"
local len = redis.call("XLEN", KEYS[1])
if len >= tonumber(ARGV[1]) then
    return 0
end
redis.call("XADD", KEYS[1], "*", "frame", ARGV[2])
return 1
"#;

pub struct RedisMessageQueue {
    conn: ConnectionManager,
    per_agent_capacity: usize,
}

impl RedisMessageQueue {
    pub fn new(conn: ConnectionManager, per_agent_capacity: usize) -> Self {
        Self {
            conn,
            per_agent_capacity,
        }
    }
}

#[async_trait]
impl MessageQueueProvider for RedisMessageQueue {
    async fn push(&self, agent_id: Uuid, frame: String) -> Result<(), QueueError> {
        let key = buffer_key(agent_id);
        let mut conn = self.conn.clone();

        // Atomic check-and-add via Lua to avoid TOCTOU race between
        // XLEN and XADD.
        let added: i32 = Script::new(PUSH_SCRIPT)
            .key(&key)
            .arg(self.per_agent_capacity)
            .arg(&frame)
            .invoke_async(&mut conn)
            .await
            .map_err(|_| QueueError::Full {
                agent_id,
                capacity: self.per_agent_capacity,
            })?;

        if added == 0 {
            return Err(QueueError::Full {
                agent_id,
                capacity: self.per_agent_capacity,
            });
        }

        Ok(())
    }

    async fn drain(&self, agent_id: Uuid) -> Vec<String> {
        let key = buffer_key(agent_id);
        let mut conn = self.conn.clone();

        // Atomic XRANGE + DEL via Lua to avoid losing messages pushed
        // between the two commands.
        let result: Result<Vec<(String, Vec<(String, String)>)>, _> = Script::new(DRAIN_SCRIPT)
            .key(&key)
            .arg(self.per_agent_capacity)
            .invoke_async(&mut conn)
            .await;

        match result {
            Ok(entries) => entries
                .into_iter()
                .filter_map(|(_id, fields)| {
                    fields
                        .into_iter()
                        .find(|(k, _)| k == "frame")
                        .map(|(_, v)| v)
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    async fn len(&self, agent_id: Uuid) -> usize {
        let key = buffer_key(agent_id);
        let mut conn = self.conn.clone();
        redis::cmd("XLEN")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap_or(0)
    }

    async fn is_empty(&self, agent_id: Uuid) -> bool {
        self.len(agent_id).await == 0
    }

    fn capacity(&self) -> usize {
        self.per_agent_capacity
    }
}
