//! WebSocket tunnel client. Lifted from `overfolder/overlet/src/tunnel.rs`.

use anyhow::{Context, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::StreamExt;
use std::cmp::min;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};
use url::Url;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type WsSink = SplitSink<WsStream, WsMessage>;
pub type WsRead = SplitStream<WsStream>;

const MAX_BACKOFF_SECS: u64 = 30;

/// Open a single WebSocket connection to the controlplane tunnel
/// endpoint, authenticating with a bearer token.
pub async fn connect(url: &str, token: &str) -> Result<(WsRead, WsSink)> {
    let parsed = Url::parse(url).context("invalid tunnel URL")?;
    let host = parsed
        .host_str()
        .context("tunnel URL missing host")?
        .to_string();
    let host_header = match parsed.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };

    let request = Request::builder()
        .uri(url)
        .header("Host", host_header)
        .header("Authorization", format!("Bearer {token}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())
        .context("build WebSocket request")?;

    let (ws, _response) = connect_async(request)
        .await
        .context("WebSocket connect failed")?;

    info!("tunnel connected to {url}");
    let (sink, stream) = ws.split();
    Ok((stream, sink))
}

/// Connect with exponential backoff (1s → 30s, capped). Retries
/// indefinitely; the caller is responsible for choosing when to give
/// up by dropping the future.
pub async fn connect_with_retry(url: &str, token: &str) -> (WsRead, WsSink) {
    let mut attempt: u32 = 0;
    loop {
        match connect(url, token).await {
            Ok(streams) => return streams,
            Err(e) => {
                let backoff = min(1u64 << attempt.min(6), MAX_BACKOFF_SECS);
                warn!(
                    attempt,
                    backoff_secs = backoff,
                    "tunnel connect failed: {e:#}"
                );
                sleep(Duration::from_secs(backoff)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Compute the next backoff value used by `connect_with_retry`.
/// Exposed for tests.
pub fn backoff_secs(attempt: u32) -> u64 {
    min(1u64 << attempt.min(6), MAX_BACKOFF_SECS)
}
