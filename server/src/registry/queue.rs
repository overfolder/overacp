//! `InMemoryMessageQueue` — bounded per-agent buffer for `session/message`
//! pushes that arrive while the agent's tunnel is disconnected.
//!
//! When `POST /agents/{id}/messages` runs against an agent that has
//! no live tunnel in [`crate::registry::AgentRegistry`], the broker
//! enqueues the rendered notification frame here. The next time the
//! agent's tunnel registers, the tunnel write loop drains the queue
//! and sends every buffered frame down the wire before serving live
//! traffic.
//!
//! The queue is in-memory and lossy across restarts. The operator's
//! REST clients are expected to re-push anything they care about
//! after a broker restart.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dashmap::DashMap;
use thiserror::Error;
use uuid::Uuid;

/// How many notifications we'll buffer per agent before refusing
/// further pushes. Picked to be large enough for normal flapping
/// reconnects but small enough that a wedged agent can't accumulate
/// unbounded backlog.
pub const DEFAULT_PER_AGENT_CAPACITY: usize = 64;

#[derive(Debug, Error)]
pub enum QueueError {
    /// The per-agent buffer is full. Caller should treat this as
    /// "back-pressure": surface the error to the operator and let
    /// them decide whether to retry or drop.
    #[error("message queue for agent {agent_id} is full ({capacity} messages)")]
    Full { agent_id: Uuid, capacity: usize },
}

/// Trait for message queue implementations. The in-memory default
/// uses a bounded `VecDeque` per agent; the Redis backend uses a
/// capped stream.
#[async_trait]
pub trait MessageQueueProvider: Send + Sync {
    /// Append `frame` to `agent_id`'s buffer. Returns `Err(Full)`
    /// if the buffer is at capacity.
    async fn push(&self, agent_id: Uuid, frame: String) -> Result<(), QueueError>;

    /// Drain `agent_id`'s buffer in FIFO order. Returns an empty
    /// vec if there is nothing to drain.
    async fn drain(&self, agent_id: Uuid) -> Vec<String>;

    /// Number of buffered frames for `agent_id`.
    async fn len(&self, agent_id: Uuid) -> usize;

    /// Whether `agent_id`'s buffer is empty (or absent).
    async fn is_empty(&self, agent_id: Uuid) -> bool;

    /// Per-agent capacity ceiling.
    fn capacity(&self) -> usize;
}

/// Per-agent bounded notification buffer. Cheap to clone.
#[derive(Clone)]
pub struct InMemoryMessageQueue {
    inner: Arc<DashMap<Uuid, Mutex<VecDeque<String>>>>,
    per_agent_capacity: usize,
}

impl Default for InMemoryMessageQueue {
    fn default() -> Self {
        Self::new(DEFAULT_PER_AGENT_CAPACITY)
    }
}

impl InMemoryMessageQueue {
    /// Build a queue with the given per-agent capacity.
    pub fn new(per_agent_capacity: usize) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            per_agent_capacity,
        }
    }

    /// Append `frame` to `agent_id`'s buffer. Returns `Err(Full)`
    /// if the buffer is at capacity.
    pub fn push(&self, agent_id: Uuid, frame: String) -> Result<(), QueueError> {
        let entry = self
            .inner
            .entry(agent_id)
            .or_insert_with(|| Mutex::new(VecDeque::new()));
        let mut guard = entry.value().lock().unwrap_or_else(|p| p.into_inner());
        if guard.len() >= self.per_agent_capacity {
            return Err(QueueError::Full {
                agent_id,
                capacity: self.per_agent_capacity,
            });
        }
        guard.push_back(frame);
        Ok(())
    }

    /// Drain `agent_id`'s buffer in FIFO order, removing the per-
    /// agent slot from the map. Returns an empty vec if there is
    /// nothing to drain.
    pub fn drain(&self, agent_id: Uuid) -> Vec<String> {
        let Some((_, slot)) = self.inner.remove(&agent_id) else {
            return Vec::new();
        };
        let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::with_capacity(guard.len());
        while let Some(frame) = guard.pop_front() {
            out.push(frame);
        }
        out
    }

    /// Number of buffered frames for `agent_id`. O(1) lookup; O(1)
    /// length read inside the per-agent lock.
    pub fn len(&self, agent_id: Uuid) -> usize {
        self.inner
            .get(&agent_id)
            .map(|slot| {
                slot.value()
                    .lock()
                    .map(|g| g.len())
                    .unwrap_or_else(|p| p.into_inner().len())
            })
            .unwrap_or(0)
    }

    /// Whether `agent_id`'s buffer is empty (or absent).
    pub fn is_empty(&self, agent_id: Uuid) -> bool {
        self.len(agent_id) == 0
    }

    /// Per-agent capacity ceiling.
    pub fn capacity(&self) -> usize {
        self.per_agent_capacity
    }
}

