use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::debug;

use crate::llm::{FunctionDef, ToolDefinition};

use super::{McpRequest, McpResponse, McpToolDef, McpToolResult};

static MCP_REQUEST_ID: AtomicU64 = AtomicU64::new(10000);

fn next_id() -> u64 {
    MCP_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

pub struct McpClient {
    client: Client,
    url: String,
    session_id: Option<String>,
}

impl McpClient {
    pub fn new(url: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("build MCP http client");

        Self {
            client,
            url: url.trim_end_matches('/').to_string(),
            session_id: None,
        }
    }

    /// Initialize the MCP connection.
    pub async fn initialize(&mut self) -> Result<()> {
        let resp = self
            .send(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "overloop",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
            )
            .await?;

        debug!("MCP initialized: {:?}", resp);
        Ok(())
    }

    /// Discover tools from the MCP server.
    pub async fn list_tools(&mut self) -> Result<Vec<ToolDefinition>> {
        self.initialize().await?;

        let result = self.send("tools/list", None).await?;
        let list: super::McpToolListResult =
            serde_json::from_value(result).context("parse tools/list")?;

        Ok(list.tools.into_iter().map(mcp_to_tool_definition).collect())
    }

    /// Call an MCP tool by name.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<String> {
        let result = self
            .send(
                "tools/call",
                Some(serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                })),
            )
            .await?;

        let tool_result: McpToolResult =
            serde_json::from_value(result).context("parse tools/call")?;

        if tool_result.is_error {
            let msg = tool_result
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("MCP tool error: {}", msg);
        }

        let text = tool_result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(text)
    }

    async fn send(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let req = McpRequest {
            jsonrpc: "2.0",
            id: next_id(),
            method: method.to_string(),
            params,
        };

        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json");

        if let Some(sid) = &self.session_id {
            request = request.header("Mcp-Session-Id", sid);
        }

        let response = request
            .json(&req)
            .send()
            .await
            .context("MCP request failed")?;

        // Capture session ID from response headers
        if let Some(sid) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(sid.to_string());
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MCP {} error {}: {}", method, status, body);
        }

        let resp: McpResponse = response
            .json()
            .await
            .context("parse MCP JSON-RPC response")?;

        if let Some(err) = resp.error {
            anyhow::bail!("MCP error: {}", err.message);
        }

        resp.result
            .ok_or_else(|| anyhow::anyhow!("MCP response missing result"))
    }
}

fn mcp_to_tool_definition(mcp: McpToolDef) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDef {
            name: mcp.name,
            description: mcp.description.unwrap_or_else(|| "MCP tool".to_string()),
            parameters: mcp.input_schema,
        },
    }
}

// Silence unused warning for session_id field assignment
impl Drop for McpClient {
    fn drop(&mut self) {
        drop(self.session_id.take());
    }
}
