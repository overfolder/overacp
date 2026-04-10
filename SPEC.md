# over/ACP — message broker for remote agents

**Status:** Draft (broker refactor, 2026-04-09)
**Repo:** github.com/overfolder/overacp
**License:** Apache-2.0
**Historical:** see [`SPEC_OLD.md`](./SPEC_OLD.md) for the prior
controlplane-shaped design this document supersedes.

## What this is

over/ACP is a small framework for **dispatching messages to LLM
agents that run somewhere else**. Its core is a **stateless message
broker**: agents connect over a single multiplexed WebSocket tunnel
and the broker routes user messages, tool calls, and streaming output
between the agents and an external system that owns everything
durable.

over/ACP does not manage compute, store conversations, or own agent
identity. Those are jobs for whichever system wraps it.

## What over/ACP does

- **Terminates agent tunnels.** One WebSocket per agent, JSON-RPC 2.0
  on the wire, JWT bearer auth on upgrade.
- **Routes user messages to agents.** `POST /agents/{id}/messages`
  pushes a message frame down the agent's tunnel as a `session/message`
  notification. Body travels in the notification — no poll round-trip.
- **Fans out streaming output.** `GET /agents/{id}/stream` is an SSE
  feed of `stream/textDelta`, `stream/toolCall`, `stream/toolResult`,
  and `turn/end` notifications emitted by the agent.
- **Routes tool calls.** Agents call `tools/list` / `tools/call` over
  the tunnel; the broker dispatches them through a pluggable
  `ToolHost` (typically a controlplane-hosted MCP fan-out implemented
  by the operator).
- **Bootstraps agents.** On `initialize` the broker delegates to a
  `BootProvider` hook supplied by the operator, which returns the
  system prompt + recent message window. The broker stores nothing.
- **Enforces quota.** `quota/check` and `quota/update` delegate to a
  `QuotaPolicy` hook.
- **Authenticates.** Pluggable `Authenticator` trait. Reference impl
  is HS256 JWT against a static signing key.
- **Buffers pushes for disconnected agents.** A small bounded
  in-memory `MessageQueue` per agent holds `session/message` pushes
  that arrive while the tunnel is down; on reconnect the queue is
  drained before normal traffic resumes.

## What over/ACP does NOT do

- **No persistence.** No conversation store, no message table, no
  `SessionStore` trait. The broker keeps only in-memory routing
  state. The operator owns durable storage.
- **No compute provisioning.** over/ACP does not start, stop, scale,
  or schedule the environments where agents run. An external
  orchestrator launches compute and points the agent at the broker
  via `OVERACP_TUNNEL_URL` and `OVERACP_JWT`.
- **No agent enrollment API.** Connecting with a valid JWT *is* the
  enrollment. There is no `POST /agents`.
- **No identity hierarchy.** No tier, plan, or entitlement claim in
  the JWT. Whatever identity model the operator wants lives outside.
- **No tool registry.** `ToolHost` is a trait. Tools come from
  whatever the operator plugs in (MCP fan-out, custom registry, ...).
- **No workspace sync.** Workspace hydration is the agent's
  responsibility, configured outside the protocol.
- **No channels.** Telegram, Slack, web UI, voice — all jobs for the
  layer that drives the broker over REST.
- **No durable resume state.** "Resume agent X" means cold-starting
  the agent, which calls `BootProvider::initialize`; the operator's
  hook decides what resumed state to return.

## Crates

| Crate | What |
|---|---|
| `overacp-protocol` | Pure wire types, method-name constants, JWT claim helpers. No I/O, no tokio. |
| `overacp-agent` | Supervisor that holds the WebSocket and bridges JSON-RPC to a child process's stdio. `AgentAdapter` trait so the supervised child can be any ACP-speaking harness. |
| `overacp-server` | The broker. Stateless tunnel terminator + REST adapters + the four pluggable hooks (`BootProvider`, `ToolHost`, `QuotaPolicy`, `Authenticator`). |
| **`overloop`** | Reference child agent: minimal agentic loop with built-in tools and an OpenAI-compatible LLM client. Speaks the protocol on stdio. |

