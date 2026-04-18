//! `RedisStreamBroker` — Redis pub/sub-backed fan-out for `stream/*`,
//! `turn/end`, and `heartbeat` notifications to SSE subscribers.
//!
//! Uses `PUBLISH` / `SUBSCRIBE` on channel `overacp:stream:{agent_id}`.
//! Each subscriber gets a dedicated pub/sub connection (Redis
//! multiplexed connections can't mix SUBSCRIBE with other commands).

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use redis::aio::ConnectionManager;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::warn;
use uuid::Uuid;

use crate::tunnel::broker::StreamBrokerProvider;

use super::keys::stream_channel;

pub struct RedisStreamBroker {
    conn: ConnectionManager,
    /// Redis URL for creating dedicated subscriber connections.
    redis_url: String,
}

impl RedisStreamBroker {
    pub fn new(conn: ConnectionManager, redis_url: String) -> Self {
        Self { conn, redis_url }
    }
}

#[async_trait]
impl StreamBrokerProvider for RedisStreamBroker {
    async fn publish(&self, agent_id: Uuid, frame: String) {
        let channel = stream_channel(agent_id);
        let mut conn = self.conn.clone();
        let _: Result<(), _> = redis::cmd("PUBLISH")
            .arg(&channel)
            .arg(&frame)
            .query_async(&mut conn)
            .await;
    }

    fn subscribe(&self, agent_id: Uuid) -> BoxStream<'static, String> {
        let channel = stream_channel(agent_id);
        let redis_url = self.redis_url.clone();

        let (tx, rx) = mpsc::unbounded_channel::<String>();

        // Spawn a dedicated pub/sub listener. It runs until the
        // receiver is dropped (which closes the channel via tx).
        tokio::spawn(async move {
            let client = match redis::Client::open(redis_url.as_str()) {
                Ok(c) => c,
                Err(e) => {
                    warn!("redis subscribe client error: {e}");
                    return;
                }
            };
            let mut pubsub = match client.get_async_pubsub().await {
                Ok(ps) => ps,
                Err(e) => {
                    warn!("redis pubsub connect error: {e}");
                    return;
                }
            };
            if let Err(e) = pubsub.subscribe(&channel).await {
                warn!("redis subscribe error: {e}");
                return;
            }

            let mut stream = pubsub.on_message();
            loop {
                tokio::select! {
                    msg = stream.next() => {
                        match msg {
                            Some(msg) => {
                                let payload: String = match msg.get_payload() {
                                    Ok(p) => p,
                                    Err(_) => continue,
                                };
                                if tx.send(payload).is_err() {
                                    // Receiver dropped — SSE client disconnected.
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = tx.closed() => {
                        // Receiver dropped — SSE client disconnected.
                        break;
                    }
                }
            }
        });

        UnboundedReceiverStream::new(rx).boxed()
    }
}
