use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::error::Error as StdError;
use std::fmt;
use std::time::{Duration, Instant};
use tracing::{debug, error, warn};

use crate::traits::{LlmService, StreamedResponse};
use tokio::time::sleep;

use super::{
    CompletionResponse, Content, Delta, FunctionCall, Message, Role, StreamEvent, ToolCall,
    ToolDefinition,
};

/// Classifies mid-stream / transport errors so callers can decide whether to
/// retry.
#[derive(Debug)]
enum StreamError {
    /// Transient failure (timeout, connection reset, server error) — caller
    /// should retry with backoff.
    Retryable {
        message: String,
        source: Box<dyn StdError + Send + Sync>,
    },
    /// Permanent failure (auth error, bad request) — retrying will not help.
    Fatal {
        message: String,
        source: Box<dyn StdError + Send + Sync>,
    },
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retryable { message, .. } | Self::Fatal { message, .. } => {
                write!(f, "{message}")
            }
        }
    }
}

impl StdError for StreamError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Retryable { source, .. } | Self::Fatal { source, .. } => Some(source.as_ref()),
        }
    }
}

impl StreamError {
    const fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }
}

/// Inspect a `reqwest::Error` and wrap it as retryable or fatal.
fn classify_reqwest_error(e: reqwest::Error, elapsed: Duration) -> StreamError {
    let elapsed_s = elapsed.as_secs_f64();
    if e.is_timeout() {
        StreamError::Retryable {
            message: format!("Stream read timed out after {elapsed_s:.1}s: {e}"),
            source: Box::new(e),
        }
    } else if e.is_connect() {
        StreamError::Retryable {
            message: format!("Connection error during stream after {elapsed_s:.1}s: {e}"),
            source: Box::new(e),
        }
    } else if e.is_body() || e.is_decode() {
        // Body / decode errors typically indicate a connection reset mid-stream.
        StreamError::Retryable {
            message: format!(
                "Stream body error (possible connection reset) after {elapsed_s:.1}s: {e}"
            ),
            source: Box::new(e),
        }
    } else if let Some(status) = e.status() {
        if status.is_server_error() {
            StreamError::Retryable {
                message: format!("Server error {status} during stream after {elapsed_s:.1}s: {e}"),
                source: Box::new(e),
            }
        } else {
            // 4xx — auth, bad request, etc.
            StreamError::Fatal {
                message: format!("Client error {status} during stream after {elapsed_s:.1}s: {e}"),
                source: Box::new(e),
            }
        }
    } else {
        // Unknown reqwest error — treat as retryable to be safe.
        StreamError::Retryable {
            message: format!("Stream error after {elapsed_s:.1}s: {e}"),
            source: Box::new(e),
        }
    }
}

pub struct LlmClient {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
}

