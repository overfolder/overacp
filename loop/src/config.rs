use anyhow::{Context, Result};
use std::env;

/// Runtime configuration parsed from environment variables.
pub struct Config {
    pub llm_api_key: String,
    pub llm_api_url: String,
    pub model: String,
    pub workspace: String,
    pub mcp_servers: Vec<String>,
    pub max_iterations: usize,
    pub timeout_minutes: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let llm_api_key = env::var("LLM_API_KEY").context("LLM_API_KEY must be set")?;

        let llm_api_url =
            env::var("LLM_API_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".into());

        let model = env::var("OVERFOLDER_MODEL")
            .unwrap_or_else(|_| "anthropic/claude-sonnet-4-20250514".into());

        let workspace = env::var("OVERFOLDER_WORKSPACE").unwrap_or_else(|_| ".".into());

        let mcp_servers: Vec<String> = env::var("MCP_SERVERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let max_iterations: usize = env::var("MAX_ITERATIONS")
            .unwrap_or_else(|_| "100".into())
            .parse()
            .unwrap_or(100);

        let timeout_minutes: u64 = env::var("TIMEOUT_MINUTES")
            .unwrap_or_else(|_| "60".into())
            .parse()
            .unwrap_or(60);

        Ok(Self {
            llm_api_key,
            llm_api_url,
            model,
            workspace,
            mcp_servers,
            max_iterations,
            timeout_minutes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    fn cleanup_env() {
        for key in [
            "LLM_API_KEY",
            "LLM_API_URL",
            "OVERFOLDER_MODEL",
            "OVERFOLDER_WORKSPACE",
            "MCP_SERVERS",
            "MAX_ITERATIONS",
            "TIMEOUT_MINUTES",
        ] {
            env::remove_var(key);
        }
    }

    #[test]
    #[serial]
    fn test_from_env_missing_key() {
        cleanup_env();
        let result = Config::from_env();
        assert!(result.is_err());
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_from_env_defaults() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.llm_api_key, "test");
        assert_eq!(cfg.llm_api_url, "https://openrouter.ai/api/v1");
        assert_eq!(cfg.model, "anthropic/claude-sonnet-4-20250514");
        assert_eq!(cfg.workspace, ".");
        assert!(cfg.mcp_servers.is_empty());
        assert_eq!(cfg.max_iterations, 100);
        assert_eq!(cfg.timeout_minutes, 60);
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_from_env_custom() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "key123");
        env::set_var("LLM_API_URL", "http://localhost:8080");
        env::set_var("OVERFOLDER_MODEL", "gpt-4");
        env::set_var("OVERFOLDER_WORKSPACE", "/tmp/ws");
        env::set_var("MCP_SERVERS", "http://a");
        env::set_var("MAX_ITERATIONS", "50");
        env::set_var("TIMEOUT_MINUTES", "30");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.llm_api_key, "key123");
        assert_eq!(cfg.llm_api_url, "http://localhost:8080");
        assert_eq!(cfg.model, "gpt-4");
        assert_eq!(cfg.workspace, "/tmp/ws");
        assert_eq!(cfg.mcp_servers, vec!["http://a"]);
        assert_eq!(cfg.max_iterations, 50);
        assert_eq!(cfg.timeout_minutes, 30);
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_mcp_servers_parsing() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        env::set_var("MCP_SERVERS", "http://a, http://b , ");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.mcp_servers, vec!["http://a", "http://b"]);
        cleanup_env();
    }
}
