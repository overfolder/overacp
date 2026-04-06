use anyhow::Result;
use tracing::info;

use crate::llm::{Content, Message, Role};
use crate::traits::LlmService;

const COMPACTION_PROMPT: &str = "\
You are summarizing a conversation between a user and an AI assistant. \
Preserve all important context: decisions made, files modified, \
key findings, current task state, and any commitments or next steps. \
Be concise but complete — this summary replaces the original messages.";

/// Compact older messages into a summary when context is too large.
///
/// Keeps the system prompt and the last `keep_recent` messages intact.
/// Summarizes everything in between using an LLM call.
pub async fn compact_messages(
    llm: &(impl LlmService + ?Sized),
    messages: &[Message],
    keep_recent: usize,
) -> Result<Vec<Message>> {
    if messages.len() <= keep_recent + 2 {
        return Ok(messages.to_vec());
    }

    let system_msg = messages.first().filter(|m| m.role == Role::System).cloned();

    let start = if system_msg.is_some() { 1 } else { 0 };
    let split_point = messages.len().saturating_sub(keep_recent);

    if split_point <= start {
        return Ok(messages.to_vec());
    }

    let to_compact = &messages[start..split_point];
    let recent = &messages[split_point..];

    let summary = summarize(llm, to_compact).await?;
    info!(
        "Compacted {} messages into summary ({} chars)",
        to_compact.len(),
        summary.len()
    );

    let mut result = Vec::new();
    if let Some(sys) = system_msg {
        result.push(sys);
    }
    result.push(Message {
        role: Role::User,
        content: Some(Content::Text(format!(
            "[Conversation summary]\n{}",
            summary
        ))),
        tool_calls: None,
        tool_call_id: None,
    });
    result.push(Message {
        role: Role::Assistant,
        content: Some(Content::Text(
            "Understood. I have the conversation context from the summary. \
             Continuing from where we left off."
                .to_string(),
        )),
        tool_calls: None,
        tool_call_id: None,
    });
    result.extend_from_slice(recent);

    Ok(result)
}

async fn summarize(llm: &(impl LlmService + ?Sized), messages: &[Message]) -> Result<String> {
    let conversation_text = messages
        .iter()
        .filter_map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool",
                Role::System => "System",
            };
            m.content
                .as_ref()
                .and_then(|c| c.as_text())
                .map(|text| format!("{}: {}", role, text))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let summary_messages = vec![
        Message {
            role: Role::System,
            content: Some(Content::Text(COMPACTION_PROMPT.to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: Role::User,
            content: Some(Content::Text(format!(
                "Summarize this conversation:\n\n{}",
                conversation_text
            ))),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let response = llm.complete(&summary_messages).await?;

    response
        .choices
        .first()
        .and_then(|c| c.message.as_ref())
        .and_then(|m| m.content.as_ref())
        .and_then(|c| c.as_text())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("compaction returned empty response"))
}

/// Estimate token count from messages (rough: 1 token ≈ 4 chars).
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter_map(|m| m.content.as_ref())
        .filter_map(|c| c.as_text())
        .map(|t| t.len() / 4)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn test_estimate_tokens_text() {
        let msg = Message {
            role: Role::User,
            content: Some(Content::Text("a".repeat(100))),
            tool_calls: None,
            tool_call_id: None,
        };
        assert_eq!(estimate_tokens(&[msg]), 25);
    }

    #[test]
    fn test_estimate_tokens_no_content() {
        let msg = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: None,
            tool_call_id: None,
        };
        assert_eq!(estimate_tokens(&[msg]), 0);
    }
}
