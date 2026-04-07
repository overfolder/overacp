# over/ACP — Status

**Updated:** 2026-04-07

## Current milestone

**0.4 — `overacp-server`** (next)

Milestones 0.1, 0.2, and 0.3 have landed. The repo now hosts the
vendored reference agent, the wire-protocol crate, and the agent-side
WS supervisor. Next up is extracting the server from
`overfolder/controlplane`.

## Crates

| Crate | State |
|---|---|
| `overloop` | Vendored, builds. Reference agent. |
| `overacp-protocol` | Landed (0.2). Pure types, JWT, methods, fixtures. |
| `overacp-agent` | Landed (0.3). WS supervisor + AgentAdapter trait. |
| `overacp-server` | Not started. |
| `overacp-tools-mcp` | Not started. |
| `examples/*` | Not started. |

## Decisions locked in

- **MCP injection model:** controlplane-hosted only (case A in SPEC). The
  server runs MCP clients and re-exposes tools via `ToolHost`. Child-process
  MCP injection (case B) is explicitly out of scope.
- **Foreign agent harnesses:** `overacp-agent` hosts Claude Code and
  Codex CLI as child processes via existing third-party ACP adapters
  rather than reimplementing translation layers in Rust:
  - Codex → `cola-io/codex-acp` (Rust, Apache-2.0) or
    `zed-industries/codex-acp` (pending LICENSE check).
  - Claude Code → `agentclientprotocol/claude-agent-acp` (TS/Node, spawned
    as subprocess).
  - The `AgentAdapter` trait exists in `overacp-agent` 0.3; the
    Claude/Codex impls are deferred.
- **No tier or billing claims in the protocol.** over/ACP is OSS;
  per-user policy lives in deployments, keyed on the `user` UUID, not
  in `Claims`. See `docs/design/protocol.md` § 2.1.
- **Protocol naming policy.** ACP names where they overlap (`initialize`),
  MCP names for tools (`tools/list`, `tools/call`), over/ACP-specific
  names for everything else. Resolved in `docs/design/protocol.md` § 7.

## Not yet decided

See TODO.md and the "Open questions" section of SPEC.md.
