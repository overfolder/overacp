# over/ACP — TODO

Tracks concrete next steps. High-level roadmap lives in
[`SPEC.md`](./SPEC.md); subsystem designs live under
[`docs/design/`](./docs/design).

## 0.1 — vendor `loop`

- [x] Set up workspace `Cargo.toml` at repo root.
- [x] CI: fmt, clippy, test on stable.
- [x] Apache-2.0 LICENSE + NOTICE file at repo root.
- [x] README pointing at SPEC.md.

## 0.2 — `overacp-protocol`

- [x] Extract wire types from `overfolder/controlplane/src/{acp,session}.rs`.
- [x] Pure types, no I/O, no tokio.
- [x] Decide method naming: borrow Zed/Anthropic ACP names where they fit,
      MCP names for `tools/*`, over/ACP for the rest.
      *(see `docs/design/protocol.md`)*
- [x] JWT claims helpers. No `tier` claim — over/ACP is OSS and does
      not encode billing.
- [x] Round-trip tests against captured fixtures.

## 0.3 — `overacp-agent`

- [x] Lift `overfolder/overlet` into the workspace.
- [x] WS client + reconnect/backoff.
- [x] Child-process supervisor + stdio bridge.
- [x] `AgentAdapter` trait so the supervised child can be any
      ACP-speaking harness.
- [x] Built-in adapters:
  - [x] `loop` — identity passthrough to `overloop`.
  - [ ] `claude-code` — spawn `agentclientprotocol/claude-agent-acp` as a
        Node subprocess. Document Node version requirement. *(deferred)*
  - [ ] `codex` — depend on `cola-io/codex-acp` (verify Apache-2.0 still
        current) OR vendor `zed-industries/codex-acp` after LICENSE check.
        *(deferred)*
- [x] `WorkspaceSync` trait + `NoopSync` default impl.

## 0.3.x — `overloop` migration to the protocol crate

Tracked separately because the gaps surfaced after the protocol
crate landed. All items block calling the loop "protocol-conformant".

- [x] Loop depends on `overacp-protocol` and consumes its method-name
      constants instead of hard-coding strings in `loop/src/acp.rs`.
- [x] Loop consumes `overacp_protocol::messages` payload types for
      outbound notifications (`TextDelta`, `Activity`, `TurnEndParams`,
      `QuotaUpdateRequest`) and for inbound `SessionMessageParams`.
      *(Note: `llm::Message` stays local because it carries richer
      tool-call typing the LLM client needs; the wire is bridged at
      the `AcpClient::turn_end` boundary via a serde round-trip
      into `protocol::Message`. `AcpClient::initialize` returns a
      local `InitializeResult` struct that parses directly into the
      LLM-facing shape rather than `protocol::InitializeResponse`,
      because the LLM-facing `Message` is the consumer.)*
- [x] Loop reads the message body inline from the `session/message`
      notification's `params` field instead of polling. The
      `poll/newMessages` method has been removed from the protocol.
- [x] Loop emits `turn/end` (fire-and-forget notification) at the
      end of each turn instead of the old `turn/save` request.
- [x] Loop emits `stream/toolCall` and `stream/toolResult`
      notifications around every tool invocation.
- [ ] **Tool sources unified into one registry**, ordered:
      built-in → supervisor-injected → ACP-tunnelled → MCP-direct.
      Names are namespaced per source.
  - [ ] `tools_config` typed struct in `overacp-protocol`
        (replaces the opaque `Value` field on `InitializeResponse`).
  - [ ] Loop reads `OVERACP_INJECTED_TOOLS` (env var) or a first-line
        stdio handshake from the supervisor and registers stdio/HTTP
        MCP descriptors under `injected/`.
  - [ ] Loop calls `tools/list` over the ACP tunnel when
        `tools_config.acp_tools_enabled` is set, registers under
        `acp/`, routes invocations through `tools/call`.
  - [ ] Loop spins up one MCP client per
        `tools_config.mcp_servers` descriptor, registers under
        `mcp/<name>/`. Drop the legacy `MCP_SERVERS` env var.
  - [ ] New `loop/src/mcp/stdio_client.rs` for stdio-transport MCP.
- [ ] `AgentAdapter` trait gains an `injected_tools()` hook so
      deployment adapters can supply tool descriptors without
      touching the supervisor.

## 0.4 — `overacp-server`: stateless message broker (landed)

The centerpiece. A small HTTP + WebSocket router that terminates
agent tunnels and delegates substance to four operator hooks.
Compute provisioning, persistence, and agent lifecycle are
**out of scope** for the broker — see `SPEC.md`.

### Operator hooks

- [x] `BootProvider` trait. Reference impl: `DefaultBootProvider`
      (empty bootstrap). Backs `initialize`.
- [x] `ToolHost` trait. Reference impl: `DefaultToolHost` (empty
      catalogue, `call` returns `NotFound`). The `overacp-tools-mcp`
      MCP adapter from 0.6 will be a drop-in replacement.
- [x] `QuotaPolicy` trait. Reference impl: `DefaultQuotaPolicy`
      (no-op, everything allowed).
