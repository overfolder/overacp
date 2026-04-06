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

pub trait AcpService {
    fn stream_text_delta(&mut self, text: &str) -> Result<()>;
    fn stream_activity(&mut self, activity: &str) -> Result<()>;
    fn turn_save(&mut self, messages: &[Message], usage: &Value) -> Result<()>;
    fn quota_check(&mut self) -> Result<bool>;
    fn quota_update(&mut self, input_tokens: u64, output_tokens: u64) -> Result<()>;
    fn poll_new_messages(&mut self) -> Result<Vec<Message>>;
    fn heartbeat(&mut self) -> Result<()>;
}
