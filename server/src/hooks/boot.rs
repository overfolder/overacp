//! `BootProvider` hook — runs on `initialize` to hand the agent its
//! starting state.
//!
//! See `SPEC.md` § "The four hooks" and `docs/design/protocol.md`
//! § 3.1.

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;

use crate::auth::Claims;

/// Errors a `BootProvider` can return. Mapped to JSON-RPC application
/// error codes by the dispatch layer.
#[derive(Debug, Error)]
pub enum BootError {
    /// The agent_id in `claims.sub` does not correspond to anything
    /// the operator's bootstrap layer knows about.
    #[error("no boot state for agent {0}")]
    NotFound(String),
    /// The operator's bootstrap layer hit an internal error.
    #[error("boot provider failed: {0}")]
    Internal(String),
}

/// Operator hook for the `initialize` request.
///
/// The broker calls this once per cold-start of an agent. The
/// returned `Value` is shipped verbatim to the agent as the result
/// of the `initialize` JSON-RPC response, so its shape is whatever
/// the agent harness expects (typically `{ system_prompt, messages,
/// tools_config }` per protocol.md § 3.1).
#[async_trait]
pub trait BootProvider: Send + Sync + 'static {
    async fn initialize(&self, claims: &Claims) -> Result<Value, BootError>;
}

/// Default `BootProvider` for the reference server. Returns an empty
/// system prompt, an empty message history, and an empty tools_config.
/// This lets `cargo run` boot end-to-end without an operator stack.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultBootProvider;

#[async_trait]
impl BootProvider for DefaultBootProvider {
    async fn initialize(&self, _claims: &Claims) -> Result<Value, BootError> {
        Ok(json!({
            "system_prompt": "",
            "messages": [],
            "tools_config": {},
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn default_returns_empty_bootstrap() {
        let provider = DefaultBootProvider;
        let claims = Claims::agent(Uuid::new_v4(), None, 60, "test");
        let result = provider.initialize(&claims).await.expect("ok");
        assert_eq!(result["system_prompt"], "");
        assert_eq!(result["messages"], json!([]));
        assert_eq!(result["tools_config"], json!({}));
    }

    #[tokio::test]
    async fn default_ignores_user_field() {
        let provider = DefaultBootProvider;
        let with_user = Claims::agent(Uuid::new_v4(), Some(Uuid::new_v4()), 60, "test");
        let without_user = Claims::agent(Uuid::new_v4(), None, 60, "test");
        assert_eq!(
            provider.initialize(&with_user).await.unwrap(),
            provider.initialize(&without_user).await.unwrap(),
        );
    }
}
