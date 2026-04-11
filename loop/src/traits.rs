use anyhow::Result;
use serde_json::Value;
use std::future::Future;

use crate::llm::{CompletionResponse, Message, StopReason, ToolDefinition, Usage};

/// Streamed LLM response assembled from SSE deltas.
pub struct StreamedResponse {
    pub message: Message,
    pub finish_reason: Option<StopReason>,
    pub usage: Option<Usage>,
}

pub trait LlmService: Send + Sync {
    fn stream_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> impl Future<Output = Result<StreamedResponse>> + Send;

    fn complete(
        &self,
        messages: &[Message],
    ) -> impl Future<Output = Result<CompletionResponse>> + Send;
}

/// Result of waiting for the next tunnel push. `session/message`
/// arrives inline as a user `Message`; `session/cancel` surfaces as
/// a sentinel that the outer loop uses to exit cleanly.
#[derive(Debug)]
pub enum NextPush {
    /// A new user message to append to history and start a turn on.
    Message(Message),
    /// The server asked us to cancel the current conversation.
    Cancel,
}

pub trait AcpService {
    fn stream_text_delta(&mut self, text: &str) -> Result<()>;
    fn stream_activity(&mut self, activity: &str) -> Result<()>;

    /// Notification emitted immediately before a tool is invoked.
    /// `id` is the tool-call id from the model's response;
    /// `arguments` is the raw argument JSON that will be passed to
    /// the tool. The matching `stream_tool_result` call echoes the
    /// same `id`.
    fn stream_tool_call(&mut self, id: &str, name: &str, arguments: &Value) -> Result<()>;

    /// Notification emitted immediately after a tool invocation
    /// returns. `id` echoes the one from `stream_tool_call`.
    fn stream_tool_result(&mut self, id: &str, content: &Value, is_error: bool) -> Result<()>;

    /// Fire-and-forget notification emitted when a turn completes,
    /// carrying the turn's messages and usage totals. The broker fans
    /// this out to SSE subscribers.
    fn turn_end(&mut self, messages: &[Message], usage: &Usage) -> Result<()>;
    fn quota_check(&mut self) -> Result<bool>;
    fn quota_update(&mut self, input_tokens: u64, output_tokens: u64) -> Result<()>;
    /// Block until the next `session/message` or `session/cancel`
    /// notification arrives on the tunnel.
    fn next_push(&mut self) -> Result<NextPush>;
    fn heartbeat(&mut self) -> Result<()>;
}
