use std::collections::HashSet;

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
    let proposed = messages.len().saturating_sub(keep_recent);
    let split_point = find_safe_split(messages, start, proposed);

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

/// Walk the proposed split backward until no tool_call in
/// `messages[start..split]` has its matching `Role::Tool` response
/// outside that range. Keeps assistant-with-tool_calls and their
/// responses together across the compaction boundary — providers
/// reject histories with dangling `tool_call_id`s.
fn find_safe_split(messages: &[Message], start: usize, proposed: usize) -> usize {
    let mut split = proposed.min(messages.len());
    while split > start && has_straddling_pair(messages, start, split) {
        split -= 1;
    }
    split
}

/// True if any tool call issued by an assistant in `messages[start..split]`
/// has no matching `Role::Tool` response within the same slice.
fn has_straddling_pair(messages: &[Message], start: usize, split: usize) -> bool {
    let mut unanswered: HashSet<&str> = HashSet::new();
    for msg in &messages[start..split] {
        match msg.role {
            Role::Assistant => {
                if let Some(calls) = msg.tool_calls.as_ref() {
                    for c in calls {
                        unanswered.insert(c.id.as_str());
                    }
                }
            }
            Role::Tool => {
                if let Some(id) = msg.tool_call_id.as_deref() {
                    unanswered.remove(id);
                }
            }
            _ => {}
        }
    }
    !unanswered.is_empty()
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
                .and_then(|c| c.extract_text())
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

/// Estimate token count from messages (rough: 1 token ≈ 4 chars for
/// text, flat 765 per image).
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter_map(|m| m.content.as_ref())
        .map(|c| c.estimate_tokens())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ContentBlock, FunctionCall, ToolCall, TypedBlock};
    use serde_json::json;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: Some(Content::Text(text.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn assistant_text(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: Some(Content::Text(text.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn assistant_tools(ids: &[&str]) -> Message {
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(
                ids.iter()
                    .map(|id| ToolCall {
                        id: (*id).into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "f".into(),
                            arguments: "{}".into(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
        }
    }

    fn tool_result(id: &str) -> Message {
        Message {
            role: Role::Tool,
            content: Some(Content::Text(format!("result-{id}"))),
            tool_calls: None,
            tool_call_id: Some(id.into()),
        }
    }

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

    #[test]
    fn test_safe_split_no_tool_calls() {
        let msgs = vec![
            user("q1"),
            assistant_text("a1"),
            user("q2"),
            assistant_text("a2"),
            user("q3"),
            assistant_text("a3"),
        ];
        // Proposed split anywhere is safe — no tool pairs to straddle.
        assert_eq!(find_safe_split(&msgs, 0, 3), 3);
        assert_eq!(find_safe_split(&msgs, 0, 5), 5);
    }

    #[test]
    fn test_safe_split_walks_past_tool_pair() {
        // [U, A(tc=a), T(a), done]   len=4
        let msgs = vec![
            user("q"),
            assistant_tools(&["a"]),
            tool_result("a"),
            assistant_text("done"),
        ];
        // proposed=2: to_compact=[U, A(tc=a)], recent=[T(a), done].
        // A(tc=a)'s response is in recent → unsafe. Walk back to 1:
        // to_compact=[U], no tool calls → safe.
        assert_eq!(find_safe_split(&msgs, 0, 2), 1);
        // proposed=3 is already safe: both A(tc=a) and T(a) in to_compact.
        assert_eq!(find_safe_split(&msgs, 0, 3), 3);
    }

    #[test]
    fn test_safe_split_multi_tool_chain() {
        // [U, A(tc=[a,b]), T(a), T(b), done]   len=5
        let msgs = vec![
            user("q"),
            assistant_tools(&["a", "b"]),
            tool_result("a"),
            tool_result("b"),
            assistant_text("done"),
        ];
        // proposed=3: A(tc=[a,b]) in compact, T(b) in recent → unsafe.
        // Walk back past A(tc=...) until to_compact has no unanswered
        // tool calls. Lands at 1 ([U] only).
        assert_eq!(find_safe_split(&msgs, 0, 3), 1);
        // proposed=4: to_compact includes both tool responses → safe.
        assert_eq!(find_safe_split(&msgs, 0, 4), 4);
    }

    #[test]
    fn test_safe_split_respects_start() {
        // System prompt at index 0; tool pair straddles the only
        // viable split — walk-back stops at `start` and returns it.
        let msgs = vec![
            Message {
                role: Role::System,
                content: Some(Content::Text("sys".into())),
                tool_calls: None,
                tool_call_id: None,
            },
            assistant_tools(&["a"]),
            tool_result("a"),
        ];
        // proposed=2 unsafe; walk-back to 1 unsafe (A still in compact);
        // walk-back stops at start=1 and returns 1.
        // (Caller guards `split <= start` and skips compaction.)
        assert_eq!(find_safe_split(&msgs, 1, 2), 1);
    }

    #[test]
    fn test_safe_split_earlier_pair_unaffected() {
        // An earlier completed tool pair shouldn't force walk-back if
        // the proposed split lands cleanly.
        // [U, A(tc=a), T(a), U', A'']   split at 4 → recent=[A'']
        let msgs = vec![
            user("q1"),
            assistant_tools(&["a"]),
            tool_result("a"),
            user("q2"),
            assistant_text("a2"),
        ];
        assert_eq!(find_safe_split(&msgs, 0, 4), 4);
    }

    #[test]
    fn test_estimate_tokens_with_image_blocks() {
        let msg = Message {
            role: Role::User,
            content: Some(Content::Blocks(vec![
                TypedBlock::Known(ContentBlock::Text {
                    text: "a".repeat(40),
                }),
                TypedBlock::Known(ContentBlock::ImageUrl {
                    image_url: json!({"url": "https://x/y.png"}),
                }),
            ])),
            tool_calls: None,
            tool_call_id: None,
        };
        // 40/4 + 765 = 775
        assert_eq!(estimate_tokens(&[msg]), 775);
    }
}
