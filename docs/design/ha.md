---
status: Active
---

# Multi-Instance HA — Redis/Valkey Backend

Design doc for the optional Redis/Valkey-backed implementations of
`AgentRegistryProvider`, `MessageQueueProvider`, and
`StreamBrokerProvider` that enable multi-instance overacp-server
deployments behind a plain (non-sticky) load balancer.

## Motivation

The in-memory defaults (`InMemoryAgentRegistry`, `InMemoryMessageQueue`,
`InMemoryStreamBroker`) keep all routing state in-process. A second
server instance behind the same LB would not know about agents
connected to the first. REST pushes that land on the wrong instance
silently fail. This module solves that by externalizing routing state
to Redis/Valkey.

## Architecture

### Design principles

1. **Symmetric instances.** No leader election. Any instance serves
   any REST request; whichever holds the tunnel is the owner.
2. **Tunnel-holder is owner.** The ownership lock follows the
   WebSocket tunnel, not a pre-computed hash.
3. **Produce unconditionally.** `deliver()` always writes to the
   per-agent inbox stream. The owner's XREADGROUP consumer drains it.
4. **Pub/sub for SSE fan-out.** Broadcast semantics; no durability.
5. **Natural pinning.** The agent_id → instance_id lease is the pin.
   Consistent-hash LB is an optional optimization.

### Keyspace

| Key | Type | TTL | Purpose |
|-----|------|-----|---------|
| `overacp:owner:{agent_id}` | STRING | 30s | Ownership lease. Value = instance_id. |
| `overacp:tunnel:{agent_id}` | HASH | 60s | Tunnel directory (instance_id, connected_at). |
| `overacp:agents:connected` | SET | — | Live agent_id set. |
| `overacp:agents:recent` | ZSET | — | Recently-disconnected log (score = ts_ms, capped at 64). |
| `overacp:agents:claims:{agent_id}` | STRING | 60s | Serialized claims snapshot. |
| `overacp:inbox:{agent_id}` | STREAM | — | Per-agent delivery stream. Consumer group `owners`. |
| `overacp:inbox:dlq` | STREAM | — | Dead-letter queue. |
| `overacp:buffer:{agent_id}` | STREAM | — | Offline message buffer (drained on reconnect). |
| `overacp:stream:{agent_id}` | PUBSUB | — | SSE fan-out channel. |
| `overacp:control:{agent_id}` | PUBSUB | — | Control signals (takeover, disconnect). |

### Ownership lease lifecycle

1. **Acquire** (`SET EX 30`, force — no NX). Publishes `takeover`
   on `overacp:control:{agent_id}` if a different instance holds it.
2. **Heartbeat** (10s interval). Lua CAS `EXPIRE` on the owner key.
   Also refreshes tunnel directory and claims TTL.
3. **Release** (Drop / explicit). Lua CAS `DEL`. Updates connected
   set and recently-disconnected ZSET.

Lua scripts ported from
`overfolder/agent-runner/src/session_lock.rs`.

### Message delivery

`deliver(agent_id, frame)`:
1. `EXISTS overacp:owner:{agent_id}`
2. Owner present → `XADD overacp:inbox:{agent_id} * frame {frame}` → `Live`
3. Owner absent → `NoTunnel(frame)` — caller buffers via `MessageQueueProvider`

Owner's inbox consumer: `XREADGROUP GROUP owners {consumer_id} COUNT 1 STREAMS overacp:inbox:{agent_id} >`.
ACK on successful local delivery. XAUTOCLAIM every ~5 min for orphan
recovery. DLQ on `MAX_DELIVERIES=3` or age > 4h.

### SSE fan-out

`publish(agent_id, frame)` → `PUBLISH overacp:stream:{agent_id} {frame}`.
`subscribe(agent_id)` → dedicated pub/sub connection, returns `BoxStream`.

## Configuration

| Env var | Required | Default | Notes |
|---------|----------|---------|-------|
| `OVERACP_REDIS_URL` | No | — | Enables Redis backend when set. |
| `OVERACP_INSTANCE_ID` | No | hostname / random | Instance identifier for leases. |
| `PORT` | No | `8080` | HTTP listen port. |

## Feature gate

The Redis backend is behind `features = ["redis"]` in
`server/Cargo.toml`. The `redis` crate is not compiled without
the feature; all in-memory tests run without it.

## Relationship to overfolder-on-overacp

Per `overfolder/docs/design/overfolder-on-overacp.md` § "Storage,
Valkey, and Locks", the lock primitives are designed to be
extractable to a shared `valkey-session` crate. For now they live
inside `overacp-server` behind the `redis` feature gate. Overfolder's
controlplane embeds overacp-server as a library and gets HA for free.

## Verification

- `tests/smoke-ha.sh` — two-instance end-to-end test against
  dockerized Valkey. Covers cross-instance push, cross-instance SSE,
  and failover after instance kill.
- Unit tests of in-memory implementations remain the primary safety
  net (150 tests, all pass without the `redis` feature).
