//! WebSocket tunnel — single multiplexed JSON-RPC 2.0 connection per
//! over/ACP session. Lifted from `overfolder/controlplane/src/tunnel.rs`
//! (branch `feat/dev-reborn`) and rewritten to dispatch against
//! `SessionStore` instead of overfolder's bespoke `acp` module.

pub mod broker;
pub mod dispatch;
pub mod run;
pub mod session_manager;

pub use broker::StreamBroker;
pub use dispatch::handle_message;
pub use run::{run_tunnel, TunnelContext};
pub use session_manager::{new_session_manager, SessionManager, TunnelHandle};
