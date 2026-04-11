//! Environment-variable configuration for `overacp-agent`.
//!
//! All configuration comes from environment variables so the agent
//! binary can be deployed into any compute backend (Docker, Morph VM,
//! bare metal) without per-environment config files.

use anyhow::{Context, Result};
use std::env;

/// Runtime configuration for the agent supervisor.
pub struct Config {
    /// JWT minted by the over/ACP broker. Used as the bearer token
    /// on the WebSocket tunnel and decoded (without verification) to
    /// find the agent_id (JWT `sub` claim).
    pub token: String,
    /// Base URL of the over/ACP server, e.g. `https://acp.example.com`
    /// or `http://localhost:8080`. The agent rewrites the scheme to
    /// `ws` / `wss` for the tunnel.
    pub server_url: String,
    /// Working directory the child agent process should treat as its
    /// workspace. Defaults to `/workspace`.
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

    /// Build the WebSocket tunnel URL for a given agent_id (which is
    /// the JWT `sub` claim for agent tokens — the broker's routing
    /// key on `/tunnel/<agent_id>`).
    pub fn tunnel_url(&self, agent_id: &str) -> String {
        let base = self
            .server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        format!("{base}/tunnel/{agent_id}")
    }
}

fn required(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("missing env var: {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(server_url: &str) -> Config {
        Config {
            token: "tok".into(),
            server_url: server_url.into(),
            workspace: "/workspace".into(),
            agent_binary: "overloop".into(),
        }
    }

    #[test]
    fn tunnel_url_rewrites_https_to_wss() {
        let c = dummy("https://acp.example.com");
        assert_eq!(
            c.tunnel_url("abc-123"),
            "wss://acp.example.com/tunnel/abc-123"
        );
    }

    #[test]
    fn tunnel_url_rewrites_http_to_ws() {
        let c = dummy("http://localhost:8080");
        assert_eq!(
            c.tunnel_url("deadbeef-0001"),
            "ws://localhost:8080/tunnel/deadbeef-0001"
        );
    }

    #[test]
    fn tunnel_url_preserves_port_and_path_prefix() {
        let c = dummy("http://127.0.0.1:8080");
        let url = c.tunnel_url("00000000-0000-0000-0000-000000000001");
        assert!(url.starts_with("ws://127.0.0.1:8080/tunnel/"));
        assert!(url.ends_with("00000000-0000-0000-0000-000000000001"));
    }

    #[test]
    fn tunnel_url_passes_through_already_ws_scheme() {
        // If the operator already gave us a ws:// URL we should not
        // double-rewrite.
        let c = dummy("ws://broker:9000");
        assert_eq!(c.tunnel_url("x"), "ws://broker:9000/tunnel/x");
    }
}
