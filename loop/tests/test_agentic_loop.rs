use anyhow::Result;
use overloop::agentic_loop::{run, LoopConfig};
use overloop::llm::{
    CompletionResponse, Content, FunctionCall, Message, Role, StopReason, ToolCall, ToolDefinition,
    Usage,
};
use overloop::tools::ToolRegistry;
use overloop::traits::{AcpService, LlmService, NextPush, StreamedResponse};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

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

    // Verify two LLM calls were made (tool call + final text)
    assert!(llm.responses.lock().unwrap().is_empty());

    // Verify the tool result is in messages
    let tool_msgs: Vec<_> = messages.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(tool_msgs.len(), 1);
    let tool_output = tool_msgs[0].content.as_ref().unwrap().as_text().unwrap();
    assert!(tool_output.contains("hi"), "Tool output: {}", tool_output);
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