Compute providers, workspace-sync backends, MCP `ToolHost`
implementations, and storage adapters live **outside** these four
crates — either as separate crates the operator pulls in, or in the
operator's own codebase.

## Architecture

```
┌─────────────────────────── trusted side ─────────────────────────────┐
│                                                                       │
│   ┌─────────────────────────────────────────────────────────────┐    │
│   │  External system (operator's process)                       │    │
│   │  ── owns durable conversation/user/quota state              │    │
│   │  ── implements BootProvider, ToolHost, QuotaPolicy          │    │
│   │  ── drives the broker via REST                              │    │
│   │                                                             │    │
│   │  ┌───────────────────────────────────────────────────────┐ │    │
│   │  │  overacp-server  (in-process or sidecar)              │ │    │
│   │  │  ── stateless: AgentRegistry + MessageQueue +         │ │    │
│   │  │     StreamBroker, all in-memory                       │ │    │
│   │  │  ── invokes the operator's hook impls per request     │ │    │
│   │  └─────────────────────┬─────────────────────────────────┘ │    │
│   └────────────────────────┼───────────────────────────────────┘    │
│                            │                                         │
│                            │  WebSocket tunnel                        │
│                            │  JSON-RPC 2.0, JWT bearer                │
│                            │                                         │
└────────────────────────────┼─────────────────────────────────────────┘
                             │
┌────────────────────────────┼──────── untrusted side ─────────────────┐
│                            │                                         │
│   ┌────────────────────────▼─────────────────────────────────┐      │
│   │  Compute environment (VM, container, laptop, ...)        │      │
│   │  ── launched by external orchestrator                    │      │
│   │  ── only knows OVERACP_TUNNEL_URL + OVERACP_JWT          │      │
│   │  ── no DB credentials, no operator secrets               │      │
│   │                                                          │      │
│   │  ┌────────────────────────────────────────────────────┐ │      │
│   │  │  overacp-agent (supervisor)                        │ │      │
│   │  │  ── opens the WebSocket, reconnects on drop        │ │      │
│   │  │  ── bridges WS frames ↔ child stdio                │ │      │
│   │  └─────────────────────┬──────────────────────────────┘ │      │
│   │                        │                                │      │
│   │  ┌─────────────────────▼──────────────────────────────┐ │      │
│   │  │  overloop (or any ACP-speaking child)              │ │      │
│   │  │  ── runs the agentic loop                          │ │      │
│   │  │  ── holds the working conversation in memory       │ │      │
│   │  │  ── refetches via `initialize` on cold start       │ │      │
│   │  └────────────────────────────────────────────────────┘ │      │
│   └──────────────────────────────────────────────────────────┘      │
└──────────────────────────────────────────────────────────────────────┘
```

The trust boundary cuts through the WebSocket. Hook implementations
run on the trusted side with whatever credentials the operator gives
them; the agent VM only ever sees JSON the broker sends back through
the tunnel.

## Wire protocol summary

Full spec in [`docs/design/protocol.md`](./docs/design/protocol.md).

A single multiplexed WebSocket tunnel per agent, JSON-RPC 2.0 frames,
JWT in the `Authorization: Bearer` header on upgrade.

JWT claims:

| Field | Meaning |
|---|---|
| `sub`  | agent_id (also the routing key) |
| `user` | optional opaque user identifier (operator-defined) |
| `exp`  | expiry |
| `iss`  | issuer |

Method catalogue:

| Method              | Origin     | Direction       | Kind         | Handled by |
|---------------------|------------|-----------------|--------------|------------|
| `initialize`        | ACP        | agent → server  | request      | `BootProvider::initialize` |
| `session/message`   | extension  | server → agent  | notification | (push from REST; payload carries the body) |
| `session/cancel`    | extension  | server → agent  | notification | (push from `POST /agents/{id}/cancel`) |
| `tools/list`        | MCP        | agent → server  | request      | `ToolHost::list` |
| `tools/call`        | MCP        | agent → server  | request      | `ToolHost::call` |
| `quota/check`       | extension  | agent → server  | request      | `QuotaPolicy::check` |
| `quota/update`      | extension  | agent → server  | request      | `QuotaPolicy::record` |
| `stream/textDelta`  | extension  | agent → server  | notification | broker fan-out → SSE |
| `stream/activity`   | extension  | agent → server  | notification | broker fan-out → SSE |
| `stream/toolCall`   | extension  | agent → server  | notification | broker fan-out → SSE |
| `stream/toolResult` | extension  | agent → server  | notification | broker fan-out → SSE |
| `turn/end`          | extension  | agent → server  | notification | broker fan-out → SSE (operator persists) |
| `heartbeat`         | extension  | agent → server  | notification | registry liveness ping |

