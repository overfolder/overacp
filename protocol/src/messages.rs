//! Request, response, and notification payload types for over/ACP.
//!
//! Every type here is `Serialize + Deserialize` and represents a
//! payload that crosses the WebSocket tunnel. Requests use
//! `#[serde(deny_unknown_fields)]` so accidental schema drift fails
//! loudly in tests; responses are permissive so the server can grow
//! new fields without breaking older agents.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ── Shared message shape ─────────────────────────────────────────────

/// Conversation participant role.
///
/// This intentionally mirrors the OpenAI/LLM convention so that
/// `Message` round-trips cleanly through both `turn/save` and the
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

/// Token / cost usage reported alongside a saved turn.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

// ── initialize ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InitializeRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResponse {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub conversation_id: Uuid,
    /// Opaque tools configuration. The agent treats this as
    /// pass-through state.
    #[serde(default)]
    pub tools_config: Value,
}

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

// ── turn/save ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnSaveRequest {
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnSaveResponse {}

// ── poll/newMessages ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PollNewMessagesRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollNewMessagesResponse {
    pub messages: Vec<Message>,
}

// ── stream/* notifications ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextDelta {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub kind: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallNotification {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultNotification {
    pub id: String,
    pub content: Value,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {}