- [x] `Authenticator` trait with `validate`, `mint`, `issuer`.
      Reference impl: `StaticJwtAuthenticator` (HS256 against a
      static signing key).

### In-memory routing state

- [x] `AgentRegistry` — per-agent routing table keyed on the JWT
      `sub`, with a bounded recently-disconnected log.
- [x] `MessageQueue` — bounded per-agent buffer for
      `session/message` pushes that arrive while the tunnel is
      disconnected. Drained on reconnect.
- [x] `StreamBroker` — in-memory broadcast fan-out of `stream/*`
      and `turn/end` frames to SSE subscribers.

### Tunnel + dispatch

- [x] WebSocket tunnel at `/tunnel/:agent_id`, JWT-gated on the
      agent role with `sub == agent_id`.
- [x] JSON-RPC 2.0 dispatch that delegates `initialize`,
      `tools/list`, `tools/call`, `quota/check`, and
      `quota/update` to the operator hooks.
- [x] `turn/end` fire-and-forget notification (replacing the old
      request-shaped `turn/save`), `session/cancel`, and
      inline `session/message` body delivery (no `poll/newMessages`).

### REST surface

Served at the root — no `/api/v{n}` prefix; breaking changes ride
software semver.

- [x] `POST /tokens` (admin-only) — mint agent JWTs via the
      `Authenticator` hook.
- [x] `GET /agents` (admin-only) — list connected + recently
      disconnected agents.
- [x] `GET /agents/{id}` (admin-only) — describe one agent.
- [x] `DELETE /agents/{id}` (admin-only) — force-disconnect.
- [x] `POST /agents/{id}/messages` (admin or scoped agent) —
      push `session/message`, buffer if disconnected, 503 on
      per-agent queue overflow.
- [x] `GET /agents/{id}/stream` (admin or scoped agent) — SSE
      fan-out.
- [x] `POST /agents/{id}/cancel` (admin or scoped agent) —
      inject `session/cancel`.
- [x] JWT auth middleware in `routes.rs`: `require_admin` for the
      first four endpoints, `require_agent_or_admin` for the
      streaming endpoints (accepts an agent JWT whose `sub`
      matches the path `{id}`).

### 0.4 non-blockers (deferred)

Tracked here so they don't get lost. None of them block the
broker itself.

- [ ] Agent JWT rotation strategy. Currently mints once with a
      30-day TTL per `docs/design/protocol.md` § 2.4 and never
      refreshes.
- [ ] Persistent `MessageQueue` for multi-replica deployments
      (Redis stream, NATS jetstream). Current in-memory queue
      survives reconnects but not broker restarts.
- [ ] Streaming completions (multiple `stream/textDelta` per turn,
      proper turn terminator framing).

## 0.5 — workspace sync + demo

Compute provisioning has moved out of the broker's scope. The
`overacp-compute-core` crate remains a standalone library that
operators pull in if they want a ready-made `ComputeProvider`
abstraction; Docker/Morph backends are operator territory.

- [ ] `overacp-workspace-gcs` — `WorkspaceSync` impl for Google
      Cloud Storage. See
      [`docs/design/workspace-sync.md`](./docs/design/workspace-sync.md).
- [ ] `overacp-workspace-s3` — S3-compatible object store.
- [ ] `overacp-workspace-rclone` — wraps the rclone CLI.
- [ ] `WorkspaceSyncRegistry` in the agent crate, dispatching from
      `OVERACP_WORKSPACE_SYNC` env var.
- [x] End-to-end demo: clone repo, `cargo run`, mint an admin JWT,
      `POST /tokens` for an agent, `POST /agents/{id}/messages`,
      observe the SSE stream. *(Landed as `tests/smoke-e2e.sh` —
      requires `LLM_API_KEY` in `.env`.)*

## 0.6 — `overacp-tools-mcp` + Overfolder cutover

- [ ] `overacp-tools-mcp`: `ToolHost` impl that fans out to N MCP
      client connections, namespaces tool names per server, injects
      per-session short-lived credentials.
- [ ] Wire `overacp-tools-mcp` as the operator's `ToolHost`
      implementation in their own backend.
- [ ] Shrink `overfolder/controlplane` to glue code: its own
      persistence layer (whatever store it chooses) behind a
      `BootProvider` impl, Telegram channel, Overslash auth
      provider.
- [ ] Archive `overfolder/overloop`.

## 1.0 — protocol + REST freeze

- [ ] Semver guarantees on `overacp-protocol`.
- [ ] Semver guarantees on the v1 REST surface.
- [ ] Migration guide for any wire-format breaking changes
      accumulated during 0.x.

## Cross-cutting / open questions

- [ ] Verify `zed-industries/codex-acp` LICENSE (GitHub reports `NOASSERTION`).
- [ ] Worked example for multi-agent (parent agent spawns sub-agent
      on a new compute node via `ComputeProvider` from inside a tool
      call, not a special protocol method).
- [ ] Streaming exec on compute nodes — design the wire shape
      (SSE vs second WebSocket vs piggyback on the tunnel).
- [ ] Cross-pool scheduling / fair-share. Out of scope for 1.0.
- [x] Document the protocol-naming mapping table once 0.2 lands.
      *(landed in `docs/design/protocol.md`)*
