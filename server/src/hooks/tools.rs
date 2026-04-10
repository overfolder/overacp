//! `ToolHost` hook — backs `tools/list` and `tools/call`.
//!
//! See `SPEC.md` § "The four hooks". Operators typically implement
//! this with a fan-out across one or more MCP servers; the broker
//! never inspects the payloads.

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;

use crate::auth::Claims;

/// Errors a `ToolHost` can return. Mapped to JSON-RPC application
/// error codes by the dispatch layer.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The named tool is not in the operator's catalogue.
    #[error("tool not found: {0}")]
    NotFound(String),
    /// The caller is not allowed to invoke this tool.
    #[error("tool denied: {0}")]
    Denied(String),
    /// The tool's execution failed.
    #[error("tool execution failed: {0}")]
    Execution(String),
    /// The operator's tool host hit an internal error.
    #[error("tool host failed: {0}")]
    Internal(String),
}

/// Operator hook for `tools/list` and `tools/call`.
///
/// Both methods take a borrowed [`Claims`] so the operator can
/// implement per-user or per-agent tool catalogues. The `req` and
/// returned `Value` are opaque to the broker — they pass through
/// to/from the agent unchanged.
#[async_trait]
pub trait ToolHost: Send + Sync + 'static {
    /// Return the available tools for this caller, in the shape the
    /// agent expects (typically `{ "tools": [ ... ] }`).
    async fn list(&self, claims: &Claims) -> Result<Value, ToolError>;

    /// Execute a tool call. The request shape is whatever the agent
    /// sends in `tools/call.params`; the result is whatever the
    /// agent expects to consume.
    async fn call(&self, claims: &Claims, req: Value) -> Result<Value, ToolError>;
}

/// Default `ToolHost` for the reference server. Reports an empty
/// catalogue and rejects any `call` with `ToolError::NotFound`. The
/// reference server installs this so `cargo run` boots without an
/// operator-supplied tool host.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultToolHost;

#[async_trait]
impl ToolHost for DefaultToolHost {
    async fn list(&self, _claims: &Claims) -> Result<Value, ToolError> {
        Ok(json!({ "tools": [] }))
    }

    async fn call(&self, _claims: &Claims, req: Value) -> Result<Value, ToolError> {
        let name = req
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        Err(ToolError::NotFound(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn claims() -> Claims {
        Claims::agent(Uuid::new_v4(), None, 60, "test")
    }

    #[tokio::test]
    async fn list_is_empty() {
        let host = DefaultToolHost;
        let result = host.list(&claims()).await.unwrap();
        assert_eq!(result, json!({ "tools": [] }));
    }

    #[tokio::test]
    async fn call_returns_not_found_with_tool_name() {
        let host = DefaultToolHost;
        let err = host
            .call(&claims(), json!({ "name": "echo", "arguments": {} }))
            .await
            .unwrap_err();
        let ToolError::NotFound(name) = err else {
            panic!("expected NotFound");
        };
        assert_eq!(name, "echo");
    }

    #[tokio::test]
    async fn call_handles_missing_name() {
        let host = DefaultToolHost;
        let err = host.call(&claims(), json!({})).await.unwrap_err();
        let ToolError::NotFound(name) = err else {
            panic!("expected NotFound");
        };
        assert_eq!(name, "<unknown>");
    }
}