Method-name origin policy: borrow from upstream where possible.
`initialize` is from Zed/Anthropic ACP. `tools/list` and `tools/call`
are from MCP. The rest are over/ACP extensions and have no upstream
equivalent. Per-method classification lives in the protocol doc.

## REST surface

Agent-facing routes (`/tunnel/{id}`, `/agents/{id}/messages`,
`/agents/{id}/stream`, `/agents/{id}/cancel`) authenticate with the
same JWT as the tunnel. Operator-facing routes (`GET /agents`,
`GET /agents/{id}`, `DELETE /agents/{id}`) authenticate with whatever
the `Authenticator` impl accepts; the reference deployment uses HTTP
Basic backed by an htpasswd file (bcrypt only).

```
POST   /agents/{id}/messages    push a message (queued if disconnected)
GET    /agents/{id}/stream      SSE feed of stream/* and turn/end
POST   /agents/{id}/cancel      inject a session/cancel notification
GET    /agents/{id}             describe — connection state, last activity, claims
GET    /agents                  list connected (and recently disconnected) agents
DELETE /agents/{id}             force-disconnect the tunnel
```

There is intentionally no `POST /agents`. Agents enroll implicitly on
their first connect with a valid JWT.

## The four hooks

over/ACP-server is a router. The substance behind every dispatched
method comes from a trait the operator implements:

```rust
#[async_trait]
pub trait BootProvider: Send + Sync {
    async fn initialize(&self, claims: &Claims) -> Result<InitializeResponse, BootError>;
}

#[async_trait]
pub trait ToolHost: Send + Sync {
    async fn list(&self, claims: &Claims) -> Result<ToolList, ToolError>;
    async fn call(&self, claims: &Claims, req: ToolCall) -> Result<ToolResult, ToolError>;
}

#[async_trait]
pub trait QuotaPolicy: Send + Sync {
    async fn check(&self, claims: &Claims) -> Result<bool, QuotaError>;
    async fn record(&self, claims: &Claims, usage: Usage) -> Result<(), QuotaError>;
}

pub trait Authenticator: Send + Sync {
    fn validate(&self, token: &str) -> Result<Claims, AuthError>;
}
```

Default implementations: `BootProvider` returns an empty bootstrap
(no system prompt, no history); `ToolHost` returns no tools;
`QuotaPolicy` allows everything; `Authenticator` validates HS256 JWTs
against a static signing key. The reference server boots with all
four defaults so `cargo run` works out of the box and end-to-end
demos don't need an operator stack.

## What lives outside

over/ACP is intentionally a small core. Everything below is the
operator's responsibility, or another (separately distributed)
crate's:

- **Conversation storage.** Postgres, SQLite, Firestore, JSONL on
  disk — operator picks. The `BootProvider` hook is the seam between
  the broker and the store.
- **Compute provisioning.** Spinning up VMs / containers / processes
  and pointing them at the broker. Reference orchestrators may ship
  as separate examples or crates, but none of them are part of the
  broker core.
- **Workspace sync.** Hydrating `/workspace` from object storage on
  cold start. Agent-side concern; the protocol carries no workspace
  messages.
- **Channels.** Telegram, web chat, Slack, voice — built on top of
  the REST surface, not in the broker.
- **Identity, billing, tier policy.** No JWT claim, no method, no
  trait in over/ACP encodes any of this. The `user` claim is opaque
  to the broker.
- **Tool catalogues / MCP servers.** A `ToolHost` impl can fan out
  to many MCP servers; that fan-out is operator code.
