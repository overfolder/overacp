//! JSON-RPC method names used on the over/ACP wire.
//!
//! These are the canonical strings sent in the `method` field of every
//! JSON-RPC 2.0 request and notification on the agent ↔ server tunnel.
//! Constants live in one place so a typo can't drift between server
//! and agent implementations.
//!
//! ## Naming policy
//!
//! Where the method overlaps with Zed/Anthropic ACP, the name matches
//! theirs (`initialize`, `tools/list`, `tools/call`, `session/message`,
//! `session/cancel`) so external harnesses can plug in via thin
//! adapters. Methods that are specific to over/ACP (`quota/*`,
//! `turn/end`, `stream/*`, `heartbeat`) keep their own names — there
//! is no external standard to borrow.
//!
//! See `docs/design/protocol.md` for the per-method payload
//! specification.

// ── Lifecycle / handshake ────────────────────────────────────────────

/// Agent → server. Asks the server for the system prompt,
/// conversation history, and per-session config. First call after
/// the tunnel is up. Broker delegates to `BootProvider::initialize`.
pub const INITIALIZE: &str = "initialize";

/// Server → agent. Pushes a user message to the agent. The body
/// (`role`, `content`) travels inline in the notification's
/// `params` — there is no follow-up poll. Injected into the tunnel
/// by `POST /agents/{id}/messages`.
pub const SESSION_MESSAGE: &str = "session/message";

/// Server → agent. Asks the agent to abandon its current turn.
/// Injected into the tunnel by `POST /agents/{id}/cancel`. Empty
/// params.
pub const SESSION_CANCEL: &str = "session/cancel";

// ── Tool surface (operator-hosted, exposed via ToolHost) ────────────

/// Agent → server. List the tools currently exposed by the
/// operator's `ToolHost` hook.
pub const TOOLS_LIST: &str = "tools/list";

/// Agent → server. Invoke a tool by name with arguments. Dispatched
/// to `ToolHost::call`.
pub const TOOLS_CALL: &str = "tools/call";

// ── Turn lifecycle + quota ──────────────────────────────────────────

/// Agent → server. Fire-and-forget notification marking the end of
/// a turn; carries the turn's messages and usage. The broker fans it
/// out to SSE subscribers; the operator's backend is responsible for
/// persisting the data.
pub const TURN_END: &str = "turn/end";

/// Agent → server. Ask the operator's `QuotaPolicy` whether the next
/// turn is allowed.
pub const QUOTA_CHECK: &str = "quota/check";

/// Agent → server. Report token / cost usage so the operator's
/// `QuotaPolicy` can update its counters.
pub const QUOTA_UPDATE: &str = "quota/update";

// ── Streaming notifications (agent → server, fire-and-forget) ───────

/// Streaming text delta from the model. No response expected.
pub const STREAM_TEXT_DELTA: &str = "stream/textDelta";

/// Free-form activity update (status, progress). No response
/// expected.
pub const STREAM_ACTIVITY: &str = "stream/activity";

/// A tool call has been issued by the model. Emitted BEFORE the
/// agent actually invokes the tool, for observability. The actual
/// call still goes through `tools/call`.
pub const STREAM_TOOL_CALL: &str = "stream/toolCall";

/// A tool call has produced a result. Emitted AFTER the tool
/// invocation returns, carrying the opaque result and an `is_error`
/// flag.
pub const STREAM_TOOL_RESULT: &str = "stream/toolResult";

/// Periodic keep-alive. No response expected.
pub const HEARTBEAT: &str = "heartbeat";
