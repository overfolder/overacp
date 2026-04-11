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
//! `StreamBroker` interface.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::broadcast;
use uuid::Uuid;

const STREAM_BUFFER: usize = 128;

#[derive(Default)]
pub struct StreamBroker {
    bus: DashMap<Uuid, broadcast::Sender<String>>,
}

impl StreamBroker {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fan_out_to_subscriber() {
        let b = StreamBroker::new();
        let id = Uuid::new_v4();
        let mut rx = b.subscribe(id);
        b.sender_for(id).send("hello".into()).unwrap();
        assert_eq!(rx.recv().await.unwrap(), "hello");
    }
}
