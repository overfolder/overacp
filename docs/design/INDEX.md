# Design docs

Authoritative design documents for over/ACP. Each doc here is the
source of truth for one subsystem; if a doc and the code disagree,
the doc is wrong and should be updated.

For the high-level architecture and milestone roadmap see
[`SPEC.md`](../../SPEC.md) at the repo root.

## Index

- [`protocol.md`](./protocol.md) — wire protocol: WebSocket transport,
  JWT session claims, JSON-RPC method catalogue, shared payload types,
  schema-discipline rules, and the resolved naming policy. The
  `overacp-protocol` crate is the Rust expression of this document.
- [`controlplane.md`](./controlplane.md) — **superseded.** Historical
  controlplane architecture: `SessionStore`, `ComputeProvider`, and
  the Kafka-Connect-shaped REST API for managing compute pools and
  agent lifecycle. Replaced by the stateless message broker model in
  [`SPEC.md`](../../SPEC.md). Kept for context only.
- [`context-management.md`](./context-management.md) — how
  over/ACP separates the agent's working context from the operator's
  canonical conversation record. Covers auto-compaction, the
  `context/compacted` notification, agent-internal scaffolding, and
  the optional memory-flush hook.
- [`workspace-sync.md`](./workspace-sync.md) — pluggable workspace
  hydration and persistence: where it lives in the architecture
  (agent supervisor, not the broker), the `WorkspaceSync` trait,
  the per-backend crate convention, and the configuration model.
- [`ha.md`](./ha.md) — multi-instance HA via Redis/Valkey: ownership
  leases, inbox streams, pub/sub SSE fan-out, and the `redis` feature
  gate. Optional backend behind `AgentRegistryProvider`,
  `MessageQueueProvider`, and `StreamBrokerProvider` traits.