#[async_trait]
impl MessageQueueProvider for InMemoryMessageQueue {
    async fn push(&self, agent_id: Uuid, frame: String) -> Result<(), QueueError> {
        InMemoryMessageQueue::push(self, agent_id, frame)
    }

    async fn drain(&self, agent_id: Uuid) -> Vec<String> {
        InMemoryMessageQueue::drain(self, agent_id)
    }

    async fn len(&self, agent_id: Uuid) -> usize {
        InMemoryMessageQueue::len(self, agent_id)
    }

    async fn is_empty(&self, agent_id: Uuid) -> bool {
        InMemoryMessageQueue::is_empty(self, agent_id)
    }

    fn capacity(&self) -> usize {
        self.per_agent_capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: usize) -> String {
        format!(r#"{{"jsonrpc":"2.0","method":"session/message","params":{{"n":{n}}}}}"#)
    }

    #[test]
    fn empty_drain_returns_empty() {
        let q = InMemoryMessageQueue::default();
        assert!(q.drain(Uuid::new_v4()).is_empty());
    }

    #[test]
    fn push_then_drain_preserves_order() {
        let q = InMemoryMessageQueue::default();
        let id = Uuid::new_v4();
        q.push(id, frame(1)).unwrap();
        q.push(id, frame(2)).unwrap();
        q.push(id, frame(3)).unwrap();
        let drained = q.drain(id);
        assert_eq!(drained.len(), 3);
        assert!(drained[0].contains(r#""n":1"#));
        assert!(drained[1].contains(r#""n":2"#));
        assert!(drained[2].contains(r#""n":3"#));
    }

    #[test]
    fn drain_removes_slot() {
        let q = InMemoryMessageQueue::default();
        let id = Uuid::new_v4();
        q.push(id, frame(1)).unwrap();
        let _ = q.drain(id);
        assert_eq!(q.len(id), 0);
        // Pushing again starts a fresh slot.
        q.push(id, frame(2)).unwrap();
        assert_eq!(q.len(id), 1);
    }

    #[test]
    fn capacity_overflow_returns_full() {
        let q = InMemoryMessageQueue::new(3);
        let id = Uuid::new_v4();
        q.push(id, frame(1)).unwrap();
        q.push(id, frame(2)).unwrap();
        q.push(id, frame(3)).unwrap();
        let err = q.push(id, frame(4)).unwrap_err();
        let QueueError::Full { agent_id, capacity } = err;
        assert_eq!(agent_id, id);
        assert_eq!(capacity, 3);
    }

    #[test]
    fn agents_are_isolated() {
        let q = InMemoryMessageQueue::default();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        q.push(a, frame(1)).unwrap();
        q.push(b, frame(99)).unwrap();
        assert_eq!(q.len(a), 1);
        assert_eq!(q.len(b), 1);
        let drained_a = q.drain(a);
        assert!(drained_a[0].contains(r#""n":1"#));
        assert_eq!(q.len(b), 1);
    }

    #[test]
    fn len_and_is_empty_track_buffer() {
        let q = InMemoryMessageQueue::default();
        let id = Uuid::new_v4();
        assert!(q.is_empty(id));
        q.push(id, frame(1)).unwrap();
        assert_eq!(q.len(id), 1);
        assert!(!q.is_empty(id));
    }

    #[test]
    fn capacity_returns_configured_value() {
        assert_eq!(InMemoryMessageQueue::new(7).capacity(), 7);
        assert_eq!(
            InMemoryMessageQueue::default().capacity(),
            DEFAULT_PER_AGENT_CAPACITY
        );
    }
}
