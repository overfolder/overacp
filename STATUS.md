# over/ACP — Status

**Updated:** 2026-04-10

## Current milestone

**Broker refactor (in progress).** The SPEC was rewritten in commits
59c1b29 and bbaab3f to redefine `overacp-server` as a stateless message
broker. The code is mid-migration on `refactor/stateless-broker`:

- **Phase 1 (Claims + Authenticator mint)** — landed. JWT `Claims` now
  carries `{sub, role, user?, exp, iss}`; `Authenticator` gained a
  `mint` method; the tunnel upgrade path validates the `agent` role
  and that the JWT `sub` matches the `<agent_id>` segment of the URL.
  Dispatch handlers that depended on the old `conv` claim
  (`initialize`, `tools/call`, `turn/save`, `poll/newMessages`)
  return a transitional 1503 error pending the operator hooks landing
  in Phase 3. `docs/design/controlplane.md` is now marked
  `Superseded`.

## Crates

| Crate | State |
|---|---|
| `overloop` | Vendored, builds. Reference agent. Still on the controlplane-era wire shape; migration tracked in `TODO.md` § 0.3.x. |
| `overacp-compute-core` | Landed as a standalone library. `ComputeProvider` trait + node/exec/log types + `${provider:path:key}` config resolver. The broker no longer depends on it; operators can pull it in directly. |
| `overacp-protocol` | Not started. |
| `overacp-agent` | Boot-config crate landed; supervisor + stdio bridge not started. |
| `overacp-server` | Mid-refactor on `refactor/stateless-broker`. Authentication and tunnel auth gate are on the new broker shape; operator hooks (`BootProvider`, `ToolHost`, `QuotaPolicy`), `AgentRegistry`, `MessageQueue`, and the new REST surface land in subsequent phases. The legacy `SessionStore`, compute REST surface, and HTTP Basic auth still exist on disk and will be removed in Phase 5. |
| `overacp-tools-mcp` | Not started. |
| `examples/*` | Not started. |

## Decisions locked in

- **MCP injection model:** controlplane-hosted only (case A in SPEC). The
  server runs MCP clients and re-exposes tools via `ToolHost`. Child-process
  MCP injection (case B) is explicitly out of scope.
- **Foreign agent harnesses:** `overacp-agent` will host Claude Code and
  Codex CLI as child processes via existing third-party ACP adapters rather
  than reimplementing translation layers in Rust:
  - Codex → `cola-io/codex-acp` (Rust, Apache-2.0) or
    `zed-industries/codex-acp` (pending LICENSE check).
  - Claude Code → `agentclientprotocol/claude-agent-acp` (TS/Node, spawned
    as subprocess).
  - This pushes a soft constraint on `overacp-protocol`: stay close enough
    to Zed/Anthropic ACP that no middle translation layer is needed.

## Not yet decided

See TODO.md and the "Open questions" section of SPEC.md.
