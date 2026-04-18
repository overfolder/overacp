---
status: Active
---

# over/ACP Protocol

This document specifies the wire protocol that the over/ACP server and
agent speak to each other across a single multiplexed WebSocket
tunnel. The Rust types live in the `overacp-protocol` crate; this
file is the authoritative description of *what* goes on the wire and
*why*.

For a higher-level architectural picture see [`SPEC.md`](../../SPEC.md).

## 1. Transport

A single WebSocket connection per agent, opened by the agent and
terminated by the server. URL shape:

```
wss://<server-host>/tunnel/<agent-id>
Authorization: Bearer <jwt>
```

The `<agent-id>` is the agent UUID and matches the `sub` claim of the
JWT. The agent obtains it from its boot environment
(`OVERACP_AGENT_ID`); the server validates the bearer token
authoritatively at upgrade time and rejects the connection if the
JWT's `sub` does not match the path, or if the token does not carry
the `agent` role.

Reconnects use exponential backoff (1s → 30s, capped). The agent
identifier is stable across reconnects, so the server is expected to
restore the agent's routing state on a fresh tunnel.

### 1.1 Frame format

Each WebSocket text frame carries exactly one JSON-RPC 2.0 message:
either a request, a response, or a notification. There is no batching,
no framing prefix, and no length header. Implementations MUST NOT
split a single JSON-RPC message across multiple WebSocket frames.

Binary frames are reserved and MUST be ignored by both sides today.

### 1.2 JSON-RPC 2.0 conformance

- `jsonrpc` is always the string `"2.0"`.
- Requests carry `id` (string or number). Responses echo it.
- Notifications omit `id`.
- Errors use the JSON-RPC `error` object with `code` and `message`.
  over/ACP reserves the standard `-32700`..`-32603` codes; application
  errors use codes `≥ 1000`.

## 2. Authentication: JWT session tokens

over/ACP authenticates the tunnel and the LLM proxy with the same
short-lived JWT. The crate exposes `Claims`, `mint_token`,
`validate_token`, and `peek_claims_unverified` in `overacp_protocol::jwt`.

### 2.1 Claims

| Field  | Type        | Meaning                                                              |
|--------|-------------|----------------------------------------------------------------------|
| `sub`  | UUID        | Subject. For `agent` tokens this is the `agent_id` (routing key). For `admin` tokens this is the operator identity. |
| `role` | string      | `"admin"` or `"agent"`. Decides which routes the token can hit.      |
| `user` | UUID? (opt) | Optional opaque user identifier. Present only on agent tokens when the operator chooses to forward it. The broker never inspects it. |
| `exp`  | i64         | Expiration as a Unix timestamp.                                      |
| `iss`  | string      | Issuer string. Must match what the server expects.                   |

The broker has exactly two token types:

- **Admin JWT** — held by the operator backend. Full access to every
  REST endpoint, can mint agent tokens via `POST /tokens`. Cannot
  hold a tunnel.
- **Agent JWT** — held by the agent process inside the compute
  environment, and optionally by clients (web frontend) for a
  specific agent. Scoped to a single `agent_id`; can hold the
  matching tunnel and call the agent-scoped REST endpoints.

over/ACP intentionally has **no tier, plan, or entitlement claim** in
the protocol. Quota and tier policy belong to the deployment, not the
wire format — over/ACP is OSS and doesn't dictate billing models.
Servers that need per-user policy decisions can carry that state in
their own database keyed on `user`, or in additional out-of-band
headers, but it must not leak into the protocol crate's `Claims`.

### 2.2 Issuer and TTL

The issuer string and the TTL are **parameters** of `mint_token` and
`validate_token`. The protocol crate bakes no product-specific issuer
into the wire format; deployments choose their own. The recommended
default lifetime is `DEFAULT_TOKEN_TTL_SECS` (3600 seconds).

### 2.3 Algorithm

HS256 with a shared signing key. The protocol does not currently
support asymmetric algorithms; that is on the roadmap for the
multi-tenant deployment described in `SPEC.md`.

