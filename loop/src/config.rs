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
    /// Total context window in tokens. Env: `CONTEXT_WINDOW`.
    pub context_window: usize,
    /// Fraction of the context window at which auto-compaction kicks
    /// in. Env: `COMPACTION_THRESHOLD`.
    pub compaction_threshold: f64,
    /// Number of most-recent messages kept during compaction. Env:
    /// `COMPACTION_KEEP_RECENT`.
    pub compaction_keep_recent: usize,
    /// Max compaction rounds per session. Env: `MAX_COMPACTIONS`.
    pub max_compactions: usize,
    /// Process-local identity. Attached as a tracing span field and,
    /// when the `sentry` feature is enabled, as a Sentry tag.
    pub agent_name: Option<String>,
    pub sentry_dsn: Option<String>,
    pub sentry_environment: String,
    pub sentry_traces_sample_rate: f32,
    /// Langfuse public key. When absent, Langfuse tracing is a no-op.
    pub langfuse_public_key: Option<String>,
    /// Langfuse secret key. When absent, Langfuse tracing is a no-op.
    pub langfuse_secret_key: Option<String>,
    /// Langfuse ingestion host. Defaults to `https://cloud.langfuse.com`.
    pub langfuse_host: String,
    /// Tag attached to every Langfuse observation. Defaults to `local`.
    pub langfuse_environment: String,
    /// Opt-in: attach a redacted chat-log snapshot to the Langfuse
    /// generation `input` field. Off by default so prompt content
    /// stays local unless the operator explicitly opts in.
    pub langfuse_capture_input: bool,
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

        let context_window: usize = env::var("CONTEXT_WINDOW")
            .unwrap_or_else(|_| "128000".into())
            .parse()
            .unwrap_or(128_000);

        let compaction_threshold: f64 = env::var("COMPACTION_THRESHOLD")
            .unwrap_or_else(|_| "0.80".into())
            .parse()
            .unwrap_or(0.80);

        let compaction_keep_recent: usize = env::var("COMPACTION_KEEP_RECENT")
            .unwrap_or_else(|_| "10".into())
            .parse()
            .unwrap_or(10);

        let max_compactions: usize = env::var("MAX_COMPACTIONS")
            .unwrap_or_else(|_| "3".into())
            .parse()
            .unwrap_or(3);

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

        let langfuse_public_key = env::var("LANGFUSE_PUBLIC_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let langfuse_secret_key = env::var("LANGFUSE_SECRET_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let langfuse_host =
            env::var("LANGFUSE_HOST").unwrap_or_else(|_| "https://cloud.langfuse.com".into());

        let langfuse_environment =
            env::var("LANGFUSE_ENVIRONMENT").unwrap_or_else(|_| "local".into());

        let langfuse_capture_input = env::var("LANGFUSE_CAPTURE_INPUT")
            .ok()
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        Ok(Self {
            llm_api_key,
            llm_api_url,
            model,
            workspace,
            mcp_servers,
            max_iterations,
            timeout_minutes,
            context_window,
            compaction_threshold,
            compaction_keep_recent,
            max_compactions,
            agent_name,
            sentry_dsn,
            sentry_environment,
            sentry_traces_sample_rate,
            langfuse_public_key,
            langfuse_secret_key,
            langfuse_host,
            langfuse_environment,
            langfuse_capture_input,
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
            "CONTEXT_WINDOW",
            "COMPACTION_THRESHOLD",
            "COMPACTION_KEEP_RECENT",
            "MAX_COMPACTIONS",
            "OVERLOOP_AGENT_NAME",
            "SENTRY_DSN",
            "SENTRY_ENVIRONMENT",
            "SENTRY_TRACES_SAMPLE_RATE",
            "LANGFUSE_PUBLIC_KEY",
            "LANGFUSE_SECRET_KEY",
            "LANGFUSE_HOST",
            "LANGFUSE_ENVIRONMENT",
            "LANGFUSE_CAPTURE_INPUT",
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
        assert_eq!(cfg.context_window, 128_000);
        assert!((cfg.compaction_threshold - 0.80).abs() < f64::EPSILON);
        assert_eq!(cfg.compaction_keep_recent, 10);
        assert_eq!(cfg.max_compactions, 3);
        assert!(cfg.agent_name.is_none());
        assert!(cfg.sentry_dsn.is_none());
        assert_eq!(cfg.sentry_environment, "local");
        assert!((cfg.sentry_traces_sample_rate - 0.1).abs() < f32::EPSILON);
        assert!(cfg.langfuse_public_key.is_none());
        assert!(cfg.langfuse_secret_key.is_none());
        assert_eq!(cfg.langfuse_host, "https://cloud.langfuse.com");
        assert_eq!(cfg.langfuse_environment, "local");
        assert!(!cfg.langfuse_capture_input);
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_langfuse_custom() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        env::set_var("LANGFUSE_PUBLIC_KEY", "pk-xyz");
        env::set_var("LANGFUSE_SECRET_KEY", "sk-xyz");
        env::set_var("LANGFUSE_HOST", "https://self.hosted.langfuse");
        env::set_var("LANGFUSE_ENVIRONMENT", "prod");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.langfuse_public_key.as_deref(), Some("pk-xyz"));
        assert_eq!(cfg.langfuse_secret_key.as_deref(), Some("sk-xyz"));
        assert_eq!(cfg.langfuse_host, "https://self.hosted.langfuse");
        assert_eq!(cfg.langfuse_environment, "prod");
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_langfuse_capture_input_toggle() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        for (val, expected) in [
            ("1", true),
            ("true", true),
            ("TRUE", true),
            ("yes", true),
            ("0", false),
            ("false", false),
            ("bogus", false),
            ("", false),
        ] {
            env::set_var("LANGFUSE_CAPTURE_INPUT", val);
            let cfg = Config::from_env().unwrap();
            assert_eq!(
                cfg.langfuse_capture_input, expected,
                "LANGFUSE_CAPTURE_INPUT={val:?}"
            );
        }
        cleanup_env();
    }

    #[test]
    #[serial]
    fn test_langfuse_empty_keys_treated_as_unset() {
        cleanup_env();
        env::set_var("LLM_API_KEY", "test");
        env::set_var("LANGFUSE_PUBLIC_KEY", "   ");
        env::set_var("LANGFUSE_SECRET_KEY", "");
        let cfg = Config::from_env().unwrap();
        assert!(cfg.langfuse_public_key.is_none());
        assert!(cfg.langfuse_secret_key.is_none());
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
