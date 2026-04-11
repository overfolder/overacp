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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_on_each_attempt_until_cap() {
        assert_eq!(backoff_secs(0), 1);
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(2), 4);
        assert_eq!(backoff_secs(3), 8);
        assert_eq!(backoff_secs(4), 16);
    }

    #[test]
    fn backoff_capped_at_max() {
        // 2^5 = 32 > 30 cap.
        assert_eq!(backoff_secs(5), MAX_BACKOFF_SECS);
        assert_eq!(backoff_secs(6), MAX_BACKOFF_SECS);
        // Even a wildly high attempt count stays at the cap.
        assert_eq!(backoff_secs(1_000), MAX_BACKOFF_SECS);
    }

    #[tokio::test]
    async fn connect_fails_fast_against_unreachable_host() {
        // 127.0.0.1:1 is almost certainly not listening. The
        // single-shot `connect` should return Err, not panic.
        let result = connect("ws://127.0.0.1:1/tunnel/x", "dummy-token").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connect_rejects_malformed_url() {
        // `Url::parse` rejects missing scheme; assert we surface
        // that as an error rather than panicking.
        let err = connect("not a url", "tok").await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("tunnel url"));
    }
}
