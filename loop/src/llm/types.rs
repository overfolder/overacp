use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Content {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            Content::Blocks(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct CompletionResponse {
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: Option<Message>,
    pub delta: Option<Delta>,
    pub finish_reason: Option<StopReason>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Delta {
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallDelta {
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<FunctionCallDelta>,
}

#[derive(Debug, Deserialize)]
pub struct FunctionCallDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

#[derive(Debug, Deserialize)]
pub struct StreamEvent {
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_role_serde() {
        for (variant, expected) in [
            (Role::System, "system"),
            (Role::User, "user"),
            (Role::Assistant, "assistant"),
            (Role::Tool, "tool"),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, format!("\"{}\"", expected));
            let back: Role = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn test_message_roundtrip() {
        let msg = Message {
            role: Role::Assistant,
            content: Some(Content::Text("hello".into())),
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "foo".into(),
                    arguments: "{}".into(),
                },
            }]),
            tool_call_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, Role::Assistant);
        assert!(back.content.is_some());
        assert_eq!(back.tool_calls.unwrap().len(), 1);
    }

    #[test]
    fn test_content_as_text() {
        let text = Content::Text("hi".into());
        assert_eq!(text.as_text(), Some("hi"));

        let blocks = Content::Blocks(vec![ContentBlock::Text { text: "b".into() }]);
        assert!(blocks.as_text().is_none());
    }

    #[test]
    fn test_stop_reason_deser() {
        for (s, expected) in [
            ("\"stop\"", StopReason::Stop),
            ("\"tool_calls\"", StopReason::ToolCalls),
            ("\"length\"", StopReason::Length),
            ("\"content_filter\"", StopReason::ContentFilter),
        ] {
            let r: StopReason = serde_json::from_str(s).unwrap();
            assert_eq!(r, expected);
        }
    }

    #[test]
    fn test_usage_deser() {
        let with: Usage = serde_json::from_value(
            json!({"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}),
        )
        .unwrap();
        assert_eq!(with.total_tokens, 15);

        let without: Usage =
            serde_json::from_value(json!({"prompt_tokens":10,"completion_tokens":5})).unwrap();
        assert_eq!(without.total_tokens, 0);
    }
}
