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
- **Authenticates and mints tokens.** Two JWT types — admin (full
  access) and agent (scoped to one agent_id) — validated and minted
  by a pluggable `Authenticator` trait. Reference impl is HS256
  against a static signing key. Admin JWTs mint agent JWTs via
  `POST /tokens` or a direct library call.
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
- **No agent enrollment API.** Connecting with a valid agent JWT
  *is* the enrollment. There is no `POST /agents`.
- **No identity hierarchy.** No tier, plan, or entitlement claim in
  the JWT beyond `role` (admin vs agent). Whatever identity model
  the operator wants lives outside.
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

## Authentication

Two JWT types, one signing key, both validated by the same
`Authenticator` trait. The `role` claim decides what you can do.

### Admin JWT

Held by the external system (operator backend, CLI, orchestrator).
Grants full access to all REST endpoints and the ability to mint
agent tokens.

| Claim  | Value |
|--------|-------|
| `sub`  | operator identity (UUID or service account) |
| `role` | `"admin"` |
| `exp`  | expiry |
| `iss`  | issuer |

### Agent JWT

Held by the agent process in the VM, and optionally by clients
(web frontend) for a specific agent. Scoped to a single `agent_id`.

| Claim  | Value |
|--------|-------|
| `sub`  | agent_id (routing key) |
| `role` | `"agent"` |
| `user` | optional opaque user identifier |
| `exp`  | expiry |
| `iss`  | issuer |

### Route authorization

Admin JWTs grant access to every endpoint. Agent JWTs are scoped to
the agent identified by `sub`:

| Route | Admin | Agent (sub=X) |
|---|---|---|
| `POST /tokens` | yes | no |
| `GET /agents` | yes | no |
| `GET /agents/{id}` | yes (any) | no |
| `DELETE /agents/{id}` | yes (any) | no |
| `/tunnel/X` | no | yes (sub must match) |
| `POST /agents/X/messages` | yes (any) | yes (sub=X only) |
| `GET /agents/X/stream` | yes (any) | yes (sub=X only) |
| `POST /agents/X/cancel` | yes (any) | yes (sub=X only) |

This lets a web frontend hold an agent JWT for the conversation it's
showing — stream and send messages, but can't list other agents or
mint tokens.

### Minting agent tokens

**REST endpoint** — for external systems that drive the broker over
HTTP:

```
POST /tokens
Authorization: Bearer <admin-jwt>
Body: { "agent_id": "uuid", "user": "uuid", "ttl_secs": 2592000 }

Response: { "token": "eyJ...", "claims": { "sub": "...", ... } }
```

**Library call** — for operators embedding `overacp-server` in-process
(no HTTP round-trip):

```rust
let claims = Claims::agent(agent_id, user_id, ttl);
let jwt = state.authenticator.mint(&claims)?;
```

Both paths use the same signing key. The REST endpoint is a thin
wrapper around the library call.

### Bootstrapping admin tokens

The admin JWT is the bootstrap credential. Three equivalent paths:

1. **CLI tool.** `overacp-server mint-admin --signing-key $KEY`
   prints a long-lived admin JWT.
2. **Self-minted.** The operator knows the signing key and constructs
   `{sub, role: "admin", exp, iss}` with any JWT library in any
   language.
3. **Startup emission.** The server prints an admin JWT to stderr on
   boot when `OVERACP_EMIT_ADMIN_TOKEN=true`. Useful for local dev.

## Wire protocol summary

Full spec in [`docs/design/protocol.md`](./docs/design/protocol.md).

A single multiplexed WebSocket tunnel per agent, JSON-RPC 2.0 frames,
agent JWT in the `Authorization: Bearer` header on upgrade.

### Method catalogue

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

### `initialize` — conversation bootstrap

Called **once** by the agent on cold start (not per-turn). The broker
delegates to the operator's `BootProvider` hook, which returns
whatever the agent needs to begin or resume a conversation.

```
Agent → server (request):
  { "jsonrpc": "2.0", "id": 1, "method": "initialize" }

Server → agent (response):
  { "jsonrpc": "2.0", "id": 1, "result": {
      "system_prompt": "You are a helpful assistant.",
      "messages": [ ...curated history window... ],
      "tools_config": {}
  }}
```

The broker itself never inspects the response — it's opaque JSON
flowing from the operator's hook back through the tunnel. The
`BootProvider` implementation runs on the trusted side with DB
access; the agent VM only sees the serialized result.

The agent holds the returned messages in memory and accumulates
new messages across turns. It does not call `initialize` again
unless the process restarts (cold start). "Resume by agent_id"
is implicit: cold-start the agent with the same `OVERACP_AGENT_ID`,
and the `BootProvider` hook looks up the conversation history in the
operator's database using `claims.sub`.

### `session/message` — push delivery

The body travels **in the notification**, not via a separate poll.
When the operator `POST`s to `/agents/{id}/messages`, the broker
wraps the content in a `session/message` notification and pushes it
down the tunnel:

```
Server → agent (notification):
  { "jsonrpc": "2.0", "method": "session/message",
    "params": { "role": "user", "content": "hello" } }
```

The agent appends this message to its in-memory history and starts
a turn. There is no `poll/newMessages` on the wire. Mid-turn
message delivery is handled internally by the supervisor, which
buffers incoming `session/message` notifications and serves them
to the child process on demand.

### `turn/end` — fire-and-forget turn completion

When the agent finishes a turn it emits `turn/end` as a
**notification** (no response expected). The broker fans it out
to SSE subscribers; the operator is responsible for persisting the
data.

```
Agent → server (notification):
  { "jsonrpc": "2.0", "method": "turn/end",
    "params": {
      "messages": [ ...turn messages... ],
      "usage": { "input_tokens": 1234, "output_tokens": 567 }
  }}
```

## REST surface

Agent-facing routes authenticate with agent JWTs (scoped to the
agent's `sub`). Admin routes authenticate with admin JWTs. Both
validated by the same `Authenticator`.

```
POST   /tokens                  mint agent JWTs (admin only)
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
pub trait BootProvider: Send + Sync + 'static {
    async fn initialize(&self, claims: &Claims) -> Result<Value, BootError>;
}

#[async_trait]
pub trait ToolHost: Send + Sync + 'static {
    async fn list(&self, claims: &Claims) -> Result<Value, ToolError>;
    async fn call(&self, claims: &Claims, req: Value) -> Result<Value, ToolError>;
}

#[async_trait]
pub trait QuotaPolicy: Send + Sync + 'static {
    async fn check(&self, claims: &Claims) -> Result<bool, QuotaError>;
    async fn record(&self, claims: &Claims, usage: Value) -> Result<(), QuotaError>;
}

pub trait Authenticator: Send + Sync + 'static {
    fn validate(&self, token: &str) -> Result<Claims, AuthError>;
    fn mint(&self, claims: &Claims) -> Result<String, AuthError>;
}
```

Hook payloads (`req`, `usage`, and the boot/tools/list responses)
are intentionally typed as `serde_json::Value`. The broker is
payload-agnostic — it routes JSON-RPC frames between the agent and
the operator's hook impl without ever inspecting their contents.
Operators with stronger typing needs are free to deserialize the
`Value` into their own structs inside the impl.

Default implementations: `BootProvider` returns an empty bootstrap
(no system prompt, no history); `ToolHost` returns no tools;
`QuotaPolicy` allows everything; `Authenticator` validates and mints
HS256 JWTs against a static signing key. The reference server boots
with all four defaults so `cargo run` works out of the box and
end-to-end demos don't need an operator stack.

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
