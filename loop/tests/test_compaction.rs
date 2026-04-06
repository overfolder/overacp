use anyhow::Result;
use overloop::compaction::compact_messages;
use overloop::llm::{Choice, CompletionResponse, Content, Message, Role, ToolDefinition, Usage};
use overloop::traits::{LlmService, StreamedResponse};

struct MockLlm {
    summary: String,
}

impl LlmService for MockLlm {
    async fn stream_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<StreamedResponse> {
        unimplemented!("stream_completion not used in compaction tests")
    }

    async fn complete(&self, _messages: &[Message]) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            choices: vec![Choice {
                message: Some(Message {
                    role: Role::Assistant,
                    content: Some(Content::Text(self.summary.clone())),
                    tool_calls: None,
                    tool_call_id: None,
                }),
                delta: None,
                finish_reason: None,
            }],
            usage: Some(Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            }),
        })
    }
}

fn make_msg(role: Role, text: &str) -> Message {
    Message {
        role,
        content: Some(Content::Text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

fn make_conversation(count: usize) -> Vec<Message> {
    let mut msgs = Vec::new();
    for i in 0..count {
        if i % 2 == 0 {
            msgs.push(make_msg(Role::User, &format!("User message {}", i)));
        } else {
            msgs.push(make_msg(Role::Assistant, &format!("Assistant reply {}", i)));
        }
    }
    msgs
}

#[tokio::test]
async fn test_short_no_compaction() {
    let llm = MockLlm {
        summary: "should not be called".to_string(),
    };

    let messages = vec![
        make_msg(Role::User, "hello"),
        make_msg(Role::Assistant, "hi"),
        make_msg(Role::User, "how are you"),
    ];

    let result = compact_messages(&llm, &messages, 4).await.unwrap();
    // 3 messages <= keep_recent(4) + 2, so no compaction
    assert_eq!(result.len(), 3);
}

#[tokio::test]
async fn test_preserves_system_prompt() {
    let llm = MockLlm {
        summary: "Conversation summary".to_string(),
    };

    let mut messages = vec![make_msg(Role::System, "You are helpful.")];
    // Add 15 user/assistant messages
    messages.extend(make_conversation(15));

    let result = compact_messages(&llm, &messages, 5).await.unwrap();

    // First message should be the system prompt
    assert_eq!(result[0].role, Role::System);
    assert_eq!(
        result[0].content.as_ref().unwrap().as_text().unwrap(),
        "You are helpful."
    );
}

#[tokio::test]
async fn test_compact_structure() {
    let llm = MockLlm {
        summary: "Summary of earlier conversation.".to_string(),
    };

    let mut messages = vec![make_msg(Role::System, "You are helpful.")];
    messages.extend(make_conversation(15));

    let result = compact_messages(&llm, &messages, 5).await.unwrap();

    // Structure: [system, summary_user, ack_assistant, ...5 recent]
    assert_eq!(result.len(), 8);
    assert_eq!(result[0].role, Role::System);
    assert_eq!(result[1].role, Role::User);
    assert!(result[1]
        .content
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap()
        .contains("[Conversation summary]"));
    assert_eq!(result[2].role, Role::Assistant);
    assert!(result[2]
        .content
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap()
        .contains("Understood"));
}

#[tokio::test]
async fn test_no_system_prompt() {
    let llm = MockLlm {
        summary: "Summarized.".to_string(),
    };

    let messages = make_conversation(15);

    let result = compact_messages(&llm, &messages, 5).await.unwrap();

    // No system prompt in result
    assert_ne!(result[0].role, Role::System);
    // First should be the summary user message
    assert_eq!(result[0].role, Role::User);
    assert!(result[0]
        .content
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap()
        .contains("[Conversation summary]"));
}
