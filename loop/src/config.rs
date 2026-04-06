use anyhow::{Context, Result};

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
        let llm_api_key = std::env::var("LLM_API_KEY").context("LLM_API_KEY must be set")?;

        let llm_api_url =
            std::env::var("LLM_API_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".into());

        let model = std::env::var("OVERFOLDER_MODEL")
            .unwrap_or_else(|_| "anthropic/claude-sonnet-4-20250514".into());

        let workspace = std::env::var("OVERFOLDER_WORKSPACE").unwrap_or_else(|_| ".".into());

        let mcp_servers: Vec<String> = std::env::var("MCP_SERVERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let max_iterations: usize = std::env::var("MAX_ITERATIONS")
            .unwrap_or_else(|_| "100".into())
            .parse()
            .unwrap_or(100);

        let timeout_minutes: u64 = std::env::var("TIMEOUT_MINUTES")
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
            std::env::remove_var(key);
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
        std::env::set_var("LLM_API_KEY", "test");
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
        std::env::set_var("LLM_API_KEY", "key123");
        std::env::set_var("LLM_API_URL", "http://localhost:8080");
        std::env::set_var("OVERFOLDER_MODEL", "gpt-4");
        std::env::set_var("OVERFOLDER_WORKSPACE", "/tmp/ws");
        std::env::set_var("MCP_SERVERS", "http://a");
        std::env::set_var("MAX_ITERATIONS", "50");
        std::env::set_var("TIMEOUT_MINUTES", "30");
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
        std::env::set_var("LLM_API_KEY", "test");
        std::env::set_var("MCP_SERVERS", "http://a, http://b , ");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.mcp_servers, vec!["http://a", "http://b"]);
        cleanup_env();
    }
}
