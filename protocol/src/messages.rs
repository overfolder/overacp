//! Request, response, and notification payload types for over/ACP.
//!
//! Every type here is `Serialize + Deserialize` and represents a
//! payload that crosses the WebSocket tunnel. Requests use
//! `#[serde(deny_unknown_fields)]` so accidental schema drift fails
//! loudly in tests; responses are permissive so the server can grow
//! new fields without breaking older agents.
//!
//! The stateless broker does not persist any of these payloads. The
//! `initialize` response and `session/message` push are both opaque
//! JSON from the operator's `BootProvider` / REST POST to the agent;
//! `turn/end` fans out to SSE subscribers without being inspected.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Shared message shape ─────────────────────────────────────────────

/// Conversation participant role.
///
/// This intentionally mirrors the OpenAI/LLM convention so that
/// `Message` round-trips cleanly through both `turn/end` and the
/// reference agent's LLM client without a translation layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Message content. Either a flat string or a list of opaque blocks.
/// Blocks are kept as `Value` so the protocol crate doesn't need to
/// know about every multimodal content shape an LLM might emit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<Value>),
}

/// A single turn-history message exchanged between the server and the
/// agent. The optional `tool_calls` and `tool_call_id` fields mirror
/// the OpenAI tool-calling convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Token / cost usage reported alongside a completed turn.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

// ── initialize ───────────────────────────────────────────────────────

/// `initialize` request params. The agent sends this exactly once on
/// cold start to obtain the system prompt + recent message window.
/// Empty struct rather than `()` so the wire shape can grow fields
/// later without breaking older agents.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InitializeRequest {}

/// `initialize` response body. Returned by the broker's
/// `BootProvider` hook and consumed by the agent. The broker itself
/// never inspects these fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InitializeResponse {
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Opaque tools configuration. The agent treats this as
    /// pass-through state.
    #[serde(default)]
    pub tools_config: Value,
}

// ── session/message (server → agent push) ───────────────────────────

/// Params of a `session/message` notification pushed from the broker
/// to the agent. Construction happens server-side in
/// `POST /agents/{id}/messages`; the agent appends the `content` to
/// its in-memory history and starts a turn.
///
/// `content` is opaque — the broker does not inspect it. For the
/// reference agent it is typically a plain string, but a rich
/// multimodal payload (blocks, attachments) is equally valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessageParams {
    pub role: Role,
    pub content: Value,
}

// ── session/cancel (server → agent push) ────────────────────────────

/// Params of a `session/cancel` notification. Empty by design —
/// cancellation is identity-scoped on the tunnel, not request-scoped.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SessionCancelParams {}

// ── quota/check ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct QuotaCheckRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaCheckResponse {
    pub allowed: bool,
}

// ── quota/update ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaUpdateRequest {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// Empty struct (not `()`) so the response can grow fields later
/// without breaking the wire format.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuotaUpdateResponse {}

// ── turn/end (agent → server, fire-and-forget notification) ─────────

/// Params of the `turn/end` notification emitted by the agent when
/// a turn completes. The broker fans this out to SSE subscribers.
///
/// **`messages` is deprecated.** Agents SHOULD omit this field (it
/// will serialize as absent when the vec is empty). Operators that
/// need per-turn persistence should reconstruct the turn from
/// `stream/*` notifications, or wait for `context/compacted` which
/// carries the authoritative post-compaction history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnEndParams {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub usage: Usage,
}

/// Params of the `context/compacted` notification. Emitted by the
/// agent after it compacts its working context. Carries a prose
/// summary of the messages that were dropped and the surviving
/// recent messages (canonical — no agent-internal scaffolding).
///
/// The operator SHOULD replace its stored history for this
/// conversation with `messages` and record `summary` as the
/// compaction prefix so that a future `BootProvider::initialize`
/// can return both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompactedParams {
    /// LLM-generated summary of the messages that were dropped.
    pub summary: String,
    /// The messages that survived compaction (most recent window).
    pub messages: Vec<Message>,
    #[serde(default)]
    pub usage: Usage,
}

// ── stream/* notifications ───────────────────────────────────────────

/// `stream/textDelta` params: an incremental chunk of assistant
/// output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextDelta {
    pub text: String,
}

/// `stream/activity` params: a free-form status / progress update.
/// The `kind` discriminator is a short lowercase string (e.g.
/// `"tool"`, `"thinking"`); `data` is opaque per-kind payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub kind: String,
    #[serde(default)]
    pub data: Value,
}

/// `stream/toolCall` params: emitted immediately before the agent
/// invokes a tool, so observers can surface the in-progress call.
/// The `id` matches the `id` the agent later echoes in the
/// corresponding [`ToolResultNotification`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallNotification {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// `stream/toolResult` params: emitted immediately after a tool
/// invocation returns, carrying the opaque tool output and an
/// `is_error` flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultNotification {
    pub id: String,
    pub content: Value,
    #[serde(default)]
    pub is_error: bool,
}

/// `heartbeat` params. Empty — the frame exists only to keep the
/// tunnel's liveness timer fresh.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {}
