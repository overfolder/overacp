# over/ACP — Status

**Updated:** 2026-04-11

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
- **Phase 4a (AgentRegistry + MessageQueue)** — landed. New
  `server/src/registry/` module exposes `AgentRegistry` (per-agent
  routing table keyed on the JWT `sub` with a bounded
  recently-disconnected log) and `MessageQueue` (bounded per-agent
  buffer of `session/message` frames pushed via REST while the
  agent's tunnel is disconnected). Both are wired into `AppState`
  alongside the legacy `SessionManager`. The tunnel `run_tunnel`
  loop now registers in both tables, drains the queue on
  (re)connect before yielding to the read loop, and unregisters on
  disconnect.
- **Phase 4b (new REST surface)** — landed. `server/src/api/agents.rs`
  is rewritten against `AgentRegistry` and no longer reads from
  `SessionStore`. Every handler in the new surface now takes
  `Path<Uuid>` (the agent_id, matching the JWT `sub`) in place of
  the legacy `Path<String>`:
  `GET /agents`, `GET /agents/{id}`, `DELETE /agents/{id}`,
  `POST /agents/{id}/messages`, `GET /agents/{id}/stream`,
  `POST /agents/{id}/cancel`. Plus the new admin-only minting
  endpoint `POST /tokens` in `api/tokens.rs`.
  `POST /agents/{id}/messages` returns
  `{ delivery: "live" | "queued" }` to distinguish inline tunnel
  push from `MessageQueue` buffering, and 503 on queue overflow.
  JWT middleware in `routes.rs` gates the surface in two tiers:
  `require_admin` for the listing, describe, disconnect, and mint
  routes; `require_agent_or_admin` for the agent-scoped streaming
  routes where an agent JWT with matching `sub` is accepted.
- **Phase 5 (controlplane deletion)** — landed. The legacy
  `SessionStore` trait + `InMemoryStore`, HTTP Basic auth, and the
  entire `/compute/*` REST surface (pools, providers, nodes) have
  been removed from `overacp-server`. `TunnelContext` no longer
  carries `store` or `sessions`; `AppState::new` takes a single
  `Authenticator` argument. The server's dependency on
  `overacp-compute-core` is gone (the crate remains available as
  a standalone library for operators). `SessionManager` and the
  old session-id path segment are deleted outright.

## Crates

| Crate | State |
|---|---|
| `overloop` | Vendored, builds. Reference agent. Still on the controlplane-era wire shape; migration tracked in `TODO.md` § 0.3.x. |
| `overacp-compute-core` | Landed as a standalone library. `ComputeProvider` trait + node/exec/log types + `${provider:path:key}` config resolver. The broker no longer depends on it; operators can pull it in directly. |
| `overacp-protocol` | Landed as a workspace member. Carries the canonical `Claims` shape (`{sub, role, user?, exp, iss}` — matches the broker's `server/src/auth.rs`), method-name constants (including `TURN_END`, `SESSION_CANCEL`, `STREAM_TOOL_CALL`, `STREAM_TOOL_RESULT`), and payload types for every request / notification on the tunnel (`InitializeResponse`, `SessionMessageParams`, `SessionCancelParams`, `TurnEndParams`, stream params, quota params). JWT helpers (`mint_token`, `validate_token`, `peek_claims_unverified`) sit on top of `jsonwebtoken`. Pure types, no tokio. 22 unit + fixture tests. Not yet consumed by `overloop` / `overacp-agent` / `overacp-server` — those follow in later phases. |
| `overacp-agent` | Boot-config crate landed; supervisor + stdio bridge not started. |
| `overacp-server` | Broker refactor complete on `refactor/stateless-broker`: JWT `Claims` + `mint`, operator hooks (`BootProvider`, `ToolHost`, `QuotaPolicy`) with default impls, hook-delegating tunnel dispatch, `AgentRegistry`, `MessageQueue`, and the new REST surface (`POST /tokens`, `GET /agents`, `GET /agents/{id}`, `DELETE /agents/{id}`, `POST /agents/{id}/messages`, `GET /agents/{id}/stream`, `POST /agents/{id}/cancel`) with JWT middleware gating. The legacy `SessionStore`, compute REST surface, and HTTP Basic auth have been removed. |
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
