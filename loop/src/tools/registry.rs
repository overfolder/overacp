use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::llm::{FunctionDef, ToolContent, ToolDefinition};
use crate::mcp::McpClient;

use super::{ToolOutput, ToolResult};

type ToolFn = fn(Value) -> Pin<Box<dyn Future<Output = ToolResult> + Send>>;

pub struct ToolRegistry {
    builtins: HashMap<String, ToolFn>,
    builtin_defs: Vec<ToolDefinition>,
    mcp_clients: Vec<McpClient>,
    mcp_tools: HashMap<String, (usize, ToolDefinition)>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            builtins: HashMap::new(),
            builtin_defs: Vec::new(),
            mcp_clients: Vec::new(),
            mcp_tools: HashMap::new(),
        };
        registry.register_builtins();
        registry
    }

    fn register_builtins(&mut self) {
        self.register(
            "read",
            |a| Box::pin(super::tool_read(a)),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to read" },
                    "offset": { "type": "integer", "description": "Line offset (0-based)" },
                    "limit": { "type": "integer", "description": "Max lines to read" }
                },
                "required": ["path"]
            }),
            "Read a file with optional line offset and limit.",
        );

        self.register(
            "write",
            |a| Box::pin(super::tool_write(a)),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to write" },
                    "content": { "type": "string", "description": "File content" }
                },
                "required": ["path", "content"]
            }),
            "Write content to a file, creating parent directories.",
        );

        self.register("exec", |a| Box::pin(super::tool_exec(a)), json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Bash command to execute" },
                "timeout": { "type": "integer", "description": "Timeout in seconds (default 120)" }
            },
            "required": ["command"]
        }), "Execute a bash command with timeout.");

        self.register(
            "glob",
            |a| Box::pin(super::tool_glob(a)),
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern" },
                    "path": { "type": "string", "description": "Directory to search (default .)" }
                },
                "required": ["pattern"]
            }),
            "Find files matching a glob pattern.",
        );

        self.register(
            "grep",
            |a| Box::pin(super::tool_grep(a)),
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Search pattern (regex)" },
                    "path": { "type": "string", "description": "Directory to search (default .)" }
                },
                "required": ["pattern"]
            }),
            "Search file contents with grep.",
        );
    }

    fn register(&mut self, name: &str, func: ToolFn, parameters: Value, description: &str) {
        self.builtins.insert(name.to_string(), func);
        self.builtin_defs.push(ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: name.to_string(),
                description: description.to_string(),
                parameters,
            },
        });
    }

    /// Connect to an MCP server and discover its tools.
    pub async fn connect_mcp(&mut self, url: &str) -> anyhow::Result<()> {
        let mut client = McpClient::new(url);
        let tools = client.list_tools().await?;
        let idx = self.mcp_clients.len();

        for tool_def in tools {
            self.mcp_tools
                .insert(tool_def.function.name.clone(), (idx, tool_def));
        }

        self.mcp_clients.push(client);
        Ok(())
    }

    /// All tool definitions (builtin + MCP) for the LLM.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut defs = self.builtin_defs.clone();
        for (_, def) in self.mcp_tools.values() {
            defs.push(def.clone());
        }
        defs
    }

    /// Execute a tool by name.
    pub async fn execute(&mut self, name: &str, arguments: Value) -> Result<ToolOutput, String> {
        if let Some(func) = self.builtins.get(name) {
            return func(arguments).await.map(ToolOutput::Text);
        }

        if let Some((client_idx, _)) = self.mcp_tools.get(name) {
            let idx = *client_idx;
            let contents = self.mcp_clients[idx]
                .call_tool(name, arguments)
                .await
                .map_err(|e| e.to_string())?;

            // If all content is text, collapse to a single string for
            // simpler downstream handling.
            if contents.iter().all(|c| matches!(c, ToolContent::Text(_))) {
                let text = contents
                    .into_iter()
                    .filter_map(|c| match c {
                        ToolContent::Text(t) => Some(t),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(ToolOutput::Text(text));
            }

            return Ok(ToolOutput::Blocks(contents));
        }

        Err(format!("unknown tool: {}", name))
    }
}
