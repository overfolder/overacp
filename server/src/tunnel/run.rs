//! `run_tunnel` — read/write loops for a single connected agent.

use std::sync::Arc;
use std::time::Duration;

use std::future::pending;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time;
use tracing::{info, trace, warn};

use crate::auth::Claims;
use crate::hooks::{BootProvider, QuotaPolicy, ToolHost};
use crate::registry::agent::AgentRegistryProvider;
use crate::registry::queue::MessageQueueProvider;
use crate::tunnel::broker::StreamBrokerProvider;
use crate::tunnel::dispatch::handle_message;

const PING_INTERVAL: Duration = Duration::from_secs(20);

/// Context passed to message handlers. Carries the agent's claims,
/// the routing state (`registry`, `stream_broker`,
/// `message_queue`), and the three operator hooks the dispatch
/// table delegates to (`BootProvider`, `ToolHost`, `QuotaPolicy`).
///
/// The fourth hook from the SPEC, `Authenticator`, lives on
/// `AppState` and is consumed by the upgrade handler in
/// [`crate::routes`] before the context is built — by the time
/// dispatch runs the JWT has already been validated.
pub struct TunnelContext {
    pub claims: Claims,
    pub registry: Arc<dyn AgentRegistryProvider>,
    pub message_queue: Arc<dyn MessageQueueProvider>,
    pub stream_broker: Arc<dyn StreamBrokerProvider>,
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

    // Keep a local clone of the sender for dispatch responses and
    // buffered-frame delivery. The original `tx` is moved into the
    // registry via `acquire`.
    let local_tx = tx.clone();

    // Acquire a tunnel lease (registers the agent in the routing
    // table; in Redis mode also takes the ownership lock and starts
    // the heartbeat + inbox consumer).
    let lease = match ctx.registry.acquire(agent_id, tx, claims.clone()).await {
        Ok(lease) => lease,
        Err(e) => {
            warn!(%agent_id, "failed to acquire tunnel lease: {e}");
            return;
        }
    };

    // Drain any session/message pushes that arrived while this
    // agent's tunnel was disconnected. The drain happens before we
    // yield to the read loop so the agent sees the buffered frames
    // first.
    let buffered = ctx.message_queue.drain(agent_id).await;
    if !buffered.is_empty() {
        info!(
            %agent_id,
            count = buffered.len(),
            "draining buffered session/message frames on reconnect"
        );
        for frame in buffered {
            let _ = local_tx.send(frame);
        }
    }

    // Subscribe to control signals (takeover / disconnect) so
    // cross-instance events can close this tunnel.
    let mut control_rx = ctx.registry.control_receiver(agent_id);

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
                    if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Some(()) = ping_rx.recv() => {
                    if ws_tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // Read loop — also listens for control signals so cross-instance
    // takeover / disconnect events can close the tunnel promptly.
    loop {
        tokio::select! {
            ws_msg = ws_rx.next() => {
                let Some(Ok(msg)) = ws_msg else { break; };
                match msg {
                    Message::Text(text) => {
                        trace!(%agent_id, payload = %text, "agent→server");
                        ctx.registry.touch(agent_id).await;

                        // Best-effort fan-out of stream/*, turn/end,
                        // and context/compacted notifications to the
                        // broker so SSE subscribers receive them. Cheap
                        // string sniff to avoid parsing every frame twice.
                        if text.contains("\"stream/")
                            || text.contains("\"turn/end\"")
                            || text.contains("\"context/compacted\"")
                            || text.contains("\"heartbeat\"")
                        {
                            ctx.stream_broker
                                .publish(agent_id, text.to_string())
                                .await;
                        }

                        if let Some(response) = handle_message(&text, &ctx).await {
                            trace!(%agent_id, payload = %response, "server→agent");
                            let _ = local_tx.send(response);
                        }
                    }
                    Message::Close(_) => {
                        info!(%agent_id, "tunnel closed by client");
                        break;
                    }
                    _ => {}
                }
            }
            Some(signal) = recv_control(&mut control_rx) => {
                info!(%agent_id, signal = %signal, "control signal, closing tunnel");
                break;
            }
        }
    }

    // Dropping the lease unregisters the agent (in-memory) or
    // releases the ownership lock (Redis).
    drop(lease);
    ping_task.abort();
    write_task.abort();
    info!(%agent_id, "tunnel disconnected");
}

/// Helper for `tokio::select!`: yields the next control signal, or
/// pends forever when no control receiver is present (in-memory mode).
async fn recv_control(rx: &mut Option<mpsc::UnboundedReceiver<String>>) -> Option<String> {
    match rx {
        Some(rx) => rx.recv().await,
        None => pending().await,
    }
}