### 2.4 Agent supervisor boot contract

The `overacp-agent` supervisor is configured **exclusively through
environment variables**. There is no config file, no CLI flags
beyond `--help`/`--version`, and no positional arguments. The
operator (or whichever orchestrator launches the compute environment)
populates these variables before starting the supervisor process.

| Variable | Required | Description |
|---|---|---|
| `OVERACP_TOKEN` | yes | Agent JWT (bearer token for the WebSocket upgrade). See § 2.1 for the claims. The supervisor decodes the `sub` claim locally (without signature verification, because the broker validates it authoritatively on the WebSocket upgrade) to build the tunnel URL; the broker also uses it as the routing key. |
| `OVERACP_SERVER_URL` | yes | Base URL of the broker, e.g. `https://acp.example.com` or `http://localhost:8080`. The supervisor rewrites the scheme to `ws` / `wss` and appends `/tunnel/<agent_id>`. |
| `OVERACP_WORKSPACE` | no | Working directory the child agent process should treat as its workspace. Defaults to `/workspace`. Forwarded to `LoopAdapter` as the `OVERACP_WORKSPACE` env var on the child. |
| `OVERACP_AGENT_BINARY` | no | Path or basename of the child-agent binary. Resolved by the active `AgentAdapter` impl. Defaults to `overloop`. |

The `OVERACP_*` namespace is reserved for the supervisor; any other
environment variables are inherited by the child process so
deployments can pass adapter-specific config (e.g. `LLM_API_KEY`,
`LLM_API_URL`, `OVERFOLDER_MODEL` for `overloop`) without the broker
having to know about it.

The recommended **agent JWT TTL is 30 days**. Rotation is deferred
(tracked in [`TODO.md`](../../TODO.md) and `SPEC.md` open questions);
the current reference server mints once via `POST /tokens` and does
not refresh.

## 3. Method catalogue

All method names are exported as `&'static str` constants from
`overacp_protocol::methods`.

Each method is tagged with its **origin**:

- **ACP** — borrowed verbatim from the Zed/Anthropic Agent Client
  Protocol so external harnesses can plug in unchanged.
- **MCP** — borrowed verbatim from the Model Context Protocol.
  The operator's `ToolHost` hook typically fans out to one or more
  MCP clients and re-exposes them through `tools/list` / `tools/call`
  verbatim, so the wire names match upstream unchanged.
- **extension** — over/ACP-specific. No upstream standard covers
  these; the names are ours.

| Method              | Origin     | Direction       | Kind         |
|---------------------|------------|-----------------|--------------|
| `initialize`        | ACP        | agent → server  | request      |
| `session/message`   | extension  | server → agent  | notification |
| `session/cancel`    | extension  | server → agent  | notification |
| `tools/list`        | MCP        | agent → server  | request      |
| `tools/call`        | MCP        | agent → server  | request      |
| `quota/check`       | extension  | agent → server  | request      |
| `quota/update`      | extension  | agent → server  | request      |
| `turn/end`          | extension  | agent → server  | notification |
| `context/compacted` | extension  | agent → server  | notification |
| `stream/textDelta`  | extension  | agent → server  | notification |
| `stream/activity`   | extension  | agent → server  | notification |
| `stream/toolCall`   | extension  | agent → server  | notification |
| `stream/toolResult` | extension  | agent → server  | notification |
| `heartbeat`         | extension  | agent → server  | notification |

### 3.1 Lifecycle

`initialize` is the first call after the tunnel is up, and is called
exactly once per cold-start of the agent (not per turn). The broker
delegates to the operator's `BootProvider` hook, which returns the
system prompt, recent conversation history, and an opaque
`tools_config` blob the agent will treat as pass-through state. The
broker itself never inspects the response.

```jsonc
// initialize response
{
  "system_prompt": "You are a helpful assistant.",
  "messages": [ ...prior turns... ],
  "tools_config": {}
}
```

