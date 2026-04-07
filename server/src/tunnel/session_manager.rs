//! Active tunnel session registry.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::auth::Claims;

pub struct TunnelHandle {
    /// Send messages to the connected agent.
    pub tx: mpsc::UnboundedSender<String>,
    /// Claims from the session JWT.
    pub claims: Claims,
    /// Last activity timestamp for idle detection.
    pub last_activity: Instant,
    /// Cursor for `poll/newMessages` — id of the last message we returned.
    pub poll_cursor: Mutex<Option<Uuid>>,
}

pub type SessionManager = Arc<DashMap<Uuid, TunnelHandle>>;

pub fn new_session_manager() -> SessionManager {
    Arc::new(DashMap::new())
}
