//! Redacted chat-log snapshot used as the Langfuse generation `input`.
//!
//! Ported from the sister `overfolder/agent-runner` harness; adapted for
//! overloop's `Content` enum (which supports multimodal `Blocks`) and for
//! the absence of `<turn …>` system markers in this stack. Off by default
//! — callers gate on `Config::langfuse_capture_input`.

use std::fmt::Write as _;

use serde_json::Value;

use crate::llm::{Content, Message, Role};

/// Number of trailing messages rendered verbatim (after the system
/// prompt at index 0). Anything earlier is summarised as a count.
const DETAIL_TAIL: usize = 3;

const SYSTEM_TRUNCATE: usize = 200;
const ASSISTANT_TRUNCATE: usize = 300;
const TOOL_TRUNCATE: usize = 200;

/// Build a redacted plain-text chat-log snapshot suitable for the
/// Langfuse generation `input` field.
///
/// Shape:
/// - Older messages (past the trailing window) collapse to
///   `<N more messages...>`.
/// - Each retained message renders as `[ROLE]: body`, with per-role
///   truncation: system 200, assistant 300, tool 200, user full.
/// - Multimodal image/audio blocks render as `[image]` / `[media]`
///   placeholders so no base64/data-URI payload leaks into the trace.
pub fn build_context_snapshot(messages: &[Message]) -> Value {
    // Messages retained in detail: the trailing DETAIL_TAIL items,
    // always keeping the system prompt at index 0 hidden from detail
    // (it's already captured by Langfuse's trace-level metadata).
    let detail_start = messages.len().saturating_sub(DETAIL_TAIL).max(1);
    let skipped = detail_start.saturating_sub(1);

    let mut out = String::new();
    if skipped > 0 {
        let _ = writeln!(out, "<{skipped} more messages...>");
    }

    for m in messages.iter().skip(detail_start) {
        let body = content_to_text(m.content.as_ref());
        match m.role {
            Role::System => {
                let _ = write!(out, "\n[SYSTEM]: {}", truncate_str(&body, SYSTEM_TRUNCATE));
            }
            Role::User => {
                let _ = write!(out, "\n[USER]: {body}");
            }
            Role::Assistant => {
                let _ = write!(
                    out,
                    "\n[AGENT]: {}",
                    truncate_str(&body, ASSISTANT_TRUNCATE)
                );
            }
            Role::Tool => {
                let id = m.tool_call_id.as_deref().unwrap_or("?");
                let _ = write!(out, "\n[TOOL {id}]: {}", truncate_str(&body, TOOL_TRUNCATE));
            }
        }
    }

    Value::String(out)
}

