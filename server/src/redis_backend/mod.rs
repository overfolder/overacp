//! Redis/Valkey backend for multi-instance HA.
//!
//! Behind `#[cfg(feature = "redis")]`. Provides distributed
//! implementations of `AgentRegistryProvider`, `MessageQueueProvider`,
//! and `StreamBrokerProvider` using Redis ownership leases, inbox
//! streams, and pub/sub.

pub mod broker;
pub mod inbox;
pub mod keys;
pub mod lease;
pub mod queue;
pub mod registry;

use std::env;
use std::sync::{Arc, OnceLock};

use redis::aio::ConnectionManager;

use self::broker::RedisStreamBroker;
use self::queue::RedisMessageQueue;
use self::registry::RedisAgentRegistry;
use crate::registry::agent::AgentRegistryProvider;
use crate::registry::queue::MessageQueueProvider;
use crate::tunnel::broker::StreamBrokerProvider;

/// Default per-agent message buffer capacity.
const DEFAULT_QUEUE_CAPACITY: usize = 64;

/// Lazily resolved instance identifier (hostname or random fallback).
/// Mirrors `overfolder/agent-runner/src/session_lock.rs:43-51`.
pub fn instance_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        env::var("OVERACP_INSTANCE_ID")
            .or_else(|_| env::var("HOSTNAME"))
            .or_else(|_| env::var("K_REVISION"))
            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string()[..8].to_string())
    })
}

/// The three Redis-backed providers, ready to plug into `AppState`.
pub struct RedisProviders {
    pub registry: Arc<dyn AgentRegistryProvider>,
    pub message_queue: Arc<dyn MessageQueueProvider>,
    pub stream_broker: Arc<dyn StreamBrokerProvider>,
}

impl RedisProviders {
    /// Connect to Redis and build all three providers.
    pub async fn from_url(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let conn = ConnectionManager::new(client).await?;
        let id = instance_id().to_string();

        Ok(Self {
            registry: Arc::new(RedisAgentRegistry::new(
                conn.clone(),
                redis_url.to_string(),
                id,
            )),
            message_queue: Arc::new(RedisMessageQueue::new(conn.clone(), DEFAULT_QUEUE_CAPACITY)),
            stream_broker: Arc::new(RedisStreamBroker::new(conn, redis_url.to_string())),
        })
    }
}