`session/message` is a server → agent notification that delivers a
user message to a connected agent. The body travels **inline in the
notification** — there is no separate poll round-trip. The agent
appends the message to its in-memory history and starts its turn
loop. If the agent is disconnected when the broker receives a push
via `POST /agents/{id}/messages`, the broker buffers the
notification in a bounded in-memory `MessageQueue` and drains it on
the next reconnect, before yielding to live traffic. The buffer is
in-memory and lossy across broker restarts.

`session/cancel` is a server → agent notification that asks the
agent to abandon its current turn. Pushed by the broker when the
operator calls `POST /agents/{id}/cancel`.

### 3.2 Tool surface

Tools are hosted **on the operator's trusted side**, not in the
agent compute environment. The broker delegates every `tools/list`
and `tools/call` to the operator's `ToolHost` hook, which typically
runs MCP clients against operator-configured MCP servers and
re-exposes them through `tools/list` / `tools/call` as a unified
surface. The agent never learns which tools came from MCP, and the
agent compute environment never touches the MCP server directly.
Injecting MCP server configs down into the child agent process is
explicitly out of scope.

### 3.3 Turn completion and quota (extensions)

These extensions cover end-of-turn signalling and quota/usage
hooks. No upstream standard covers them, so the names are ours.

`turn/end` is a **fire-and-forget notification** that an agent
emits when it finishes a turn. The broker fans it out to SSE
subscribers. **The `messages` field is deprecated** — agents SHOULD
send only `usage`. Operators that need per-turn message persistence
should reconstruct from `stream/*` notifications or wait for
`context/compacted`.

```jsonc
// turn/end notification (messages deprecated, omitted)
{
  "jsonrpc": "2.0",
  "method": "turn/end",
  "params": {
    "usage": { "input_tokens": 100, "output_tokens": 50 }
  }
}
```

`context/compacted` is a **fire-and-forget notification** emitted
after the agent compacts its working context. It carries a prose
`summary` of the dropped messages and the surviving canonical
`messages`. The operator SHOULD replace its stored history with
`messages` and record `summary` as the compaction prefix so that a
future `BootProvider::initialize` can return both. See
[`context-management.md`](./context-management.md) for the full
design.

```jsonc
// context/compacted notification
{
  "jsonrpc": "2.0",
  "method": "context/compacted",
  "params": {
    "summary": "User asked to refactor auth...",
    "messages": [
      { "role": "user", "content": "now deploy it" },
      { "role": "assistant", "content": "Deploying..." }
    ],
    "usage": { "input_tokens": 1200, "output_tokens": 400 }
  }
}
```

`quota/check` returns `{ "allowed": bool }`. The broker delegates
to the operator's `QuotaPolicy::check`; the protocol carries no
tier or pricing state. Deployments that don't bill at all can return
a constant `{ "allowed": true }` from a no-op `QuotaPolicy`.

`quota/update` reports usage to be recorded against the agent's
running totals. The broker delegates to `QuotaPolicy::record`. The
response is an empty struct (not `()`) so it can grow fields later
without breaking the wire format.

### 3.4 Streaming notifications (extensions)

Fire-and-forget agent → server notifications that carry incremental
output to the user-facing channel. ACP has its own session-update
notifications with a different shape; over/ACP uses these flat
extension messages because they map directly onto a per-channel
SSE/Valkey fan-out on the server side.

| Method                | Payload                                              |
|-----------------------|------------------------------------------------------|
| `stream/textDelta`    | `{ "text": "..." }`                                  |
| `stream/activity`     | `{ "kind": "...", "data": ... }`                     |
| `stream/toolCall`     | `{ "id": "...", "name": "...", "arguments": ... }`   |
| `stream/toolResult`   | `{ "id": "...", "content": ..., "is_error": false }` |
| `heartbeat`           | `{}`                                                 |

`stream/toolCall` is informational — the actual tool invocation still
goes through the request-shaped `tools/call` so the agent gets a
typed response.

## 4. Shared types

### 4.1 `Message`

