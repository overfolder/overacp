use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::result::Result as StdResult;
use std::time::{Duration, Instant};
use tracing::{debug, error, warn};

use crate::traits::{LlmService, StreamedResponse};
use tokio::time::sleep;

use super::retry::{
    classify_http_response, classify_reqwest_error, escalate_if_transient, RetryBudget, StreamError,
};
use super::{
    CompletionResponse, Content, Delta, FunctionCall, Message, Role, StreamEvent, ToolCall,
    ToolDefinition,
};

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

        let mut budget = RetryBudget::default_budget();
        let mut retry_idx: u32 = 0;
        loop {
            let attempt_num = retry_idx + 1;
            let mut emitted = false;
            let outcome = match self.post_or_classify(&body).await {
                Ok(resp) => self.process_stream(resp, on_text, &mut emitted).await,
                Err(e) => Err(e),
            };

            match outcome {
                Ok(v) => return Ok(v),
                Err(mut e) => {
                    if emitted && e.is_retryable() {
                        e = e.into_fatal_with_prefix(
                            "mid-stream error after partial output emitted; cannot retry safely",
                        );
                    }
                    if let Some(next) = self
                        .handle_failure(retry_idx, attempt_num, budget, e)
                        .await?
                    {
                        budget = next.budget;
                        retry_idx += 1;
                        continue;
                    }
                    unreachable!("handle_failure returns Err on exhaustion");
                }
            }
        }
    }

    /// Non-streaming completion (used by compaction).
    pub async fn complete(&self, messages: &[Message]) -> Result<CompletionResponse> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
        });

        let mut budget = RetryBudget::default_budget();
        let mut retry_idx: u32 = 0;
        loop {
            let attempt_num = retry_idx + 1;
            let outcome: StdResult<CompletionResponse, StreamError> = async {
                let resp = self.post_or_classify(&body).await?;
                let attempt_start = Instant::now();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| classify_reqwest_error(e, attempt_start.elapsed()))?;
                serde_json::from_str::<CompletionResponse>(&text).map_err(|e| StreamError::Fatal {
                    message: format!("parse completion response: {e}"),
                    source: Box::new(e),
                })
            }
            .await;

            match outcome {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if let Some(next) = self
                        .handle_failure(retry_idx, attempt_num, budget, e)
                        .await?
                    {
                        budget = next.budget;
                        retry_idx += 1;
                        continue;
                    }
                    unreachable!("handle_failure returns Err on exhaustion");
                }
            }
        }
    }

    /// Report the failure, upgrade the budget if the body names a transient
    /// cause, and either return `Ok(Some(next))` with a fresh budget (caller
    /// continues the loop) or `Err(_)` when the error is fatal / budget
    /// exhausted.
    async fn handle_failure(
        &self,
        retry_idx: u32,
        attempt_num: u32,
        budget: RetryBudget,
        err: StreamError,
    ) -> Result<Option<NextAttempt>> {
        self.report_llm_error(retry_idx, &err.to_string());
        let budget = escalate_if_transient(budget, &err);

        if !err.is_retryable() || attempt_num >= budget.max_attempts {
            return Err(anyhow::Error::new(err));
        }

        let delay = budget.delay_for(retry_idx);
        warn!(
            provider = "openai-compatible",
            model = %self.model,
            attempt = attempt_num + 1,
            max_attempts = budget.max_attempts,
            backoff_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            escalated = budget.escalated,
            "retrying LLM request"
        );
        sleep(delay).await;
        Ok(Some(NextAttempt { budget }))
    }

    /// POST the request once. On non-2xx, consume the body and return a
    /// classified [`StreamError`]. On transport error, classify that too.
    async fn post_or_classify(&self, body: &Value) -> StdResult<reqwest::Response, StreamError> {
        let url = format!("{}/chat/completions", self.api_url);
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
            Ok(resp) if resp.status().is_success() => Ok(resp),
            Ok(resp) => {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                Err(classify_http_response(status, body_text))
            }
            Err(e) => Err(classify_reqwest_error(e, attempt_start.elapsed())),
        }
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
        emitted: &mut bool,
    ) -> StdResult<StreamedResponse, StreamError> {
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
                Err(e) => return Err(classify_reqwest_error(e, stream_start.elapsed())),
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
                        *emitted = true;
                    }

                    if let Some(Delta {
                        tool_calls: Some(deltas),
                        ..
                    }) = &choice.delta
                    {
                        for tc_delta in deltas {
                            accumulate_tool_call(&mut tool_calls, tc_delta);
                        }
                        *emitted = true;
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

struct NextAttempt {
    budget: RetryBudget,
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
