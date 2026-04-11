//! WebSocket tunnel — single multiplexed JSON-RPC 2.0 connection per
//! over/ACP agent. Each connected tunnel runs the dispatch loop in
//! [`run::run_tunnel`] against the operator hooks on
//! [`crate::AppState`]; incoming `stream/*`, `turn/end`, and
//! `heartbeat` notifications are fanned out through
//! [`broker::StreamBroker`] to SSE subscribers.

pub mod broker;
pub mod dispatch;
pub mod run;

pub use broker::StreamBroker;
pub use dispatch::handle_message;
pub use run::{run_tunnel, TunnelContext};