impl LlmClient {
    pub fn new(api_url: &str, api_key: &str, model: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("build http client");

        Self {
            client,
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    /// Stream a chat completion, calling `on_text` for each text delta.
    pub async fn stream_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<StreamedResponse> {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
        }

        let response = self
            .request_with_retry(&body)
            .await
            .context("LLM request failed")?;

        self.process_stream(response, on_text).await
    }

    /// Non-streaming completion (used by compaction).
    pub async fn complete(&self, messages: &[Message]) -> Result<CompletionResponse> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
        });

        let response = self.request_with_retry(&body).await?;
        let text = response.text().await?;
        serde_json::from_str(&text).context("parse completion response")
    }

    async fn request_with_retry(&self, body: &Value) -> Result<reqwest::Response> {
        let url = format!("{}/chat/completions", self.api_url);
        let mut last_err = None;

        for attempt in 0..3 {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt));
                warn!("LLM retry attempt {}, waiting {:?}", attempt, delay);
                sleep(delay).await;
            }

            let attempt_start = Instant::now();
            match self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) if resp.status().as_u16() == 429 => {
                    let body_text = resp.text().await.unwrap_or_default();
                    let msg = format!("rate limited (429): {body_text}");
                    self.report_llm_error(attempt, &msg);
                    last_err = Some(anyhow::anyhow!(msg));
                    continue;
                }
                Ok(resp) if resp.status().is_server_error() => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let msg = format!("server error {status}: {body}");
                    self.report_llm_error(attempt, &msg);
                    last_err = Some(anyhow::anyhow!(msg));
                    continue;
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let msg = format!("LLM error {status}: {body}");
                    self.report_llm_error(attempt, &msg);
                    anyhow::bail!(msg);
                }
                Err(e) => {
                    let classified = classify_reqwest_error(e, attempt_start.elapsed());
                    self.report_llm_error(attempt, &classified.to_string());
                    if classified.is_retryable() {
                        last_err = Some(anyhow::Error::new(classified));
                        continue;
                    }
                    return Err(anyhow::Error::new(classified));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("LLM request failed")))
    }

    /// Emit an error-level tracing event with structured Sentry context
    /// (provider/model/retry tags + error body extra) when the `sentry`
    /// feature is enabled. Without the feature, this is just a tracing
    /// `error!` call.
    fn report_llm_error(&self, retry: u32, error_body: &str) {
        #[cfg(feature = "sentry")]
        {
            let model = self.model.as_str();
            sentry::with_scope(
                |scope| {
                    scope.set_tag("llm.provider", "openai-compatible");
                    scope.set_tag("llm.model", model);
                    scope.set_tag("llm.retry", retry.to_string());
                    scope.set_extra(
                        "llm.error_body",
                        serde_json::Value::String(error_body.to_string()),
                    );
                },
                || {
                    error!(model = %model, retry, "LLM error: {}", error_body);
                },
            );
        }
        #[cfg(not(feature = "sentry"))]
        error!(model = %self.model, retry, "LLM error: {}", error_body);
    }

    async fn process_stream(
        &self,
        response: reqwest::Response,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<StreamedResponse> {
        let mut content = String::new();
        let mut tool_calls: Vec<PartialToolCall> = Vec::new();
        let mut finish_reason = None;
        let mut usage = None;

        let mut stream = response.bytes_stream();

        let mut buffer = String::new();
        let stream_start = Instant::now();

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let classified = classify_reqwest_error(e, stream_start.elapsed());
                    self.report_llm_error(0, &classified.to_string());
                    return Err(anyhow::Error::new(classified).context("read SSE chunk"));
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                let data = match line.strip_prefix("data: ") {
                    Some(d) => d.trim(),
                    None => continue,
                };

                if data == "[DONE]" {
                    break;
                }

                let event: StreamEvent = match serde_json::from_str(data) {
                    Ok(e) => e,
                    Err(e) => {
                        debug!("skip unparseable SSE event: {}", e);
                        continue;
                    }
                };

                if let Some(u) = event.usage {
                    usage = Some(u);
                }

                for choice in &event.choices {
                    if let Some(reason) = &choice.finish_reason {
                        finish_reason = Some(reason.clone());
                    }

                    if let Some(Delta {
                        content: Some(text),
                        ..
                    }) = &choice.delta
                    {
                        content.push_str(text);
                        on_text(text);
                    }

                    if let Some(Delta {
                        tool_calls: Some(deltas),
                        ..
                    }) = &choice.delta
                    {
                        for tc_delta in deltas {
                            accumulate_tool_call(&mut tool_calls, tc_delta);
                        }
                    }
                }
            }
        }

        let final_tool_calls: Option<Vec<ToolCall>> = if tool_calls.is_empty() {
            None
        } else {
            Some(
                tool_calls
                    .into_iter()
                    .map(|ptc| ToolCall {
                        id: ptc.id,
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: ptc.name,
                            arguments: ptc.arguments,
                        },
                    })
                    .collect(),
            )
        };

        let message_content = if content.is_empty() {
            None
        } else {
            Some(Content::Text(content))
        };

        Ok(StreamedResponse {
            message: Message {
                role: Role::Assistant,
                content: message_content,
                tool_calls: final_tool_calls,
                tool_call_id: None,
            },
            finish_reason,
            usage,
        })
    }
}

struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn accumulate_tool_call(calls: &mut Vec<PartialToolCall>, delta: &super::ToolCallDelta) {
    while calls.len() <= delta.index {
        calls.push(PartialToolCall {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
        });
    }

    let entry = &mut calls[delta.index];

    if let Some(id) = &delta.id {
        entry.id.clone_from(id);
    }

    if let Some(func) = &delta.function {
        if let Some(name) = &func.name {
            entry.name.clone_from(name);
        }
        if let Some(args) = &func.arguments {
            entry.arguments.push_str(args);
        }
    }
}

impl LlmService for LlmClient {
    async fn stream_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<StreamedResponse> {
        self.stream_completion(messages, tools, on_text).await
    }

    async fn complete(&self, messages: &[Message]) -> Result<CompletionResponse> {
        self.complete(messages).await
    }
}
