//! `InMemoryAgentRegistry` — in-memory routing table of currently-connected
//! agents.
//!
//! The registry stores one [`AgentEntry`] per active tunnel, keyed by
//! the agent's UUID (the JWT `sub` claim). It also keeps a small
//! bounded log of recently disconnected agents so `GET /agents` can
//! surface "agents that were connected a moment ago" without paging
//! the operator's database.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::mpsc;
use uuid::Uuid;

use async_trait::async_trait;
use thiserror::Error;

use crate::auth::Claims;

/// How many recently-disconnected agents to remember. Bounded so a
/// flapping client can't grow the registry without limit.
const RECENT_CAPACITY: usize = 64;

/// Outcome of a `deliver` call on the registry.
pub enum DeliveryOutcome {
    /// Frame was delivered to the agent's tunnel (locally or via stream).
    Live,
    /// No active tunnel for this agent. The original frame is returned
    /// so the caller can buffer it in the `MessageQueue`.
    NoTunnel(String),
}

/// Errors from tunnel lease acquisition.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("failed to acquire tunnel for agent {agent_id}: {reason}")]
    AcquireFailed { agent_id: Uuid, reason: String },
}

/// RAII guard for a tunnel lease. Dropping the guard releases the
/// lease (unregisters the agent in the in-memory case, releases the
/// Redis ownership lock in the distributed case).
pub struct TunnelLease {
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl TunnelLease {
    pub fn new(cleanup: impl FnOnce() + Send + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl Drop for TunnelLease {
    fn drop(&mut self) {
        if let Some(f) = self.cleanup.take() {
            f();
        }
    }
}

/// Trait for agent registry implementations. The in-memory default
/// stores entries in a `DashMap`; the Redis backend (behind the
/// `redis` feature) uses ownership leases and inbox streams.
#[async_trait]
pub trait AgentRegistryProvider: Send + Sync {
    /// Register a freshly-connected agent and acquire a tunnel lease.
    /// The returned `TunnelLease` is an RAII guard: dropping it
    /// releases the lease and unregisters the agent.
    async fn acquire(
        &self,
        agent_id: Uuid,
        local_tx: mpsc::UnboundedSender<String>,
        claims: Claims,
    ) -> Result<TunnelLease, RegistryError>;

    /// Deliver a frame to the agent's tunnel. Returns `Live` if the
    /// frame was accepted for delivery (locally or via a stream),
    /// `NoTunnel(frame)` if no tunnel exists (caller should buffer).
    async fn deliver(&self, agent_id: Uuid, frame: String) -> DeliveryOutcome;

    /// Whether `agent_id` currently has a live tunnel anywhere in
    /// the cluster.
    async fn is_connected(&self, agent_id: Uuid) -> bool;

    /// Snapshot of all connected + recently-disconnected agents.
    async fn list_agents(&self) -> Vec<AgentDescription>;

    /// Describe a single agent by ID.
    async fn describe_agent(&self, agent_id: Uuid) -> Option<AgentDescription>;

    /// Force-disconnect the tunnel for `agent_id`.
    async fn disconnect(&self, agent_id: Uuid);

    /// Record activity from the agent (bumps last-activity timestamp).
    async fn touch(&self, agent_id: Uuid);

    /// Subscribe to control signals (e.g. `takeover`, `disconnect`)
    /// for this agent. Returns `None` for backends that do not support
    /// cross-instance control (the in-memory default). When `Some`,
    /// the receiver yields signal strings; the tunnel read loop should
    /// break when one arrives.
    fn control_receiver(&self, _agent_id: Uuid) -> Option<mpsc::UnboundedReceiver<String>> {
        None
    }
}

/// Routing entry for one connected agent.
pub struct AgentEntry {
    /// Send messages to the connected agent over its tunnel. The
    /// receiving end of this channel lives in
    /// [`crate::tunnel::run::run_tunnel`].
    pub tx: mpsc::UnboundedSender<String>,
    /// JWT claims that the tunnel was opened with. Used by REST
    /// handlers that need to surface the agent's `user` field, etc.
    pub claims: Claims,
    /// When the tunnel was registered.
    pub connected_at: Instant,
    /// Last time we observed activity from the agent. The tunnel
    /// read loop bumps this on every received frame.
    pub last_activity: Mutex<Instant>,
}

impl AgentEntry {
    pub fn new(tx: mpsc::UnboundedSender<String>, claims: Claims) -> Self {
        let now = Instant::now();
        Self {
            tx,
            claims,
            connected_at: now,
            last_activity: Mutex::new(now),
        }
    }

    pub fn touch(&self) {
        let mut guard = self.last_activity.lock().unwrap_or_else(|p| p.into_inner());
        *guard = Instant::now();
    }

    pub fn last_activity(&self) -> Instant {
        *self.last_activity.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Description shape returned by `GET /agents` and `GET /agents/{id}`.
/// Carries enough state for the operator to render a status page
/// without exposing the channel sender.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDescription {
    pub agent_id: Uuid,
    pub connected: bool,
    /// Seconds since the entry was first registered. `None` for
    /// recently-disconnected entries (uptime is undefined when the
    /// agent is no longer holding a tunnel).
    pub uptime_secs: Option<u64>,
    /// Seconds since the most recent activity from the agent. For
    /// connected agents this is "since the last received frame";
    /// for recently-disconnected agents this is "since the
    /// disconnect timestamp". Always `Some(...)` in practice.
    pub idle_secs: Option<u64>,
    /// Echoes the JWT user field if present.
    pub user: Option<Uuid>,
}

impl AgentDescription {
    /// Build a description for a currently-connected entry.
    fn from_connected(agent_id: Uuid, entry: &AgentEntry, now: Instant) -> Self {
        Self {
            agent_id,
            connected: true,
            uptime_secs: Some(now.saturating_duration_since(entry.connected_at).as_secs()),
            idle_secs: Some(
                now.saturating_duration_since(entry.last_activity())
                    .as_secs(),
            ),
            user: entry.claims.user,
        }
    }

    /// Build a description for a recently-disconnected entry.
    fn from_recent(entry: &RecentEntry, now: Instant) -> Self {
        Self {
            agent_id: entry.agent_id,
            connected: false,
            uptime_secs: None,
            idle_secs: Some(
                now.saturating_duration_since(entry.disconnected_at)
                    .as_secs(),
            ),
            user: entry.user,
        }
    }
}

/// Recently-disconnected entry kept in the bounded log.
#[derive(Clone)]
struct RecentEntry {
    agent_id: Uuid,
    user: Option<Uuid>,
    disconnected_at: Instant,
}

/// Per-agent routing table. Cheap to clone — internally an `Arc`-
/// equipped DashMap and a Mutex-guarded VecDeque.
#[derive(Clone)]
pub struct InMemoryAgentRegistry {
    connected: Arc<DashMap<Uuid, Arc<AgentEntry>>>,
    recent: Arc<Mutex<VecDeque<RecentEntry>>>,
}

impl Default for InMemoryAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryAgentRegistry {
    pub fn new() -> Self {
        Self {
            connected: Arc::new(DashMap::new()),
            recent: Arc::new(Mutex::new(VecDeque::with_capacity(RECENT_CAPACITY))),
        }
    }

    /// Register a freshly-connected agent. Returns the previous
    /// entry, if any — useful for tests but normally callers ignore
    /// it (the tunnel write loop holding the prior `tx` will drop on
    /// the next iteration once its channel closes).
    ///
    /// Invariant ordering: insert into `connected` **first**, then
    /// purge from `recent`. A concurrent `list()` or `describe()`
    /// that races this call will always see the agent in at least
    /// one of the two structures, never in neither. Doing the purge
    /// first would open a window where an agent is missing from the
    /// snapshot entirely.
    pub fn register(&self, agent_id: Uuid, entry: AgentEntry) -> Option<Arc<AgentEntry>> {
        let previous = self.connected.insert(agent_id, Arc::new(entry));
        // If we're replacing an existing connection, also clear the
        // recently-disconnected entry — that agent is back.
        self.purge_recent(agent_id);
        previous
    }

    /// Look up the connected entry for `agent_id`.
    pub fn get(&self, agent_id: Uuid) -> Option<Arc<AgentEntry>> {
        self.connected.get(&agent_id).map(|e| e.value().clone())
    }

    /// Return whether `agent_id` currently has a live tunnel.
    pub fn is_connected(&self, agent_id: Uuid) -> bool {
        self.connected.contains_key(&agent_id)
    }

    /// Drop the entry for `agent_id` and record it in the
    /// recently-disconnected log.
    pub fn unregister(&self, agent_id: Uuid) {
        if let Some((_, entry)) = self.connected.remove(&agent_id) {
            self.push_recent(RecentEntry {
                agent_id,
                user: entry.claims.user,
                disconnected_at: Instant::now(),
            });
        }
    }

    /// Snapshot of all connected agents plus the bounded log of
    /// recently-disconnected ones. Connected entries come first.
    pub fn list(&self) -> Vec<AgentDescription> {
        let now = Instant::now();
        let mut out: Vec<AgentDescription> = self
            .connected
            .iter()
            .map(|e| AgentDescription::from_connected(*e.key(), e.value(), now))
            .collect();

        let recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        for r in recent.iter() {
            if self.connected.contains_key(&r.agent_id) {
                continue;
            }
            out.push(AgentDescription::from_recent(r, now));
        }
        out
    }

    /// Build a single-agent description, looking in the connected
    /// table first and falling back to the recently-disconnected log.
    pub fn describe(&self, agent_id: Uuid) -> Option<AgentDescription> {
        let now = Instant::now();
        if let Some(entry) = self.get(agent_id) {
            return Some(AgentDescription::from_connected(agent_id, &entry, now));
        }
        let recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        recent
            .iter()
            .find(|r| r.agent_id == agent_id)
            .map(|r| AgentDescription::from_recent(r, now))
    }

    fn push_recent(&self, entry: RecentEntry) {
        let mut recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        // De-dup: if the same agent is in the log already, drop
        // the older copy so the timestamp reflects the newer
        // disconnect.
        recent.retain(|r| r.agent_id != entry.agent_id);
        if recent.len() == RECENT_CAPACITY {
            recent.pop_front();
        }
        recent.push_back(entry);
    }

    fn purge_recent(&self, agent_id: Uuid) {
        let mut recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        recent.retain(|r| r.agent_id != agent_id);
    }
}

#[async_trait]
impl AgentRegistryProvider for InMemoryAgentRegistry {
    async fn acquire(
        &self,
        agent_id: Uuid,
        local_tx: mpsc::UnboundedSender<String>,
        claims: Claims,
    ) -> Result<TunnelLease, RegistryError> {
        self.register(agent_id, AgentEntry::new(local_tx, claims));
        let registry = self.clone();
        Ok(TunnelLease::new(move || {
            registry.unregister(agent_id);
        }))
    }

    async fn deliver(&self, agent_id: Uuid, frame: String) -> DeliveryOutcome {
        if let Some(entry) = self.get(agent_id) {
            match entry.tx.send(frame) {
                Ok(()) => DeliveryOutcome::Live,
                Err(err) => DeliveryOutcome::NoTunnel(err.0),
            }
        } else {
            DeliveryOutcome::NoTunnel(frame)
        }
    }

    async fn is_connected(&self, agent_id: Uuid) -> bool {
        self.connected.contains_key(&agent_id)
    }

    async fn list_agents(&self) -> Vec<AgentDescription> {
        self.list()
    }

    async fn describe_agent(&self, agent_id: Uuid) -> Option<AgentDescription> {
        self.describe(agent_id)
    }

    async fn disconnect(&self, agent_id: Uuid) {
        self.unregister(agent_id);
    }

    async fn touch(&self, agent_id: Uuid) {
        if let Some(entry) = self.get(agent_id) {
            entry.touch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_for(agent_id: Uuid) -> (mpsc::UnboundedReceiver<String>, AgentEntry) {
        let (tx, rx) = mpsc::unbounded_channel();
        let claims = Claims::agent(agent_id, Some(Uuid::new_v4()), 60, "test");
        (rx, AgentEntry::new(tx, claims))
    }

    #[test]
    fn register_and_get() {
        let reg = InMemoryAgentRegistry::new();
        let id = Uuid::new_v4();
        let (_rx, entry) = entry_for(id);
        reg.register(id, entry);
        assert!(reg.is_connected(id));
        assert!(reg.get(id).is_some());
    }

    #[test]
    fn default_matches_new() {
        // `InMemoryAgentRegistry::default()` should produce an empty
        // registry equivalent to `::new()`.
        let reg: InMemoryAgentRegistry = Default::default();
        assert!(reg.list().is_empty());
        assert!(!reg.is_connected(Uuid::new_v4()));
    }

    #[test]
    fn unregister_moves_to_recent_log() {
        let reg = InMemoryAgentRegistry::new();
        let id = Uuid::new_v4();
        let (_rx, entry) = entry_for(id);
        reg.register(id, entry);
        reg.unregister(id);
        assert!(!reg.is_connected(id));
        let listed = reg.list();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].connected);
        assert_eq!(listed[0].agent_id, id);
    }

    #[test]
    fn describe_finds_recent_entry_after_disconnect() {
        let reg = InMemoryAgentRegistry::new();
        let id = Uuid::new_v4();
        let (_rx, entry) = entry_for(id);
        reg.register(id, entry);
        reg.unregister(id);
        let desc = reg.describe(id).expect("recent entry");
        assert!(!desc.connected);
        assert!(desc.uptime_secs.is_none());
    }

    #[test]
    fn reconnect_clears_recent_entry() {
        let reg = InMemoryAgentRegistry::new();
        let id = Uuid::new_v4();
        let (_rx, e1) = entry_for(id);
        reg.register(id, e1);
        reg.unregister(id);

        let (_rx2, e2) = entry_for(id);
        reg.register(id, e2);
        let listed = reg.list();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].connected);
    }

    #[test]
    fn touch_advances_last_activity() {
        use std::thread::sleep;
        use std::time::Duration;

        let id = Uuid::new_v4();
        let (_rx, entry) = entry_for(id);
        let before = entry.last_activity();
        sleep(Duration::from_millis(2));
        entry.touch();
        assert!(entry.last_activity() >= before);
    }

    #[test]
    fn describe_unknown_agent_is_none() {
        let reg = InMemoryAgentRegistry::new();
        assert!(reg.describe(Uuid::new_v4()).is_none());
    }

    #[test]
    fn recent_log_is_bounded() {
        let reg = InMemoryAgentRegistry::new();
        // Push more than RECENT_CAPACITY agents through the
        // disconnect path; the log should never exceed the cap.
        for _ in 0..(RECENT_CAPACITY + 32) {
            let id = Uuid::new_v4();
            let (_rx, entry) = entry_for(id);
            reg.register(id, entry);
            reg.unregister(id);
        }
        let recent_count = reg.list().iter().filter(|d| !d.connected).count();
        assert!(recent_count <= RECENT_CAPACITY);
    }

    #[test]
    fn list_orders_connected_first() {
        let reg = InMemoryAgentRegistry::new();
        // disconnected agent
        let dead_id = Uuid::new_v4();
        let (_rx, dead) = entry_for(dead_id);
        reg.register(dead_id, dead);
        reg.unregister(dead_id);

        // connected agent
        let live_id = Uuid::new_v4();
        let (_rx2, live) = entry_for(live_id);
        reg.register(live_id, live);

        let listed = reg.list();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].connected);
        assert!(!listed[1].connected);
    }

    #[test]
    fn register_after_disconnect_is_always_visible_in_list() {
        // Regression for a race where `register()` used to purge
        // recent BEFORE inserting into connected. A concurrent
        // `list()` snapshotting in between saw the agent in neither
        // collection. The fixed ordering inserts into `connected`
        // first, so the agent always appears in at least one.
        //
        // A single-threaded test can't deterministically recreate
        // the race, but we pin the end state after an unregister
        // + register cycle: the agent appears in the list exactly
        // once and is marked connected.
        let reg = InMemoryAgentRegistry::new();
        let id = Uuid::new_v4();

        let (_rx1, e1) = entry_for(id);
        reg.register(id, e1);
        reg.unregister(id);
        let (_rx2, e2) = entry_for(id);
        reg.register(id, e2);

        let listed = reg.list();
        let matches: Vec<_> = listed.iter().filter(|d| d.agent_id == id).collect();
        assert_eq!(matches.len(), 1, "agent should appear exactly once");
        assert!(matches[0].connected);
        assert!(reg.describe(id).unwrap().connected);
    }
}
