//! Environment-variable configuration for `overacp-agent`.
//!
//! All configuration comes from environment variables so the agent
//! binary can be deployed into any compute backend (Docker, Morph VM,
//! bare metal) without per-environment config files.

use anyhow::{Context, Result};
use std::env;

/// Runtime configuration for the agent supervisor.
pub struct Config {
    /// JWT minted by the over/ACP server. Used as the bearer token on
    /// the WebSocket tunnel and decoded (without verification) to find
    /// the conversation ID.
    pub token: String,
    /// Base URL of the over/ACP server, e.g. `https://acp.example.com`
    /// or `http://localhost:8080`. The agent rewrites the scheme to
    /// `ws` / `wss` for the tunnel.
    pub server_url: String,
    /// Working directory the child agent process should treat as the
    /// session workspace. Defaults to `/workspace`.
    pub workspace: String,
    /// Path or basename of the child agent binary. Resolved by the
    /// `AgentAdapter` impl. Defaults to `overloop`.
    pub agent_binary: String,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Required:
    /// - `OVERACP_TOKEN` — the session JWT
    /// - `OVERACP_SERVER_URL` — base URL of the over/ACP server
    ///
    /// Optional:
    /// - `OVERACP_WORKSPACE` (default `/workspace`)
    /// - `OVERACP_AGENT_BINARY` (default `overloop`)
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            token: required("OVERACP_TOKEN")?,
            server_url: required("OVERACP_SERVER_URL")?,
            workspace: env::var("OVERACP_WORKSPACE").unwrap_or_else(|_| "/workspace".into()),
            agent_binary: env::var("OVERACP_AGENT_BINARY").unwrap_or_else(|_| "overloop".into()),
        })
    }

    /// Build the WebSocket tunnel URL for a given session ID.
    pub fn tunnel_url(&self, session_id: &str) -> String {
        let base = self
            .server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        format!("{base}/tunnel/{session_id}")
    }
}

fn required(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("missing env var: {key}"))
}
