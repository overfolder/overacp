mod builtin;
mod registry;

pub use builtin::{tool_exec, tool_glob, tool_grep, tool_read, tool_write};
pub use registry::ToolRegistry;

pub type ToolResult = Result<String, String>;