/// Flatten a `Content` into a single string, replacing non-text blocks
/// with placeholders so no media payload reaches Langfuse.
fn content_to_text(content: Option<&Content>) -> String {
    use crate::llm::{ContentBlock, TypedBlock};

    let Some(c) = content else {
        return String::new();
    };
    match c {
        Content::Text(s) => s.clone(),
        Content::Blocks(blocks) => {
            let mut parts: Vec<String> = Vec::with_capacity(blocks.len());
            for b in blocks {
                match b {
                    TypedBlock::Known(ContentBlock::Text { text }) => parts.push(text.clone()),
                    TypedBlock::Known(ContentBlock::ImageUrl { .. })
                    | TypedBlock::Known(ContentBlock::Image { .. }) => parts.push("[image]".into()),
                    TypedBlock::Known(ContentBlock::InputAudio { .. }) => {
                        parts.push("[audio]".into())
                    }
                    TypedBlock::Unknown(_) => parts.push("[media]".into()),
                }
            }
            parts.join("\n")
        }
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("…[truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::llm::{ContentBlock, TypedBlock};

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: Some(Content::Text(text.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn tool_msg(id: &str, text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: Some(Content::Text(text.into())),
            tool_calls: None,
            tool_call_id: Some(id.into()),
        }
    }

    #[test]
    fn short_transcript_renders_every_message_past_system() {
        let messages = vec![
            msg(Role::System, "system prompt"),
            msg(Role::User, "hello"),
            msg(Role::Assistant, "hi"),
        ];
        let snap = build_context_snapshot(&messages);
        let s = snap.as_str().unwrap();
        assert!(!s.contains("more messages"));
        assert!(s.contains("[USER]: hello"));
        assert!(s.contains("[AGENT]: hi"));
        // System prompt (index 0) is NOT rendered in the detail window
        // when the transcript fits inside the trailing window.
        assert!(!s.contains("[SYSTEM]: system prompt"));
    }

    #[test]
    fn long_transcript_summarises_older_messages() {
        let mut messages = vec![msg(Role::System, "sys")];
        for i in 0..10 {
            messages.push(msg(Role::User, &format!("u{i}")));
            messages.push(msg(Role::Assistant, &format!("a{i}")));
        }
        // 1 system + 20 turns → len=21. detail_start = max(21-3, 1) = 18.
        // skipped = 18 - 1 = 17 (system prompt at idx 0 isn't counted).
        let snap = build_context_snapshot(&messages);
        let s = snap.as_str().unwrap();
        assert!(s.starts_with("<17 more messages...>"), "snapshot: {s}");
        // Last user + last assistant must be present verbatim.
        assert!(s.contains("[USER]: u9"));
        assert!(s.contains("[AGENT]: a9"));
        // First user must NOT be in the detail window.
        assert!(!s.contains("[USER]: u0"));
    }

    #[test]
    fn assistant_truncates_at_300() {
        let long = "x".repeat(500);
        let messages = vec![msg(Role::System, "s"), msg(Role::Assistant, &long)];
        let snap = build_context_snapshot(&messages);
        let s = snap.as_str().unwrap();
        assert!(s.contains("…[truncated]"));
        // Body + marker should be strictly less than the original.
        let agent_line = s.lines().find(|l| l.starts_with("[AGENT]:")).unwrap();
        assert!(agent_line.len() < long.len() + "[AGENT]: ".len());
    }

    #[test]
    fn user_is_not_truncated() {
        let long = "y".repeat(5_000);
        let messages = vec![msg(Role::System, "s"), msg(Role::User, &long)];
        let snap = build_context_snapshot(&messages);
        let s = snap.as_str().unwrap();
        assert!(s.contains(&long));
    }

    #[test]
    fn tool_output_truncates_and_labels_with_id() {
        let long = "t".repeat(500);
        let messages = vec![msg(Role::System, "s"), tool_msg("call_123", &long)];
        let snap = build_context_snapshot(&messages);
        let s = snap.as_str().unwrap();
        assert!(s.contains("[TOOL call_123]:"));
        assert!(s.contains("…[truncated]"));
    }

    #[test]
    fn multimodal_blocks_render_as_placeholders() {
        let m = Message {
            role: Role::User,
            content: Some(Content::Blocks(vec![
                TypedBlock::Known(ContentBlock::Text {
                    text: "see this".into(),
                }),
                TypedBlock::Known(ContentBlock::ImageUrl {
                    image_url: json!({"url": "data:image/png;base64,SECRET"}),
                }),
            ])),
            tool_calls: None,
            tool_call_id: None,
        };
        let messages = vec![msg(Role::System, "s"), m];
        let snap = build_context_snapshot(&messages);
        let s = snap.as_str().unwrap();
        assert!(s.contains("[USER]: see this"));
        assert!(s.contains("[image]"));
        assert!(!s.contains("SECRET"));
        assert!(!s.contains("data:image"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        // Multi-byte char straddling the cut point.
        let s = "aaa🦀bbb";
        let out = truncate_str(s, 4);
        assert!(out.ends_with("…[truncated]"));
        // No panic means boundary handling is correct.
    }
}