```jsonc
{
  "role": "user" | "assistant" | "system" | "tool",
  "content": "string" | [ ...blocks... ],
  "tool_calls": <opaque>,         // optional, OpenAI tool-call shape
  "tool_call_id": "..."           // optional, for role="tool"
}
```

`content` is either a flat string or a list of opaque blocks. Blocks
are kept as `serde_json::Value` so the protocol crate doesn't need to
know about every multimodal content shape an LLM might emit. The
reference agent (`overloop`) emits the OpenAI tool-call shape today.

#### Known block types

The reference agent recognises the following `type` discriminators
when deserialising content blocks. Unrecognised types are absorbed
by a catch-all variant and passed through without error.

| `type`        | Payload shape                                          | Origin   |
|---------------|--------------------------------------------------------|----------|
| `text`        | `{ "text": "..." }`                                    | OpenAI   |
| `image_url`   | `{ "image_url": { "url": "..." } }`                   | OpenAI   |
| `image`       | `{ "source": { "type": "base64", "media_type": "...", "data": "..." } }` | Anthropic |
| `input_audio` | `{ "input_audio": { "data": "...", "format": "..." } }` | OpenAI   |

The protocol crate itself does **not** enumerate these types — its
`Content::Blocks` variant carries `Vec<Value>`. The table above
documents what `overloop` will understand natively; any block shape
the operator sends will transit the broker untouched.

### 4.2 `Usage`

```jsonc
{ "input_tokens": 1234, "output_tokens": 567 }
```

Both fields default to zero on the wire so older agents can omit
them.

## 5. Schema discipline

- **Requests** use `#[serde(deny_unknown_fields)]`. Schema drift on
  the request side fails loudly: a typo in a field name on either
  side becomes a deserialization error in CI rather than a silently
  ignored field at runtime.
- **Responses** are permissive. The server can grow new response
  fields without breaking older agents.
- All payload types live in `overacp_protocol::messages` with
  `Serialize + Deserialize` derives, and every shape has a fixture
  under `protocol/tests/fixtures/*.json` exercised by
  `wire_fixtures.rs` (parse → re-serialize → JSON-value compare).

## 6. Versioning

- The crate is at `0.x` and makes no SemVer guarantees on wire
  compatibility yet.
- Breaking wire changes will bump the minor version while we are
  pre-1.0; once 1.0 ships, breaking changes bump the major.
- Method names, field names, and required-vs-optional status are all
  considered part of the wire contract for SemVer purposes.

## 7. Naming policy (resolved open question)

Method names are picked from upstream standards wherever possible so
external implementations plug in without a translation layer:

- **ACP names** (`initialize`) come from the Zed/Anthropic Agent
  Client Protocol. External agent harnesses (Claude Code, Codex)
  speak ACP natively, so these names let an adapter crate be a thin
  passthrough.
- **MCP names** (`tools/list`, `tools/call`) come from the Model
  Context Protocol. The broker delegates to a `ToolHost` hook which
  typically fans out to MCP clients on behalf of the agent and
  re-exposes their tools verbatim, so the wire names match upstream
  too.
- **over/ACP extensions** keep names that fit the surrounding
  semantics (`turn/end`, `quota/*`, `stream/*`, `heartbeat`,
  `session/message`, `session/cancel`). These have no upstream
  equivalent.

The per-method origin column in § 3 makes the classification
explicit. This policy is the answer to the "Naming the protocol
crate's methods" open question in `SPEC.md`.

## 8. Out of scope

- **Child-process MCP injection.** The server hosts MCP clients
  itself; the agent never sees raw MCP. See `SPEC.md` § Open
  questions → Tool model.
- **Asymmetric JWT algorithms.** HS256 only today.
- **Workspace sync over the protocol.** Workspace hydration is the
  job of `WorkspaceSync` impls in the agent crate, not a wire
  message.
- **Multi-agent orchestration.** A parent agent that spawns child
  agents on new VMs is expressed as a `ComputeBackend` tool call
  inside `tools/call`, not a new method.
