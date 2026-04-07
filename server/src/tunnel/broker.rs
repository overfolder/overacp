//! Per-session in-memory fan-out for `stream/*` notifications.
//!
//! The REST surface from `docs/design/controlplane.md` § 3.5 will
//! consume this via `subscribe(session_id)` to feed
//! `GET /agents/{id}/stream` SSE clients. For now this lives
//! in process; multi-node deployments will swap in a Valkey-backed
//! impl later.

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

    /// Get-or-create the broadcast sender for a session.
    pub fn sender_for(&self, session_id: Uuid) -> broadcast::Sender<String> {
        self.bus
            .entry(session_id)
            .or_insert_with(|| broadcast::channel::<String>(STREAM_BUFFER).0)
            .clone()
    }

    /// Subscribe to a session's stream. Creates the channel if needed.
    pub fn subscribe(&self, session_id: Uuid) -> broadcast::Receiver<String> {
        self.sender_for(session_id).subscribe()
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
