//! `run_tunnel` — read/write loops for a single connected agent.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};
use tokio::time;
use tracing::info;

use crate::auth::Claims;
use crate::hooks::{BootProvider, QuotaPolicy, ToolHost};
use crate::registry::{AgentEntry, AgentRegistry, MessageQueue};
use crate::store::SessionStore;
use crate::tunnel::broker::StreamBroker;
use crate::tunnel::dispatch::handle_message;
use crate::tunnel::session_manager::{SessionManager, TunnelHandle};

const PING_INTERVAL: Duration = Duration::from_secs(20);

/// Context passed to message handlers. Carries the agent's claims,
/// the in-memory routing state (`sessions`, `stream_broker`), and
/// the three operator hooks the dispatch table delegates to
/// (`BootProvider`, `ToolHost`, `QuotaPolicy`).
///
/// The fourth hook from the SPEC, `Authenticator`, lives on
/// `AppState` and is consumed by the upgrade handler in
/// [`crate::routes`] before the context is built — by the time
/// dispatch runs the JWT has already been validated.
///
/// `store` is here only because the legacy `/agents/{id}/...` REST
/// surface (which still uses `SessionStore`) hangs off `AppState`
/// during the broker refactor. The tunnel itself does not read it.
pub struct TunnelContext {
    pub claims: Claims,
    pub store: Arc<dyn SessionStore>,
    pub sessions: SessionManager,
    pub registry: AgentRegistry,
    pub message_queue: MessageQueue,
    pub stream_broker: Arc<StreamBroker>,
    pub boot_provider: Arc<dyn BootProvider>,
    pub tool_host: Arc<dyn ToolHost>,
    pub quota_policy: Arc<dyn QuotaPolicy>,
}

/// Run the tunnel for a connected WebSocket. Spawns ping + write tasks
/// and runs the read loop on the current task; returns when the socket
/// closes.
pub async fn run_tunnel(ws: WebSocket, claims: Claims, ctx: Arc<TunnelContext>) {
    let agent_id = claims.sub;
    let (mut ws_tx, mut ws_rx) = ws.split();

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Register in both the legacy session table (still used by the
    // controlplane-era /agents/{id}/... REST surface) and the new
    // agent registry. Phase 4b drops the legacy half.
    ctx.sessions.insert(
        agent_id,
        TunnelHandle {
            tx: tx.clone(),
            claims: claims.clone(),
            last_activity: Instant::now(),
            poll_cursor: Mutex::new(None),
        },
    );
    ctx.registry
        .register(agent_id, AgentEntry::new(tx, claims.clone()));

    // Drain any session/message pushes that arrived while this
    // agent's tunnel was disconnected. The drain happens before we
    // yield to the read loop so the agent sees the buffered frames
    // first.
    let buffered = ctx.message_queue.drain(agent_id);
    if !buffered.is_empty() {
        info!(
            %agent_id,
            count = buffered.len(),
            "draining buffered session/message frames on reconnect"
        );
        if let Some(entry) = ctx.registry.get(agent_id) {
            for frame in buffered {
                let _ = entry.tx.send(frame);
            }
        }
    }

    info!(%agent_id, role = %claims.role, "tunnel connected");

    // Periodic WS ping. Long-poll proxies (e.g. cloudflared) close
    // idle WebSockets after ~100s, so 20s gives plenty of headroom.
    let (ping_tx, mut ping_rx) = mpsc::unbounded_channel::<()>();
    let ping_task = tokio::spawn(async move {
        let mut interval = time::interval(PING_INTERVAL);
        interval.tick().await; // skip immediate first tick
        loop {
            interval.tick().await;
            if ping_tx.send(()).is_err() {
                break;
            }
        }
    });

    // Write loop.
    let write_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    if ws_tx.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                Some(()) = ping_rx.recv() => {
                    if ws_tx.send(Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // Read loop.
    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => {
                if let Some(mut handle) = ctx.sessions.get_mut(&agent_id) {
                    handle.last_activity = Instant::now();
                }
                if let Some(entry) = ctx.registry.get(agent_id) {
                    entry.touch();
                }

                // Best-effort fan-out of stream/* and turn/end
                // notifications to the in-memory broker so SSE
                // subscribers receive them. Cheap string sniff to
                // avoid parsing every frame twice.
                if text.contains("\"stream/")
                    || text.contains("\"turn/end\"")
                    || text.contains("\"heartbeat\"")
                {
                    let sender = ctx.stream_broker.sender_for(agent_id);
                    let _ = sender.send(text.clone());
                }

                if let Some(response) = handle_message(&text, &ctx).await {
                    if let Some(handle) = ctx.sessions.get(&agent_id) {
                        let _ = handle.tx.send(response);
                    }
                }
            }
            Message::Close(_) => {
                info!(%agent_id, "tunnel closed by client");
                break;
            }
            _ => {}
        }
    }

    ctx.sessions.remove(&agent_id);
    ctx.registry.unregister(agent_id);
    ping_task.abort();
    write_task.abort();
    info!(%agent_id, "tunnel disconnected");
}
