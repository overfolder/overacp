//! Bidirectional bridge between the WebSocket tunnel and the child
//! agent process. Lifted from `overfolder/overlet/src/bridge.rs`.
//!
//! Two concurrent tasks:
//! - `ws_read → child stdin`: forward server messages to the agent.
//! - `child stdout → ws_sink`: forward agent output to the server.
//!
//! If either side closes, the other is shut down via `tokio::select!`.

use futures_util::{SinkExt, StreamExt};
use tokio::io::BufReader;
use tokio::process::{ChildStdin, ChildStdout};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info};

use crate::process::write_line;
use crate::tunnel::{WsRead, WsSink};

/// Run the bidirectional bridge until either side closes.
pub async fn run(
    mut ws_read: WsRead,
    mut ws_sink: WsSink,
    mut child_stdin: ChildStdin,
    mut child_stdout: BufReader<ChildStdout>,
) -> BridgeExit {
    tokio::select! {
        reason = ws_to_stdin(&mut ws_read, &mut child_stdin) => {
            info!("ws→stdin closed: {reason:?}");
            reason
        }
        reason = stdout_to_ws(&mut child_stdout, &mut ws_sink) => {
            info!("stdout→ws closed: {reason:?}");
            reason
        }
    }
}

/// Why the bridge exited.
#[derive(Debug)]
pub enum BridgeExit {
    /// WebSocket closed (tunnel dropped by the server).
    TunnelClosed,
    /// Child process exited (stdout EOF).
    ProcessExited,
    /// I/O error on either side.
    Error(String),
}

async fn ws_to_stdin(ws_read: &mut WsRead, stdin: &mut ChildStdin) -> BridgeExit {
    while let Some(msg) = ws_read.next().await {
        match msg {
            Ok(WsMessage::Text(text)) => {
                let text_str = text.to_string();
                debug!(len = text_str.len(), "ws→stdin");
                if let Err(e) = write_line(stdin, &text_str).await {
                    return BridgeExit::Error(format!("write to stdin: {e}"));
                }
            }
            Ok(WsMessage::Close(_)) => return BridgeExit::TunnelClosed,
            Ok(WsMessage::Ping(_)) => {} // tungstenite auto-responds with pong
            Ok(_) => {}                  // ignore binary, pong, etc.
            Err(e) => return BridgeExit::Error(format!("ws read: {e}")),
        }
    }
    BridgeExit::TunnelClosed
}

async fn stdout_to_ws(stdout: &mut BufReader<ChildStdout>, ws_sink: &mut WsSink) -> BridgeExit {
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    loop {
        line.clear();
        match stdout.read_line(&mut line).await {
            Ok(0) => return BridgeExit::ProcessExited,
            Ok(_) => {
                let trimmed = line.trim_end();
                debug!(len = trimmed.len(), "stdout→ws");
                if let Err(e) = ws_sink
                    .send(WsMessage::Text(trimmed.to_string().into()))
                    .await
                {
                    return BridgeExit::Error(format!("ws write: {e}"));
                }
            }
            Err(e) => return BridgeExit::Error(format!("stdout read: {e}")),
        }
    }
}
