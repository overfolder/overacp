//! Per-agent inbox consumer — drains `overacp:inbox:{agent_id}` via
//! XREADGROUP and forwards frames to the local mpsc sender.
//!
//! One consumer task is spawned per locally-owned agent (started by
//! `RedisAgentRegistry::acquire`, aborted when the `TunnelLease` is
//! dropped). Mirrors the consumer-group pattern from
//! `overfolder/agent-runner/src/triggers/redis_consumer.rs`.

use std::time::Duration;

use redis::aio::ConnectionManager;
use redis::streams::{StreamReadOptions, StreamReadReply};
use redis::AsyncCommands;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, trace, warn};
use uuid::Uuid;

use super::keys::{dlq_key, inbox_key};

/// Consumer group name shared by all instances.
const CONSUMER_GROUP: &str = "owners";

/// How long to sleep between polls when no messages are available.
const IDLE_POLL_MS: u64 = 200;

/// How many poll iterations between autoclaim sweeps (~5 min).
const AUTOCLAIM_INTERVAL: u64 = 1500; // 1500 * 200ms = 300s

/// Minimum idle time before an entry can be auto-claimed (5 min).
const AUTOCLAIM_MIN_IDLE_MS: u64 = 300_000;

/// Max delivery attempts before dead-lettering.
const MAX_DELIVERIES: u64 = 3;

/// Max wall-clock age of a stream entry before dead-lettering (4h).
const MAX_RECLAIM_AGE_MS: u64 = 4 * 3600 * 1000;

/// Ensure the consumer group exists on the inbox stream.
pub async fn ensure_consumer_group(conn: &ConnectionManager, agent_id: Uuid) {
    let key = inbox_key(agent_id);
    let mut c = conn.clone();
    // XGROUP CREATE ... 0 MKSTREAM — idempotent (BUSYGROUP error if
    // the group already exists, which we ignore).
    let _: Result<(), _> = redis::cmd("XGROUP")
        .arg("CREATE")
        .arg(&key)
        .arg(CONSUMER_GROUP)
        .arg("0")
        .arg("MKSTREAM")
        .query_async(&mut c)
        .await;
}

/// Spawn the inbox consumer for `agent_id`. Returns a `JoinHandle`
/// that the caller should abort when the tunnel lease is released.
pub fn spawn_consumer(
    conn: ConnectionManager,
    agent_id: Uuid,
    consumer_id: String,
    local_tx: mpsc::UnboundedSender<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let key = inbox_key(agent_id);
        let mut c = conn;
        let mut poll_count: u64 = 0;

        loop {
            poll_count += 1;

            // ── XREADGROUP ────────────────────────────────────
            let opts = StreamReadOptions::default()
                .group(CONSUMER_GROUP, &consumer_id)
                .count(1);

            let reply: Result<StreamReadReply, _> = c.xread_options(&[&key], &[">"], &opts).await;

            match reply {
                Ok(reply) if !reply.keys.is_empty() => {
                    for stream_key in &reply.keys {
                        for entry in &stream_key.ids {
                            let frame = entry.map.get("frame").and_then(|v| {
                                if let redis::Value::BulkString(bytes) = v {
                                    String::from_utf8(bytes.clone()).ok()
                                } else {
                                    None
                                }
                            });

                            if let Some(frame) = frame {
                                if local_tx.send(frame).is_err() {
                                    // Receiver dropped — tunnel closed. Do NOT ack;
                                    // the autoclaim will handle redelivery or DLQ.
                                    debug!(%agent_id, "inbox consumer: local tx closed");
                                    return;
                                }
                                // ACK on successful delivery.
                                let _: Result<(), _> = redis::cmd("XACK")
                                    .arg(&key)
                                    .arg(CONSUMER_GROUP)
                                    .arg(&entry.id)
                                    .query_async(&mut c)
                                    .await;
                                trace!(%agent_id, id = %entry.id, "inbox: delivered + acked");
                            }
                        }
                    }
                }
                Ok(_) => {
                    // No messages — idle poll.
                    time::sleep(Duration::from_millis(IDLE_POLL_MS)).await;
                }
                Err(e) => {
                    // Transient Redis error — back off.
                    warn!(%agent_id, "inbox XREADGROUP error: {e}");
                    time::sleep(Duration::from_millis(IDLE_POLL_MS)).await;
                }
            }

            // ── Periodic XAUTOCLAIM ──────────────────────────
            if poll_count.is_multiple_of(AUTOCLAIM_INTERVAL) {
                autoclaim_orphaned(&mut c, agent_id, &consumer_id, &key).await;
            }
        }
    })
}

