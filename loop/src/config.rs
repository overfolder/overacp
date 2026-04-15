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
    /// Process-local identity. Attached as a tracing span field and,
    /// when the `sentry` feature is enabled, as a Sentry tag.
    pub agent_name: Option<String>,
    pub sentry_dsn: Option<String>,
    pub sentry_environment: String,
    pub sentry_traces_sample_rate: f32,
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

        let agent_name = env::var("OVERLOOP_AGENT_NAME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let sentry_dsn = env::var("SENTRY_DSN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let sentry_environment = env::var("SENTRY_ENVIRONMENT").unwrap_or_else(|_| "local".into());

        let sentry_traces_sample_rate: f32 = env::var("SENTRY_TRACES_SAMPLE_RATE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.1);

        Ok(Self {
            llm_api_key,
            llm_api_url,
            model,
            workspace,
            mcp_servers,
            max_iterations,
            timeout_minutes,
            agent_name,
            sentry_dsn,
            sentry_environment,
            sentry_traces_sample_rate,
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
            "OVERLOOP_AGENT_NAME",
            "SENTRY_DSN",
            "SENTRY_ENVIRONMENT",
            "SENTRY_TRACES_SAMPLE_RATE",
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
        assert!(cfg.agent_name.is_none());
        assert!(cfg.sentry_dsn.is_none());
        assert_eq!(cfg.sentry_environment, "local");
        assert!((cfg.sentry_traces_sample_rate - 0.1).abs() < f32::EPSILON);
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
    fn test_sentry_and_agent_name_custom() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        env::set_var("OVERLOOP_AGENT_NAME", "worker-42");
        env::set_var("SENTRY_DSN", "https://key@example.ingest.sentry.io/1");
        env::set_var("SENTRY_ENVIRONMENT", "prod");
        env::set_var("SENTRY_TRACES_SAMPLE_RATE", "0.25");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.agent_name.as_deref(), Some("worker-42"));
        assert_eq!(
            cfg.sentry_dsn.as_deref(),
            Some("https://key@example.ingest.sentry.io/1")
        );
        assert_eq!(cfg.sentry_environment, "prod");
        assert!((cfg.sentry_traces_sample_rate - 0.25).abs() < f32::EPSILON);
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_empty_agent_name_and_dsn_treated_as_unset() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        env::set_var("OVERLOOP_AGENT_NAME", "   ");
        env::set_var("SENTRY_DSN", "");
        let cfg = Config::from_env().unwrap();
        assert!(cfg.agent_name.is_none());
        assert!(cfg.sentry_dsn.is_none());
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_invalid_traces_sample_rate_falls_back_to_default() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        env::set_var("SENTRY_TRACES_SAMPLE_RATE", "not-a-number");
        let cfg = Config::from_env().unwrap();
        assert!((cfg.sentry_traces_sample_rate - 0.1).abs() < f32::EPSILON);
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
