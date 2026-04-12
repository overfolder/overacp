use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct McpRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct McpResponse {
    #[allow(dead_code)]
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<McpError>,
}

#[derive(Debug, Deserialize)]
pub struct McpError {
    #[allow(dead_code)]
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct McpToolListResult {
    pub tools: Vec<McpToolDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    #[serde(default)]
    #[serde(rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub enum McpContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource { resource: Value },
}

impl McpContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            McpContent::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_mcp_content_as_text() {
        let text = McpContent::Text { text: "hi".into() };
        assert_eq!(text.as_text(), Some("hi"));

        let img = McpContent::Image {
            data: "abc".into(),
            mime_type: "image/png".into(),
        };
        assert!(img.as_text().is_none());
    }

    #[test]
    fn test_mcp_image_content_deser_camel_case() {
        let val = json!({
            "type": "image",
            "data": "iVBORw0KGgo=",
            "mimeType": "image/png"
        });
        let content: McpContent = serde_json::from_value(val).unwrap();
        match content {
            McpContent::Image { data, mime_type } => {
                assert_eq!(data, "iVBORw0KGgo=");
                assert_eq!(mime_type, "image/png");
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn test_tool_result_deser() {
        let val = json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": true
        });
        let r: McpToolResult = serde_json::from_value(val).unwrap();
        assert!(r.is_error);
        assert_eq!(r.content.len(), 1);
    }

    #[test]
    fn test_tool_list_deser() {
        let val = json!({
            "tools": [{
                "name": "read",
                "description": "Read a file",
                "inputSchema": {"type": "object"}
            }]
        });
        let r: McpToolListResult = serde_json::from_value(val).unwrap();
        assert_eq!(r.tools.len(), 1);
        assert_eq!(r.tools[0].name, "read");
    }

    #[test]
    fn test_request_skip_none_params() {
        let req = McpRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "tools/list".into(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("params"));
    }
}
