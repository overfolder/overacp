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
    Blocks(Vec<TypedBlock>),
}

impl Content {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            Content::Blocks(_) => None,
        }
    }

    /// Extract all text from the content, joining blocks with newlines.
    /// Returns `None` when no text is present.
    pub fn extract_text(&self) -> Option<String> {
        match self {
            Content::Text(s) => Some(s.clone()),
            Content::Blocks(blocks) => {
                let texts: Vec<&str> = blocks.iter().filter_map(|b| b.as_text()).collect();
                if texts.is_empty() {
                    None
                } else {
                    Some(texts.join("\n"))
                }
            }
        }
    }

    /// Rough token estimate including media blocks.
    pub fn estimate_tokens(&self) -> usize {
        match self {
            Content::Text(s) => s.len() / 4,
            Content::Blocks(blocks) => blocks.iter().map(|b| b.estimate_tokens()).sum(),
        }
    }
}

/// A single content block inside a multimodal message.
///
/// Known block types are parsed into typed variants. See
/// [`TypedBlock::Unknown`] for how unrecognised modalities are
/// captured verbatim to round-trip without data loss.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: Value },
    /// Anthropic-style base64 image block.
    #[serde(rename = "image")]
    Image { source: Value },
    /// OpenAI audio input block.
    #[serde(rename = "input_audio")]
    InputAudio { input_audio: Value },
}

/// Wrapper that tries to parse a typed [`ContentBlock`] and, on
/// failure, preserves the raw JSON so unknown block types survive
/// round-trips losslessly.
#[derive(Debug, Clone)]
pub enum TypedBlock {
    Known(ContentBlock),
    /// Unrecognised block type — raw JSON preserved for round-trip.
    Unknown(Value),
}

impl Serialize for TypedBlock {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            TypedBlock::Known(block) => block.serialize(serializer),
            TypedBlock::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for TypedBlock {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match serde_json::from_value::<ContentBlock>(value.clone()) {
            Ok(block) => Ok(TypedBlock::Known(block)),
            Err(_) => Ok(TypedBlock::Unknown(value)),
        }
    }
}

impl TypedBlock {
    /// Extract text content, returning `None` for non-text blocks.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            TypedBlock::Known(ContentBlock::Text { text }) => Some(text),
            _ => None,
        }
    }

    /// Whether this block carries binary/opaque media.
    pub fn is_media(&self) -> bool {
        matches!(
            self,
            TypedBlock::Known(
                ContentBlock::ImageUrl { .. }
                    | ContentBlock::Image { .. }
                    | ContentBlock::InputAudio { .. }
            )
        )
    }

    /// Rough token estimate for a single block.
    /// Text: len/4. Images: flat 765 tokens (OpenAI "low detail").
    pub fn estimate_tokens(&self) -> usize {
        match self {
            TypedBlock::Known(ContentBlock::Text { text }) => text.len() / 4,
            TypedBlock::Known(ContentBlock::ImageUrl { .. } | ContentBlock::Image { .. }) => 765,
            TypedBlock::Known(ContentBlock::InputAudio { .. }) | TypedBlock::Unknown(_) => 0,
        }
    }
}

/// A single piece of content returned by a tool invocation. Lives in
/// the `llm` module (rather than `tools`) to avoid a circular
/// dependency between `tools` and `mcp`.
#[derive(Debug, Clone)]
pub enum ToolContent {
    Text(String),
    ImageBase64 { data: String, mime_type: String },
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

        let blocks = Content::Blocks(vec![TypedBlock::Known(ContentBlock::Text {
            text: "b".into(),
        })]);
        assert!(blocks.as_text().is_none());
    }

    #[test]
    fn test_content_extract_text() {
        let text = Content::Text("hi".into());
        assert_eq!(text.extract_text(), Some("hi".into()));

        let blocks = Content::Blocks(vec![
            TypedBlock::Known(ContentBlock::Text { text: "a".into() }),
            TypedBlock::Known(ContentBlock::ImageUrl {
                image_url: json!({"url": "https://x/y.png"}),
            }),
            TypedBlock::Known(ContentBlock::Text { text: "b".into() }),
        ]);
        assert_eq!(blocks.extract_text(), Some("a\nb".into()));

        let media_only = Content::Blocks(vec![TypedBlock::Known(ContentBlock::ImageUrl {
            image_url: json!({"url": "https://x/y.png"}),
        })]);
        assert!(media_only.extract_text().is_none());
    }

    #[test]
    fn test_content_estimate_tokens() {
        let text = Content::Text("a".repeat(100));
        assert_eq!(text.estimate_tokens(), 25);

        let blocks = Content::Blocks(vec![
            TypedBlock::Known(ContentBlock::Text {
                text: "a".repeat(40),
            }),
            TypedBlock::Known(ContentBlock::ImageUrl {
                image_url: json!({"url": "https://x/y.png"}),
            }),
        ]);
        // 40/4 + 765 = 775
        assert_eq!(blocks.estimate_tokens(), 775);
    }

    #[test]
    fn test_typed_block_as_text() {
        let text_block = TypedBlock::Known(ContentBlock::Text { text: "hi".into() });
        assert_eq!(text_block.as_text(), Some("hi"));

        let img = TypedBlock::Known(ContentBlock::ImageUrl {
            image_url: json!({"url": "x"}),
        });
        assert!(img.as_text().is_none());
        assert!(img.is_media());

        assert!(!text_block.is_media());
    }

    #[test]
    fn test_unknown_block_deser_preserves_data_on_roundtrip() {
        let original = json!({"type": "hologram", "data": "xyz"});
        let block: TypedBlock = serde_json::from_value(original.clone()).unwrap();
        assert!(matches!(block, TypedBlock::Unknown(_)));
        assert!(!block.is_media());
        assert_eq!(block.estimate_tokens(), 0);
        assert!(block.as_text().is_none());

        // Round-trip: serializing preserves the original JSON
        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(serialized, original);
    }

    #[test]
    fn test_image_block_deser() {
        let val = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc"}});
        let block: TypedBlock = serde_json::from_value(val).unwrap();
        assert!(matches!(
            block,
            TypedBlock::Known(ContentBlock::Image { .. })
        ));
        assert!(block.is_media());
        assert_eq!(block.estimate_tokens(), 765);
    }

    #[test]
    fn test_input_audio_block_deser() {
        let val =
            json!({"type": "input_audio", "input_audio": {"data": "base64audio", "format": "wav"}});
        let block: TypedBlock = serde_json::from_value(val).unwrap();
        assert!(matches!(
            block,
            TypedBlock::Known(ContentBlock::InputAudio { .. })
        ));
        assert!(block.is_media());
        assert_eq!(block.estimate_tokens(), 0);
    }

    #[test]
    fn test_content_blocks_with_unknown_roundtrip() {
        let original = json!([
            {"type": "text", "text": "hello"},
            {"type": "future_type", "payload": [1,2,3]}
        ]);
        let content: Content = serde_json::from_value(original.clone()).unwrap();
        match &content {
            Content::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].as_text(), Some("hello"));
                assert!(matches!(&blocks[1], TypedBlock::Unknown(_)));
            }
            _ => panic!("expected Blocks"),
        }
        // Round-trip preserves both known and unknown blocks
        let serialized = serde_json::to_value(&content).unwrap();
        assert_eq!(serialized, original);
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
