# over/ACP Protocol

This document specifies the wire protocol that the over/ACP server and
agent speak to each other across a single multiplexed WebSocket
tunnel. The Rust types live in the `overacp-protocol` crate; this
file is the authoritative description of *what* goes on the wire and
*why*.

For a higher-level architectural picture see [`SPEC.md`](./SPEC.md).

## 1. Transport

A single WebSocket connection per session, opened by the agent and
terminated by the server. URL shape:

```
wss://<server-host>/tunnel/<session-id>
Authorization: Bearer <jwt>
```

The `<session-id>` is the conversation UUID. The agent obtains it by
decoding the `conv` claim from its JWT *without* signature
verification (`overacp_protocol::jwt::peek_claims_unverified`); the
server still validates the bearer token authoritatively when accepting
the upgrade.

Reconnects use exponential backoff (1s → 30s, capped). The session
identifier is stable across reconnects, so the server is expected to
preserve session state and resume streaming on a fresh tunnel.

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

| Field  | Type   | Meaning                                            |
|--------|--------|----------------------------------------------------|
| `sub`  | UUID   | Agent identity (subject).                          |
| `user` | UUID   | User identity.                                     |
| `conv` | UUID   | Conversation ID this token is scoped to.          |
| `tier` | string | Free-form tier label (e.g. `"free"`, `"paid"`).   |
| `exp`  | i64    | Expiration as a Unix timestamp.                    |
| `iss`  | string | Issuer string. Must match what the server expects. |

### 2.2 Issuer and TTL

The issuer string and the TTL are **parameters** of `mint_token` and
`validate_token`. The protocol crate bakes no product-specific issuer
into the wire format; deployments choose their own. The recommended
default lifetime is `DEFAULT_TOKEN_TTL_SECS` (3600 seconds).

### 2.3 Algorithm

HS256 with a shared signing key. The protocol does not currently
support asymmetric algorithms; that is on the roadmap for the
multi-tenant deployment described in `SPEC.md`.

## 3. Method catalogue

All method names are exported as `&'static str` constants from
`overacp_protocol::methods`.

### 3.1 Lifecycle

| Method            | Direction       | Kind          |
|-------------------|-----------------|---------------|
| `initialize`      | agent → server  | request       |
| `session/message` | server → agent  | notification  |

`initialize` is the first call after the tunnel is up. The server
returns the system prompt, the recent conversation history, the
conversation ID, and an opaque `tools_config` blob the agent will
treat as pass-through state.

```jsonc
// initialize response
{
  "system_prompt": "You are a helpful assistant.",
  "messages": [ ...prior turns... ],
  "conversation_id": "aaaa...-eeee",
  "tools_config": {}
}
```

`session/message` is a notification from the server telling the agent
that a new user message is available; the agent is expected to start
its turn loop. The actual message body is fetched via
`poll/newMessages`.

### 3.2 Tool surface

| Method        | Direction      | Kind     |
|---------------|----------------|----------|
| `tools/list`  | agent → server | request  |
| `tools/call`  | agent → server | request  |

Tools are hosted **on the controlplane**, not in the agent VM. The
server runs MCP clients against operator-configured MCP servers and
re-exposes them through `ToolHost` as a unified `tools/list` /
`tools/call` surface. The agent never learns which tools came from
MCP, and the agent compute environment never touches the MCP server
directly. This is the "case A" model from `SPEC.md`; the alternative
of injecting MCP server configs down into the child agent process is
explicitly out of scope.

### 3.3 Persistence and quota

| Method              | Direction      | Kind    |
|---------------------|----------------|---------|
| `turn/save`         | agent → server | request |
| `quota/check`       | agent → server | request |
| `quota/update`      | agent → server | request |
| `poll/newMessages`  | agent → server | request |

`turn/save` persists the messages and usage from a completed turn:

```jsonc
// turn/save request
{
  "messages": [
    { "role": "user", "content": "what's the weather?" },
    { "role": "assistant", "content": "I'll check." }
  ],
  "usage": { "input_tokens": 100, "output_tokens": 50 }
}
```

`quota/check` returns `{ "allowed": bool }`. The server's
`QuotaPolicy` decides; the protocol does not encode tier rules.

`quota/update` reports token usage to be added to the user's running
totals. The response is an empty struct (not `()`) so it can grow
fields later without breaking the wire format.

`poll/newMessages` returns any user messages that have arrived since
the last poll. The reference server returns up to ten at a time.

### 3.4 Streaming notifications

These are fire-and-forget agent → server notifications that carry
incremental output to the user-facing channel.

| Method                | Payload                       |
|-----------------------|-------------------------------|
| `stream/textDelta`    | `{ "text": "..." }`           |
| `stream/activity`     | `{ "kind": "...", "data": ... }` |
| `stream/toolCall`     | `{ "id": "...", "name": "...", "arguments": ... }` |
| `stream/toolResult`   | `{ "id": "...", "content": ..., "is_error": false }` |
| `heartbeat`           | `{}`                          |

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

Methods that overlap with Zed/Anthropic ACP keep their names —
`initialize`, `tools/list`, `tools/call`, `session/message` — so
external harnesses (Claude Code, Codex, ...) can plug into over/ACP
through thin adapter crates without a translation layer in the
middle. Methods that are specific to over/ACP (`quota/*`, `turn/save`,
`poll/newMessages`) keep their own names since no external standard
covers them.

This policy is the answer to the "Naming the protocol crate's
methods" open question in `SPEC.md`.

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
