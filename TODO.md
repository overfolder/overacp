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
crate landed. Full design in
[`docs/design/loop-tools.md`](./docs/design/loop-tools.md). All items
block calling the loop "protocol-conformant".

- [ ] Loop depends on `overacp-protocol` and consumes its method-name
      constants instead of hard-coding strings in `loop/src/acp.rs`.
- [ ] Loop's `Message` / `InitializeResult` types are replaced by the
      ones in `overacp_protocol::messages`.
- [ ] On `session/message` notification, loop fetches the message body
      via `poll/newMessages` instead of re-running `initialize`.
- [ ] Loop emits `stream/toolCall` and `stream/toolResult`
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

## 0.4 — `overacp-server`: compute-pool controlplane (current)

The centerpiece. Stand up an HTTP + WebSocket service that **owns
compute provisioning, agent lifecycle, and protocol routing**.
Full design in
[`docs/design/controlplane.md`](./docs/design/controlplane.md).

### Persistence + traits

- [ ] `SessionStore` trait covering conversations + messages, plus
      three new tables for compute pools, compute nodes, and agents.
- [ ] Reference impls: in-memory + SQLite. Postgres later.
- [ ] `Authenticator` trait. Reference impl: static JWT (HS256).
- [ ] `QuotaPolicy` trait. Reference impl: no-op (everything allowed).
- [ ] `ToolHost` trait. Reference impl wired to the `tools/list` /
      `tools/call` ACP surface (uses the `overacp-tools-mcp`
      adapter from 0.6 once it lands; until then, in-memory tool
      registration only).

### Compute provisioning

- [ ] `ComputeProvider` trait per `docs/design/controlplane.md` § 4.
- [ ] `ResolvedConfig` + `${provider:path:key}` secret reference
      machinery (`env`, `file`; vault is a future hook).
- [ ] `overacp-compute-local` reference impl: spawns
      `overacp-agent` as a local subprocess. Ships in the same
      milestone so the demo works without infra.
- [ ] `ProviderRegistry` populated at startup. Compile-time feature
      flags for which providers to include.

### REST API surface

Served at the root — no `/api/v{n}` prefix; breaking changes ride
software semver. Modeled on Kafka Connect's connector API. Endpoints
listed in full in `docs/design/controlplane.md` § 3.

- [ ] `GET /compute/providers`,
      `GET /compute/providers/{type}`,
      `POST /compute/providers/{type}/config/validate`.
- [ ] `GET /compute/pools`, `POST /compute/pools`,
      `GET /compute/pools/{name}`,
      `GET|PUT /compute/pools/{name}/config`,
      `DELETE /compute/pools/{name}`,
      `GET /compute/pools/{name}/status`,
      `POST /compute/pools/{name}/{pause,resume}`.
- [ ] `GET /compute/pools/{pool}/nodes`,
      `GET /compute/pools/{pool}/nodes/{id}`,
      `DELETE /compute/pools/{pool}/nodes/{id}`,
      `POST /compute/pools/{pool}/nodes/{id}/exec`,
      `GET /compute/pools/{pool}/nodes/{id}/logs` (SSE).
- [x] `GET /agents`, `POST /agents`, `GET /agents/{id}`,
      `DELETE /agents/{id}`, `GET /agents/{id}/status`. Describe
      response includes `compute = { provider_type, pool, node_id }`.
      Pool runtimes are instantiated lazily on first agent create
      via `ProviderPlugin::instantiate`. JWTs are minted via
      `Authenticator::issue`. Follow-up: gate the whole REST tree
      behind a bearer-token middleware (today none of the REST
      endpoints are authenticated; only the `/tunnel/{id}` upgrade
      validates the JWT). Follow-up: tighten `store::Agent::user`
      from `String` to `Uuid`.
- [ ] REST adapters over the wire protocol:
      `POST /agents/{id}/messages` (enqueues + emits `session/message`),
      `GET /agents/{id}/messages?since=...`,
      `GET /agents/{id}/stream` (SSE fan-out of `stream/*`),
      `POST /agents/{id}/cancel`.

### Tunnel + LLM proxy

- [ ] Lift the WS hub + dispatcher from
      `overfolder/controlplane/src/tunnel.rs` and route methods
      against `SessionStore`/`QuotaPolicy`/`ToolHost`.
- [ ] OpenAI-compatible LLM proxy with auth/model-routing/metering
      hooks.

### Tests

- [ ] Integration test: spin up the server with the `local-process`
      provider, `POST /compute/pools` for a local pool,
      `POST /agents`, `POST /agents/{id}/messages`, assert
      `GET /agents/{id}/stream` produces a `stream/textDelta`.
- [ ] Round-trip tests for every REST endpoint against captured
      JSON fixtures.

## 0.5 — production providers + workspace sync + demo

- [ ] `overacp-compute-docker` — Docker daemon.
- [ ] `overacp-compute-morph` — Morph Cloud, lifted from
      `overfolder/backend/src/routes/workspace.rs`.
- [ ] `overacp-workspace-gcs` — `WorkspaceSync` impl, lifts the
      planned Overfolder GCS work. See
      [`docs/design/workspace-sync.md`](./docs/design/workspace-sync.md).
- [ ] `overacp-workspace-s3` — S3-compatible object store.
- [ ] `overacp-workspace-rclone` — wraps the rclone CLI.
- [ ] `WorkspaceSyncRegistry` in the agent crate, dispatching from
      `OVERACP_WORKSPACE_SYNC` env var or the controlplane-supplied
      descriptor.
- [ ] End-to-end demo: clone repo, `cargo run`, then
      `curl POST /compute/pools` (local), then
      `POST /agents`, then `POST /agents/{id}/messages`,
      observe the SSE stream.

## 0.6 — `overacp-tools-mcp` + Overfolder cutover

- [ ] `overacp-tools-mcp`: `ToolHost` impl that fans out to N MCP
      client connections, namespaces tool names per server, injects
      per-session short-lived credentials.
- [ ] Wire `overacp-tools-mcp` into the controlplane's default
      `ToolHost`.
- [ ] Shrink `overfolder/controlplane` to ~80 LOC of glue: Postgres
      `SessionStore`, Telegram channel, Overslash auth provider.
- [ ] Move Morph integration out of `overfolder/backend` and into
      `overacp-compute-morph` (this repo).
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
