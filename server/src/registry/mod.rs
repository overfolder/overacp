//! In-memory routing state for connected agents.
//!
//! [`AgentRegistry`] is the broker's source of truth for which
//! agents are currently connected, and gives the REST surface a
//! cheap way to push notifications down their tunnels. It replaces
//! the controlplane-era `SessionManager` (which was keyed on
//! `conversation_id` and held per-poll cursor state) with a
//! per-agent routing table keyed on the JWT `sub` claim.
//!
//! [`MessageQueue`] is a small bounded buffer that holds
//! `session/message` notifications pushed via REST while the
//! corresponding agent's tunnel is disconnected. The tunnel write
//! loop drains the queue on (re)connect before serving live traffic.
//!
//! Neither structure is durable: a broker restart loses both. The
//! operator's REST clients are expected to re-push anything they
//! care about.

pub mod agent;
pub mod queue;

pub use agent::{AgentDescription, AgentEntry, AgentRegistry};
pub use queue::{MessageQueue, QueueError};