- **Conversation resume / scrollback.** `initialize` returns whatever
  bounded window the operator's `BootProvider` hands back. Older
  history is reachable as a tool (`tools/call recall_history(...)`),
  not as a protocol method.

## Roadmap

The roadmap is shaped around landing the broker core and getting the
reference loop talking to it. Each milestone is small enough to ship
behind a single acceptance test.

1. **0.1 — vendor `loop`** *(landed)* — copy the reference agent in
   as the `overloop` crate, set up the workspace.
2. **0.2 — `overacp-protocol`** — extract pure wire types and
   method-name constants from the existing server code into a
   tokio-free crate. JWT helpers. Round-trip fixture tests against
   captured JSON.
3. **0.3 — `overacp-agent`** — WS client with reconnect/backoff,
   child-process supervisor, stdio bridge. `AgentAdapter` trait so
   the supervised child can be any ACP-speaking harness; built-in
   adapter for `overloop`.
4. **0.3.x — `overloop` migration to the protocol crate** — switch
   from hard-coded JSON-RPC strings to `overacp-protocol::methods`,
   adopt the new push-shaped `session/message`, drop
   `poll/newMessages`, emit `turn/end` instead of `turn/save`.
5. **0.4 — `overacp-server`: the broker** — stateless tunnel
   terminator + REST adapters + the four traits with no-op default
   impls. Acceptance gate is a single end-to-end test
   (`server/tests/acceptance_0_4.rs`) that runs the broker in-process,
   spawns `overloop` as a subprocess, posts to
   `/agents/X/messages`, and asserts a `stream/textDelta` arrives on
   `/agents/X/stream`. **No compute provider, no `SessionStore`, no
   Postgres** — those are all out of scope for the broker itself.
6. **0.5 — production hardening** — reconnect/redelivery semantics
   for the message queue, multi-replica routing options (sticky LB
   on `agent_id` or shared registry via Redis/NATS), operator REST
   stability, a real `ToolHost` reference impl wired to MCP fan-out.
7. **0.6 — overfolder cutover** — overfolder ships its own
   `BootProvider` and `QuotaPolicy` impls and uses the broker for
   backend ↔ agent-runner communication. overfolder keeps its
   existing Morph integration; the broker doesn't see it.
8. **1.0 — protocol + REST freeze** — semver-stable wire format and
   v1 REST surface.

## Documentation rule

`SPEC.md` is the index. It must describe **everything** about the
system at least at a paragraph level and link into
[`docs/design/`](./docs/design) for the details. Every design doc
under `docs/design/` carries a `status:` frontmatter field —
`Active`, `Superseded`, or `Rejected` — and **no `Active` design
doc section may be unreferenced from `SPEC.md`**. If you add a new
design doc or a new section in an existing one, add (or update) the
SPEC link in the same change. If you supersede a design, flip the
frontmatter and drop the SPEC link in the same change.

## Open questions

- **Reconnect & redelivery semantics.** The current design holds a
  bounded in-memory `MessageQueue` per agent for pushes that arrive
  while the tunnel is disconnected. Lost on broker restart; operator
  re-POSTs are the recovery path. A persistent queue (Redis stream,
  NATS jetstream) is a future option, decided once a real
  multi-replica deployment lands.
- **Multi-replica routing.** Single-replica works out of the box.
  Multi-replica needs either sticky LB routing on `agent_id` or a
  shared registry. Either is feasible behind the existing
  `AgentRegistry` interface; the call gets made when needed.
- **`turn/end` vs webhook.** Currently a notification fanned out
  over SSE for the operator to consume. Some operators may prefer a
  push webhook (HTTP POST from the broker to a configured URL on
  every `turn/end`). Add the webhook hook if and when an operator
  asks for it.
- **History scrollback.** Recommendation is to expose history search
  as a tool via `tools/call`, not to add a `history/fetch` protocol
  method. Revisit if multiple operators end up reimplementing the
  same scrollback shape.
- **`AgentAdapter` plurality.** First-party adapters are `overloop`
  only. Adapters for `claude-code` (TypeScript subprocess) and
  `codex` (Rust crate) are deferred until there is concrete demand.