/// Return type of the XAUTOCLAIM Redis command: `(cursor, entries)`.
type AutoclaimResult = (String, Vec<(String, Vec<(String, redis::Value)>)>);

/// Reclaim orphaned entries from dead consumers and either redeliver
/// or dead-letter them.
async fn autoclaim_orphaned(
    conn: &mut ConnectionManager,
    agent_id: Uuid,
    consumer_id: &str,
    stream_key: &str,
) {
    // XAUTOCLAIM <key> <group> <consumer> <min-idle> <start> COUNT 10
    let result: Result<AutoclaimResult, _> = redis::cmd("XAUTOCLAIM")
        .arg(stream_key)
        .arg(CONSUMER_GROUP)
        .arg(consumer_id)
        .arg(AUTOCLAIM_MIN_IDLE_MS)
        .arg("0-0")
        .arg("COUNT")
        .arg(10)
        .query_async(conn)
        .await;

    let (_cursor, entries) = match result {
        Ok(r) => r,
        Err(e) => {
            debug!(%agent_id, "xautoclaim error (may be normal): {e}");
            return;
        }
    };

    for (entry_id, fields) in entries {
        let delivery_count = get_delivery_count(conn, stream_key, &entry_id).await;
        let age_ms = stream_id_age_ms(&entry_id);

        let should_dlq =
            delivery_count >= MAX_DELIVERIES || age_ms.is_some_and(|a| a > MAX_RECLAIM_AGE_MS);

        if should_dlq {
            // Move to DLQ.
            let mut args: Vec<(String, String)> = fields
                .into_iter()
                .filter_map(|(k, v)| {
                    if let redis::Value::BulkString(bytes) = v {
                        Some((k, String::from_utf8_lossy(&bytes).to_string()))
                    } else {
                        None
                    }
                })
                .collect();
            args.push(("dlq_original_id".to_string(), entry_id.clone()));
            args.push(("dlq_delivery_count".to_string(), delivery_count.to_string()));
            args.push(("dlq_agent_id".to_string(), agent_id.to_string()));

            let dlq = dlq_key();
            let mut cmd = redis::cmd("XADD");
            cmd.arg(dlq).arg("*");
            for (k, v) in &args {
                cmd.arg(k).arg(v);
            }
            let _: Result<String, _> = cmd.query_async(conn).await;

            // ACK the original entry.
            let _: Result<(), _> = redis::cmd("XACK")
                .arg(stream_key)
                .arg(CONSUMER_GROUP)
                .arg(&entry_id)
                .query_async(conn)
                .await;

            warn!(
                %agent_id,
                id = %entry_id,
                delivery_count,
                "inbox entry dead-lettered"
            );
        }
        // If not DLQ'd, the entry was auto-claimed to us and will
        // be picked up on the next XREADGROUP with ID ">" or "0".
    }
}

/// Get the delivery count for a specific entry via XPENDING.
async fn get_delivery_count(conn: &mut ConnectionManager, stream_key: &str, entry_id: &str) -> u64 {
    // XPENDING <key> <group> <start> <end> 1
    let result: Result<Vec<(String, String, u64, u64)>, _> = redis::cmd("XPENDING")
        .arg(stream_key)
        .arg(CONSUMER_GROUP)
        .arg(entry_id)
        .arg(entry_id)
        .arg(1)
        .query_async(conn)
        .await;

    match result {
        Ok(entries) if !entries.is_empty() => entries[0].3,
        _ => 1,
    }
}

/// Parse the millisecond timestamp from a stream ID (`{ms}-{seq}`).
fn stream_id_age_ms(id: &str) -> Option<u64> {
    let ms_str = id.split('-').next()?;
    let ms: u64 = ms_str.parse().ok()?;
    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    Some(now_ms.saturating_sub(ms))
}
