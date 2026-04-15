//! over/ACP agent-side supervisor.
//!
//! `overacp-agent` runs inside the compute environment that hosts a
//! conversation. It opens a single WebSocket tunnel to the over/ACP
//! server, spawns a child agent process (the reference `overloop` or
//! any other `AgentAdapter` implementation), and pipes the protocol
//! traffic between them.
//!
//! See `docs/design/protocol.md` and `SPEC.md` for the full design.

pub mod adapter;
pub mod bridge;
pub mod config;
pub mod process;
pub mod run;
pub mod tunnel;
pub mod workspace;

use std::future::Future;

/// Isolate a future's Sentry hub from the caller's.
///
/// Prevents background tasks spawned via `tokio::spawn` from
/// inheriting an unrelated parent transaction. When the `sentry`
/// feature is disabled this is a pass-through.
#[cfg(feature = "sentry")]
pub fn sentry_isolated<F: Future>(fut: F) -> sentry::SentryFuture<F> {
    use sentry::SentryFutureExt;
    fut.bind_hub(sentry::Hub::new_from_top(sentry::Hub::current()))
}

#[cfg(not(feature = "sentry"))]
pub fn sentry_isolated<F: Future>(fut: F) -> F {
    fut
}

pub use adapter::{AgentAdapter, LoopAdapter};
pub use bridge::{run as run_bridge, BridgeExit};
pub use config::Config;
pub use workspace::{NoopSync, WorkspaceSync};
