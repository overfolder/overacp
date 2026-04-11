//! Bidirectional bridge between the WebSocket tunnel and the child
//! agent process.
//!
//! Two concurrent tasks:
//! - `ws_read → child stdin`: forward server messages to the agent.
//! - `child stdout → ws_sink`: forward agent output to the server.
//!
//! If either side closes, the other is shut down via `tokio::select!`.
//!
//! The pump is content-agnostic: JSON-RPC text frames are forwarded
//! one line per frame in both directions without parsing. The OS pipe
//! between supervisor and child is the natural buffer for mid-turn
//! pushes, so the supervisor keeps no in-memory queue.

use anyhow::Result;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};
use tracing::{debug, info};

use crate::tunnel::{WsRead, WsSink};

/// Run the bidirectional bridge until either side closes.
pub async fn run(
    ws_read: WsRead,
    ws_sink: WsSink,
    child_stdin: ChildStdin,
    child_stdout: BufReader<ChildStdout>,
) -> BridgeExit {
    let mut ws_read = ws_read;
    let mut ws_sink = ws_sink;
    let mut child_stdin = child_stdin;
    let mut child_stdout = child_stdout;
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
#[derive(Debug, PartialEq, Eq)]
pub enum BridgeExit {
    /// WebSocket closed (tunnel dropped by the server).
    TunnelClosed,
    /// Child process exited (stdout EOF).
    ProcessExited,
    /// I/O error on either side.
    Error(String),
}

/// Forward text frames from the WebSocket read half into the child's
/// stdin as newline-terminated lines. Generic over any stream of
/// `Result<WsMessage, WsError>` so it can be unit-tested with
/// in-memory streams.
async fn ws_to_stdin<R, W>(ws_read: &mut R, stdin: &mut W) -> BridgeExit
where
    R: Stream<Item = Result<WsMessage, WsError>> + Unpin,
    W: AsyncWrite + Unpin,
{
    while let Some(msg) = ws_read.next().await {
        match msg {
            Ok(WsMessage::Text(text)) => {
                let text_str = text.to_string();
                debug!(len = text_str.len(), "ws→stdin");
                if let Err(e) = write_frame(stdin, &text_str).await {
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

/// Forward lines from the child's stdout into the WebSocket sink as
/// text frames. Generic over any AsyncBufRead + Sink so it can be
/// unit-tested with in-memory buffers.
async fn stdout_to_ws<R, S>(stdout: &mut R, ws_sink: &mut S) -> BridgeExit
where
    R: AsyncBufRead + Unpin,
    S: Sink<WsMessage, Error = WsError> + Unpin,
{
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

/// Write a frame to a generic `AsyncWrite`. Appends a newline if
/// missing and flushes.
async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &str) -> Result<()> {
    writer.write_all(frame.as_bytes()).await?;
    if !frame.ends_with('\n') {
        writer.write_all(b"\n").await?;
    }
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // WsError is owned by tokio-tungstenite
mod tests {
    use super::*;
    use futures_util::stream;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use tokio::io::BufReader;

    // ── Helpers ─────────────────────────────────────────────────

    /// A `Sink<WsMessage>` backed by a `Vec<WsMessage>` plus a flag
    /// to force a send failure.
    #[derive(Default)]
    struct VecSink {
        sent: Arc<Mutex<Vec<WsMessage>>>,
        fail_on_send: bool,
    }

    impl Sink<WsMessage> for VecSink {
        type Error = WsError;

        fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: WsMessage) -> Result<(), Self::Error> {
            if self.fail_on_send {
                return Err(WsError::ConnectionClosed);
            }
            self.sent.lock().unwrap().push(item);
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn text(s: &str) -> Result<WsMessage, WsError> {
        Ok(WsMessage::Text(s.into()))
    }

    // ── ws_to_stdin ─────────────────────────────────────────────

    #[tokio::test]
    async fn ws_to_stdin_forwards_text_frames_with_newline() {
        let frames = vec![text(r#"{"hello":1}"#), text(r#"{"hello":2}"#)];
        let mut src = stream::iter(frames);
        let mut dst: Vec<u8> = Vec::new();
        let exit = ws_to_stdin(&mut src, &mut dst).await;
        assert_eq!(exit, BridgeExit::TunnelClosed);
        let out = String::from_utf8(dst).unwrap();
        assert_eq!(out, "{\"hello\":1}\n{\"hello\":2}\n");
    }

    #[tokio::test]
    async fn ws_to_stdin_returns_tunnel_closed_on_close_frame() {
        let frames: Vec<Result<WsMessage, WsError>> = vec![Ok(WsMessage::Close(None))];
        let mut src = stream::iter(frames);
        let mut dst: Vec<u8> = Vec::new();
        assert_eq!(
            ws_to_stdin(&mut src, &mut dst).await,
            BridgeExit::TunnelClosed
        );
    }

    #[tokio::test]
    async fn ws_to_stdin_ignores_ping_and_binary_frames() {
        let frames = vec![
            Ok(WsMessage::Ping(Default::default())),
            Ok(WsMessage::Binary(vec![1, 2, 3].into())),
            text("after"),
        ];
        let mut src = stream::iter(frames);
        let mut dst: Vec<u8> = Vec::new();
        ws_to_stdin(&mut src, &mut dst).await;
        let out = String::from_utf8(dst).unwrap();
        assert_eq!(out, "after\n");
    }

    #[tokio::test]
    async fn ws_to_stdin_surfaces_error_on_ws_read_failure() {
        let frames: Vec<Result<WsMessage, WsError>> = vec![Err(WsError::ConnectionClosed)];
        let mut src = stream::iter(frames);
        let mut dst: Vec<u8> = Vec::new();
        let exit = ws_to_stdin(&mut src, &mut dst).await;
        match exit {
            BridgeExit::Error(msg) => assert!(msg.to_lowercase().contains("ws read")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── stdout_to_ws ────────────────────────────────────────────

    #[tokio::test]
    async fn stdout_to_ws_forwards_lines_as_text_frames() {
        let input = b"{\"a\":1}\n{\"a\":2}\n";
        let mut reader = BufReader::new(&input[..]);
        let mut sink = VecSink::default();
        let exit = stdout_to_ws(&mut reader, &mut sink).await;
        assert_eq!(exit, BridgeExit::ProcessExited);
        let sent = sink.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        match (&sent[0], &sent[1]) {
            (WsMessage::Text(a), WsMessage::Text(b)) => {
                assert_eq!(a.to_string(), "{\"a\":1}");
                assert_eq!(b.to_string(), "{\"a\":2}");
            }
            other => panic!("expected two text frames, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stdout_to_ws_returns_process_exited_on_eof() {
        let input: &[u8] = b"";
        let mut reader = BufReader::new(input);
        let mut sink = VecSink::default();
        assert_eq!(
            stdout_to_ws(&mut reader, &mut sink).await,
            BridgeExit::ProcessExited
        );
    }

    #[tokio::test]
    async fn stdout_to_ws_surfaces_error_on_sink_failure() {
        let input = b"line\n";
        let mut reader = BufReader::new(&input[..]);
        let mut sink = VecSink {
            sent: Arc::new(Mutex::new(Vec::new())),
            fail_on_send: true,
        };
        let exit = stdout_to_ws(&mut reader, &mut sink).await;
        match exit {
            BridgeExit::Error(msg) => assert!(msg.to_lowercase().contains("ws write")),
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
