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
