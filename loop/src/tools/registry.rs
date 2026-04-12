use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
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
    acp_tools: HashSet<String>,
    acp_defs: Vec<ToolDefinition>,
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
            acp_tools: HashSet::new(),
            acp_defs: Vec::new(),
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

        self.register(
            "read_media",
            |a| Box::pin(super::tool_read_media(a)),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to a media file" },
                    "media_type": { "type": "string", "description": "MIME type override (e.g. image/png). Takes priority over extension-based detection." }
                },
                "required": ["path"]
            }),
            "Read an image file and return it as a visual content block the model can see.",
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

    /// Register operator-provided tools discovered via ACP `tools/list`.
    pub fn set_acp_tools(&mut self, tools: Vec<ToolDefinition>) {
        self.acp_tools.clear();
        self.acp_defs.clear();
        for def in tools {
            self.acp_tools.insert(def.function.name.clone());
            self.acp_defs.push(def);
        }
    }

    /// Check whether a tool is ACP-provided (executed via the broker,
    /// not locally).
    pub fn is_acp_tool(&self, name: &str) -> bool {
        self.acp_tools.contains(name)
    }

    /// All tool definitions (builtin + MCP + ACP) for the LLM.
    ///
    /// ACP tools shadow builtins and MCP tools with the same name —
    /// duplicates are filtered so the LLM sees each name exactly once.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut seen = HashSet::new();
        let mut defs = Vec::new();

        // ACP first (highest priority — operator controls the surface).
        for def in &self.acp_defs {
            seen.insert(def.function.name.clone());
            defs.push(def.clone());
        }
        // MCP second.
        for (_, def) in self.mcp_tools.values() {
            if seen.insert(def.function.name.clone()) {
                defs.push(def.clone());
            }
        }
        // Builtins last.
        for def in &self.builtin_defs {
            if seen.insert(def.function.name.clone()) {
                defs.push(def.clone());
            }
        }
        defs
    }

    /// Execute a tool by name.
    pub async fn execute(&mut self, name: &str, arguments: Value) -> Result<ToolOutput, String> {
        if let Some(func) = self.builtins.get(name) {
            return func(arguments).await;
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

/// Parse a `tools/list` response from the broker into tool
/// definitions. Expects `{"tools": [{"name", "description",
/// "inputSchema"}, ...]}` — the same shape as MCP `tools/list`.
pub fn parse_acp_tools(value: &Value) -> Vec<ToolDefinition> {
    let tools = match value.get("tools").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };
    tools
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_string();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("ACP tool")
                .to_string();
            let parameters = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            Some(ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDef {
                    name,
                    description,
                    parameters,
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_acp_tools_extracts_tools_from_broker_response() {
        let value = json!({
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Get the weather",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"]
                    }
                },
                {
                    "name": "search",
                    "description": "Search the web"
                }
            ]
        });
        let tools = parse_acp_tools(&value);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].function.name, "get_weather");
        assert_eq!(tools[0].function.description, "Get the weather");
        assert_eq!(tools[0].function.parameters["type"], "object");
        assert_eq!(tools[1].function.name, "search");
        // Missing inputSchema defaults to {"type": "object"}
        assert_eq!(tools[1].function.parameters["type"], "object");
    }

    #[test]
    fn parse_acp_tools_returns_empty_on_missing_tools_key() {
        assert!(parse_acp_tools(&json!({})).is_empty());
        assert!(parse_acp_tools(&json!({"tools": "not_array"})).is_empty());
    }

    #[test]
    fn parse_acp_tools_skips_entries_without_name() {
        let value = json!({"tools": [{"description": "no name"}, {"name": "ok"}]});
        let tools = parse_acp_tools(&value);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "ok");
    }

    #[test]
    fn parse_acp_tools_defaults_description() {
        let value = json!({"tools": [{"name": "bare"}]});
        let tools = parse_acp_tools(&value);
        assert_eq!(tools[0].function.description, "ACP tool");
    }

    #[test]
    fn set_acp_tools_populates_registry() {
        let mut registry = ToolRegistry::new();
        assert!(!registry.is_acp_tool("get_weather"));

        let tools = parse_acp_tools(&json!({
            "tools": [{"name": "get_weather", "description": "Weather"}]
        }));
        registry.set_acp_tools(tools);

        assert!(registry.is_acp_tool("get_weather"));
        assert!(!registry.is_acp_tool("nonexistent"));
    }

    #[test]
    fn definitions_include_acp_tools() {
        let mut registry = ToolRegistry::new();
        let builtin_count = registry.definitions().len();

        let tools = parse_acp_tools(&json!({
            "tools": [{"name": "remote_tool", "description": "Remote"}]
        }));
        registry.set_acp_tools(tools);

        let defs = registry.definitions();
        assert_eq!(defs.len(), builtin_count + 1);
        assert!(defs.iter().any(|d| d.function.name == "remote_tool"));
    }

    #[test]
    fn set_acp_tools_replaces_previous() {
        let mut registry = ToolRegistry::new();
        registry.set_acp_tools(parse_acp_tools(&json!({
            "tools": [{"name": "old_tool"}]
        })));
        assert!(registry.is_acp_tool("old_tool"));

        registry.set_acp_tools(parse_acp_tools(&json!({
            "tools": [{"name": "new_tool"}]
        })));
        assert!(!registry.is_acp_tool("old_tool"));
        assert!(registry.is_acp_tool("new_tool"));
    }

    #[test]
    fn acp_tool_shadows_builtin_in_definitions() {
        let mut registry = ToolRegistry::new();
        let builtin_count = registry.definitions().len();
        assert!(builtin_count > 0, "should have builtins");

        // Register an ACP tool with the same name as a builtin.
        registry.set_acp_tools(parse_acp_tools(&json!({
            "tools": [{"name": "read", "description": "ACP read override"}]
        })));

        let defs = registry.definitions();
        // Total count unchanged — ACP shadows the builtin, no duplicate.
        assert_eq!(defs.len(), builtin_count);

        // The ACP version wins.
        let read_def = defs.iter().find(|d| d.function.name == "read").unwrap();
        assert_eq!(read_def.function.description, "ACP read override");
    }
}
