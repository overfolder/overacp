use anyhow::Result;
use overloop::agentic_loop::{run, LoopConfig};
use overloop::llm::{
    CompletionResponse, Content, FunctionCall, Message, Role, StopReason, ToolCall, ToolDefinition,
    Usage,
};
use overloop::tools::ToolRegistry;
use overloop::traits::{AcpService, LlmService, NextPush, StreamedResponse};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// Captured `stream/toolCall` frame.
#[derive(Debug, Clone)]
struct ToolCallFrame {
    id: String,
    name: String,
    #[allow(dead_code)]
    arguments: Value,
}

/// Captured `stream/toolResult` frame.
#[derive(Debug, Clone)]
struct ToolResultFrame {
    id: String,
    #[allow(dead_code)]
    content: Value,
    is_error: bool,
}

// ─── Mock LLM ────────────────────────────────────────────────────

struct MockLlm {
    responses: Mutex<VecDeque<Result<StreamedResponse>>>,
}

impl MockLlm {
    fn new(responses: Vec<Result<StreamedResponse>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

impl LlmService for MockLlm {
    async fn stream_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<StreamedResponse> {
        let resp = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(anyhow::anyhow!("no more mock responses")))?;

        // Simulate streaming text deltas
        if let Some(content) = &resp.message.content {
            if let Some(text) = content.as_text() {
                on_text(text);
            }
        }

        Ok(resp)
    }

    async fn complete(&self, _messages: &[Message]) -> Result<CompletionResponse> {
        unimplemented!("complete not used in agentic loop tests")
    }
}

// ─── Mock ACP ────────────────────────────────────────────────────

struct MockAcp {
    text_deltas: Vec<String>,
    activities: Vec<String>,
    tool_calls_streamed: Vec<ToolCallFrame>,
    tool_results_streamed: Vec<ToolResultFrame>,
    quota_allowed: bool,
    turn_ended: bool,
    turn_end_usage: Option<Usage>,
    inbox: VecDeque<NextPush>,
}

impl MockAcp {
    fn new(quota_allowed: bool) -> Self {
        Self {
            text_deltas: Vec::new(),
            activities: Vec::new(),
            tool_calls_streamed: Vec::new(),
            tool_results_streamed: Vec::new(),
            quota_allowed,
            turn_ended: false,
            turn_end_usage: None,
            inbox: VecDeque::new(),
        }
    }
}

impl AcpService for MockAcp {
    fn stream_text_delta(&mut self, text: &str) -> Result<()> {
        self.text_deltas.push(text.to_string());
        Ok(())
    }

    fn stream_activity(&mut self, activity: &str) -> Result<()> {
        self.activities.push(activity.to_string());
        Ok(())
    }

    fn stream_tool_call(&mut self, id: &str, name: &str, arguments: &Value) -> Result<()> {
        self.tool_calls_streamed.push(ToolCallFrame {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.clone(),
        });
        Ok(())
    }

    fn stream_tool_result(&mut self, id: &str, content: &Value, is_error: bool) -> Result<()> {
        self.tool_results_streamed.push(ToolResultFrame {
            id: id.to_string(),
            content: content.clone(),
            is_error,
        });
        Ok(())
    }

    fn turn_end(&mut self, _messages: &[Message], usage: &Usage) -> Result<()> {
        self.turn_ended = true;
        self.turn_end_usage = Some(usage.clone());
        Ok(())
    }

    fn quota_check(&mut self) -> Result<bool> {
        Ok(self.quota_allowed)
    }

    fn quota_update(&mut self, _input_tokens: u64, _output_tokens: u64) -> Result<()> {
        Ok(())
    }

    fn next_push(&mut self) -> Result<NextPush> {
        self.inbox
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("mock inbox empty"))
    }

