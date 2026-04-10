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
  `docs/design/controlplane.md` is now marked `Superseded`.
- **Phase 2 (operator hook traits)** — landed. New `hooks` module
  exports `BootProvider`, `ToolHost`, and `QuotaPolicy` trait
  contracts plus stub default implementations
  (`DefaultBootProvider`, `DefaultToolHost`, `DefaultQuotaPolicy`).
  `AppState` now holds `Arc<dyn ...>` for each hook with builder
  methods (`with_boot_provider`, `with_tool_host`,
  `with_quota_policy`) for operator-supplied impls. Defaults are
  installed automatically so the reference server still boots
  end-to-end without an operator stack.
- **Phase 3 (dispatch rewrite)** — landed. Tunnel dispatch now
  delegates `initialize`, `tools/list`, `tools/call`, `quota/check`,
  and `quota/update` to the hooks introduced in Phase 2. The legacy
  `turn/save` request and `poll/newMessages` request are gone;
  `turn/end` is the new fire-and-forget agent → server notification
  for completed turns, and `session/cancel` joins `session/message`
  as a server → agent push. `TunnelContext` carries the hooks and
  `routes::tunnel_upgrade` constructs it from `AppState`. The legacy
  `SessionStore` is no longer touched by the dispatch table.

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
