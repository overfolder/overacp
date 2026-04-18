//! Per-agent in-memory fan-out for `stream/*` and `turn/end`
//! notifications.
//!
//! The tunnel read loop forwards every `stream/*`, `turn/end`, and
//! `heartbeat` frame it receives into one of these `broadcast`
//! channels; the REST `GET /agents/{id}/stream` SSE handler reads
//! from them. One channel per connected agent, keyed on the JWT
//! `sub` / `agent_id`.
//!
//! For now this lives in process only; multi-replica deployments
//! will swap in a Redis/Valkey-backed impl behind the same
//! `StreamBrokerProvider` interface.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

const STREAM_BUFFER: usize = 128;

/// Trait for stream broker implementations. The in-memory default
/// uses `tokio::sync::broadcast` channels; the Redis backend uses
/// pub/sub for cross-instance fan-out.
#[async_trait]
pub trait StreamBrokerProvider: Send + Sync {
    /// Publish a frame for fan-out to all SSE subscribers of
    /// `agent_id`, wherever they live in the cluster.
    async fn publish(&self, agent_id: Uuid, frame: String);

    /// Subscribe to an agent's stream fan-out. Returns a boxed
    /// stream that yields frames as they arrive. Dropping the
    /// stream unsubscribes.
    fn subscribe(&self, agent_id: Uuid) -> BoxStream<'static, String>;
}

#[derive(Default)]
pub struct InMemoryStreamBroker {
    bus: DashMap<Uuid, broadcast::Sender<String>>,
}

impl InMemoryStreamBroker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Get-or-create the broadcast sender for an agent.
    pub fn sender_for(&self, agent_id: Uuid) -> broadcast::Sender<String> {
        self.bus
            .entry(agent_id)
            .or_insert_with(|| broadcast::channel::<String>(STREAM_BUFFER).0)
            .clone()
    }

    /// Subscribe to an agent's stream. Creates the channel if needed.
    pub fn subscribe(&self, agent_id: Uuid) -> broadcast::Receiver<String> {
        self.sender_for(agent_id).subscribe()
    }
}

#[async_trait]
impl StreamBrokerProvider for InMemoryStreamBroker {
    async fn publish(&self, agent_id: Uuid, frame: String) {
        let sender = self.sender_for(agent_id);
        // Best-effort: if no subscribers, the send silently fails.
        let _ = sender.send(frame);
    }

    fn subscribe(&self, agent_id: Uuid) -> BoxStream<'static, String> {
        let rx = InMemoryStreamBroker::subscribe(self, agent_id);
        BroadcastStream::new(rx)
            .filter_map(|r: Result<String, _>| async { r.ok() })
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fan_out_to_subscriber() {
        let b = InMemoryStreamBroker::new();
        let id = Uuid::new_v4();
        let mut rx = b.subscribe(id);
        b.sender_for(id).send("hello".into()).unwrap();
        assert_eq!(rx.recv().await.unwrap(), "hello");
    }
}