    fn heartbeat(&mut self) -> Result<()> {
        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────

fn make_text_response(text: &str, stop: StopReason) -> Result<StreamedResponse> {
    Ok(StreamedResponse {
        message: Message {
            role: Role::Assistant,
            content: Some(Content::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        finish_reason: Some(stop),
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    })
}

fn make_tool_call_response(tool_name: &str, arguments: &str) -> Result<StreamedResponse> {
    Ok(StreamedResponse {
        message: Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_001".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: tool_name.to_string(),
                    arguments: arguments.to_string(),
                },
            }]),
            tool_call_id: None,
        },
        finish_reason: Some(StopReason::ToolCalls),
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    })
}

fn default_config() -> LoopConfig {
    LoopConfig {
        max_iterations: 10,
        timeout: Duration::from_secs(30),
    }
}

fn user_msg(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Some(Content::Text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_simple_text_response() {
    let llm = MockLlm::new(vec![make_text_response("Hello!", StopReason::Stop)]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("Hi")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    assert!(acp.turn_ended);
    assert!(acp.turn_end_usage.is_some());
    assert!(
        acp.text_deltas.iter().any(|d| d.contains("Hello!")),
        "Expected 'Hello!' in text_deltas: {:?}",
        acp.text_deltas
    );
}

#[tokio::test]
async fn test_tool_call_round_trip() {
    let llm = MockLlm::new(vec![
        make_tool_call_response("exec", r#"{"command": "echo hi"}"#),
        make_text_response("Done!", StopReason::Stop),
    ]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("run echo hi")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    // Verify tool was executed
    assert!(
        acp.activities.iter().any(|a| a.contains("exec")),
        "Expected tool activity: {:?}",
        acp.activities
    );

    // Machine-readable observability: stream/toolCall + stream/toolResult
    // should each have fired exactly once, with matching ids.
    assert_eq!(acp.tool_calls_streamed.len(), 1);
    assert_eq!(acp.tool_results_streamed.len(), 1);
    assert_eq!(acp.tool_calls_streamed[0].name, "exec");
    assert_eq!(acp.tool_calls_streamed[0].id, "call_001");
    assert_eq!(acp.tool_results_streamed[0].id, "call_001");
    assert!(!acp.tool_results_streamed[0].is_error);

    // Verify two LLM calls were made (tool call + final text)
    assert!(llm.responses.lock().unwrap().is_empty());

    // Verify the tool result is in messages
    let tool_msgs: Vec<_> = messages.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(tool_msgs.len(), 1);
    let tool_output = tool_msgs[0].content.as_ref().unwrap().as_text().unwrap();
    assert!(tool_output.contains("hi"), "Tool output: {}", tool_output);
}

#[tokio::test]
async fn test_tool_call_failure_streams_is_error_true() {
    // `_unknown_tool_` is not registered → ToolRegistry::execute errors
    // → stream_tool_result should fire with is_error = true.
    let llm = MockLlm::new(vec![
        make_tool_call_response("_unknown_tool_", r#"{}"#),
        make_text_response("sorry", StopReason::Stop),
    ]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("call the unknown tool")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    assert_eq!(acp.tool_calls_streamed.len(), 1);
    assert_eq!(acp.tool_results_streamed.len(), 1);
    assert!(
        acp.tool_results_streamed[0].is_error,
        "tool_result.is_error should be true on failure"
    );
    assert_eq!(
        acp.tool_calls_streamed[0].id, acp.tool_results_streamed[0].id,
        "call and result ids must match"
    );
}

#[tokio::test]
async fn test_quota_exhausted() {
    let llm = MockLlm::new(vec![]);
    let mut acp = MockAcp::new(false);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("Hi")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    assert!(
        acp.text_deltas
            .iter()
            .any(|d| d.contains("[Quota exhausted]")),
        "Expected quota exhausted message: {:?}",
        acp.text_deltas
    );
}

#[tokio::test]
async fn test_timeout() {
    let llm = MockLlm::new(vec![]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("Hi")];

    let config = LoopConfig {
        max_iterations: 10,
        timeout: Duration::from_millis(0),
    };

    run(&mut acp, &llm, &mut registry, &mut messages, &config)
        .await
        .unwrap();

    assert!(
        acp.text_deltas
            .iter()
            .any(|d| d.contains("[Session timed out]")),
        "Expected timeout message: {:?}",
        acp.text_deltas
    );
}

#[tokio::test]
async fn test_llm_error() {
    let llm = MockLlm::new(vec![Err(anyhow::anyhow!("model overloaded"))]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("Hi")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    assert!(
        acp.text_deltas.iter().any(|d| d.contains("[LLM error")),
        "Expected LLM error message: {:?}",
        acp.text_deltas
    );
}

#[tokio::test]
async fn test_content_length_stop() {
    let llm = MockLlm::new(vec![make_text_response("truncated...", StopReason::Length)]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("Hi")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    assert!(
        acp.text_deltas
            .iter()
            .any(|d| d.contains("[Response truncated")),
        "Expected truncation message: {:?}",
        acp.text_deltas
    );
}

#[tokio::test]
async fn test_content_filter_stop() {
    let llm = MockLlm::new(vec![make_text_response(
        "sorry",
        StopReason::ContentFilter,
    )]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("Hi")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    assert!(
        acp.text_deltas
            .iter()
            .any(|d| d.contains("[Content filtered]")),
        "Expected content-filter message: {:?}",
        acp.text_deltas
    );
}

#[tokio::test]
async fn test_wind_down_injects_system_message_when_iterations_remaining_equals_5() {
    // Config with max_iterations = 5 means the FIRST iteration has
    // `remaining == 5`, so the wind-down branch fires immediately.
    // The loop then runs the LLM call and the natural-stop branch
    // exits after one turn, leaving the wind-down system message
    // in `messages`.
    let llm = MockLlm::new(vec![make_text_response("ok", StopReason::Stop)]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("hi")];

    let config = LoopConfig {
        max_iterations: 5,
        timeout: Duration::from_secs(30),
    };

    run(&mut acp, &llm, &mut registry, &mut messages, &config)
        .await
        .unwrap();

    let wind_down = messages.iter().any(|m| {
        m.role == Role::System
            && m.content
                .as_ref()
                .and_then(|c| c.as_text())
                .map(|t| t.contains("iterations remaining"))
                .unwrap_or(false)
    });
    assert!(
        wind_down,
        "wind-down system message not injected; messages = {messages:#?}"
    );
}

#[tokio::test]
async fn test_silence_nudge_injected_after_three_silent_turns() {
    // Three empty-text, no-tool responses in a row trip `silent_turns
    // >= 3`; the 4th iteration injects the nudge. Add a final real
    // response so the loop terminates.
    fn empty_assistant_response() -> Result<StreamedResponse> {
        Ok(StreamedResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            // Emit finish_reason = ToolCalls so the loop `continue`s
            // past the stop-reason match without breaking, letting
            // `silent_turns` accumulate across iterations.
            finish_reason: Some(StopReason::ToolCalls),
            usage: Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 0,
                total_tokens: 1,
            }),
        })
    }

    let llm = MockLlm::new(vec![
        empty_assistant_response(),
        empty_assistant_response(),
        empty_assistant_response(),
        // 4th iteration: after silence_nudge triggers and resets,
        // emit a real text response to let the loop exit cleanly.
        make_text_response("done", StopReason::Stop),
    ]);
    let mut acp = MockAcp::new(true);
    let mut registry = ToolRegistry::new();
    let mut messages = vec![user_msg("go")];

    run(
        &mut acp,
        &llm,
        &mut registry,
        &mut messages,
        &default_config(),
    )
    .await
    .unwrap();

    // The nudge is a System message with the SILENCE_NUDGE text.
    let nudge = messages.iter().any(|m| {
        m.role == Role::System
            && m.content
                .as_ref()
                .and_then(|c| c.as_text())
                .map(|t| t.contains("haven't produced any output"))
                .unwrap_or(false)
    });
    assert!(
        nudge,
        "silence nudge not injected; messages = {messages:#?}"
    );
}
