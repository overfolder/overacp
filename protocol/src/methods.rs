//! JSON-RPC method names used on the over/ACP wire.
//!
//! These are the canonical strings sent in the `method` field of every
//! JSON-RPC 2.0 request and notification on the agent ↔ server tunnel.
//! Constants live in one place so a typo can't drift between server and
//! agent implementations.
//!
//! ## Naming policy
//!
//! Where the method overlaps with Zed/Anthropic ACP, the name matches
//! theirs (`initialize`, `tools/list`, `tools/call`, `session/message`)
//! so external harnesses can plug in via thin adapters. Methods that
//! are specific to over/ACP (`quota/*`, `turn/save`, `poll/newMessages`)
//! keep their own names — there is no external standard to borrow.
//!
//! See `PROTOCOL.md` for the per-method payload specification.

// ── Lifecycle / handshake ────────────────────────────────────────────

/// Agent → server. Asks the server for the system prompt, conversation
/// history, and per-session config. First call after the tunnel is up.
pub const INITIALIZE: &str = "initialize";

/// Server → agent. Notifies that a new user message is available; the
/// agent is expected to start its turn loop.
pub const SESSION_MESSAGE: &str = "session/message";

// ── Tool surface (controlplane-hosted MCP, exposed via ToolHost) ─────

/// Agent → server. List the tools currently exposed by `ToolHost`.
pub const TOOLS_LIST: &str = "tools/list";

/// Agent → server. Invoke a tool by name with arguments.
pub const TOOLS_CALL: &str = "tools/call";

// ── Persistence + quota ──────────────────────────────────────────────

/// Agent → server. Persist the messages and usage from a completed
/// turn so the server's `SessionStore` can record them.
pub const TURN_SAVE: &str = "turn/save";

/// Agent → server. Ask the server's `QuotaPolicy` whether the next
/// turn is allowed.
pub const QUOTA_CHECK: &str = "quota/check";

/// Agent → server. Report token / cost usage so the server can
/// increment the user's quota counters.
pub const QUOTA_UPDATE: &str = "quota/update";

/// Agent → server. Poll for any unprocessed user messages on the
/// current conversation.
pub const POLL_NEW_MESSAGES: &str = "poll/newMessages";

// ── Streaming notifications (agent → server, fire-and-forget) ────────

/// Streaming text delta from the model. No response expected.
pub const STREAM_TEXT_DELTA: &str = "stream/textDelta";

/// Free-form activity update (status, progress). No response expected.
pub const STREAM_ACTIVITY: &str = "stream/activity";

/// A tool call has been issued by the model. No response expected;
/// the actual call still goes through `tools/call`.
pub const STREAM_TOOL_CALL: &str = "stream/toolCall";

/// A tool call has produced a result. No response expected.
pub const STREAM_TOOL_RESULT: &str = "stream/toolResult";

/// Periodic keep-alive. No response expected.
pub const HEARTBEAT: &str = "heartbeat";
