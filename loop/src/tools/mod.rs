mod builtin;
mod registry;

pub use builtin::{tool_exec, tool_glob, tool_grep, tool_read, tool_read_media, tool_write};
pub use registry::{parse_acp_tools, ToolRegistry};

pub type ToolResult = Result<ToolOutput, String>;

use crate::llm::ToolContent;

/// Result of a tool invocation that may carry multimodal content.
#[derive(Debug, Clone)]
pub enum ToolOutput {
    /// Plain text (from builtins).
    Text(String),
    /// Mixed content blocks (from MCP tools that return images).
    Blocks(Vec<ToolContent>),
}
