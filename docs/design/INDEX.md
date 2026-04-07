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
- [`controlplane.md`](./controlplane.md) — controlplane architecture:
  the `ComputeProvider` trait, the Kafka-Connect-shaped REST API for
  managing compute pools, agent lifecycle, and the REST adapters
  over the wire protocol. Drives milestone 0.4.
- [`workspace-sync.md`](./workspace-sync.md) — pluggable workspace
  hydration and persistence: where it lives in the architecture
  (agent supervisor, not controlplane), the `WorkspaceSync` trait,
  the per-backend crate convention, and the configuration model.
- [`loop-tools.md`](./loop-tools.md) — how `overloop` discovers and
  invokes tools across the four sources (built-in, supervisor-injected,
  ACP-tunnelled, MCP-direct). Drives the `0.3.x` migration in
  [`TODO.md`](../../TODO.md).
