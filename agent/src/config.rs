//! Boot configuration for the `overacp-agent` supervisor.
//!
//! See `docs/design/protocol.md` § 2.4 — the supervisor is configured
//! exclusively through environment variables. There is no config file,
//! no CLI flags beyond `--help`/`--version`, and no positional args.

use std::collections::BTreeMap;
use std::env::{self, VarError};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

/// Required: full WebSocket URL including the agent UUID path,
/// e.g. `wss://server/tunnel/<agent_id>`.
pub const ENV_TUNNEL_URL: &str = "OVERACP_TUNNEL_URL";
/// Required: bearer JWT for the tunnel and LLM proxy.
pub const ENV_JWT: &str = "OVERACP_JWT";
/// Required: the agent's unique identifier (matches the JWT `sub`
/// claim and the `<agent_id>` segment of the tunnel URL).
pub const ENV_AGENT_ID: &str = "OVERACP_AGENT_ID";
/// Optional: which `AgentAdapter` to load. Defaults to `loop`.
pub const ENV_ADAPTER: &str = "OVERACP_ADAPTER";
/// Optional: working directory for the child agent process.
pub const ENV_WORKSPACE_DIR: &str = "OVERACP_WORKSPACE_DIR";
/// Optional: test override for the reconnect backoff base, in
/// milliseconds.
pub const ENV_RECONNECT_BACKOFF_MS: &str = "OVERACP_RECONNECT_BACKOFF_MS";

/// Default adapter name when `OVERACP_ADAPTER` is unset.
pub const DEFAULT_ADAPTER: &str = "loop";

/// Reserved namespace prefix; vars starting with this are owned by
/// the supervisor and not forwarded to the child adapter.
const OVERACP_PREFIX: &str = "OVERACP_";

/// Errors produced while parsing the supervisor boot environment.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A required environment variable was missing or empty.
    #[error("required environment variable `{0}` is not set")]
    MissingRequired(&'static str),
    /// An environment variable was set but contained invalid UTF-8.
    #[error("environment variable `{name}` is not valid UTF-8")]
    NotUnicode {
        /// Name of the offending variable.
        name: &'static str,
    },
    /// `OVERACP_AGENT_ID` was not a valid UUID.
    #[error("`{}` must be a UUID: {source}", ENV_AGENT_ID)]
    InvalidAgentId {
        /// Underlying parse error.
        #[source]
        source: uuid::Error,
    },
    /// Failed to read the launch CWD when `OVERACP_WORKSPACE_DIR` was unset.
    #[error("failed to read launch working directory: {source}")]
    Cwd {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// `OVERACP_RECONNECT_BACKOFF_MS` was not a positive integer.
    #[error(
        "`{}` must be a positive integer in milliseconds",
        ENV_RECONNECT_BACKOFF_MS
    )]
    InvalidBackoff,
}

/// Parsed supervisor boot configuration.
#[derive(Debug, Clone)]
pub struct BootConfig {
    /// Full WebSocket URL including the agent UUID path.
    pub tunnel_url: String,
    /// Bearer JWT for the tunnel upgrade and LLM proxy.
    pub jwt: String,
    /// Unique identifier for this agent. Matches the JWT `sub` claim
    /// and the `<agent_id>` segment of the tunnel URL.
    pub agent_id: Uuid,
    /// Adapter name to load (defaults to [`DEFAULT_ADAPTER`]).
    pub adapter: String,
    /// Working directory for the child adapter process. Defaults to
    /// the supervisor's launch CWD — there is no hardcoded
    /// `/workspace`.
    pub workspace_dir: PathBuf,
    /// Optional test override for the reconnect backoff base.
    pub reconnect_backoff_ms: Option<u64>,
    /// Environment variables to forward verbatim to the child
    /// adapter. Excludes `OVERACP_*` (those are the supervisor's).
    pub child_env: BTreeMap<String, String>,
}

/// Source of environment variables. Abstracted so tests don't have
/// to mutate `std::env`, which is process-global and racy.
pub trait EnvSource {
    /// Look up a variable, mirroring `std::env::var`'s semantics.
    fn var(&self, key: &str) -> Result<String, VarError>;
    /// Iterate every `(key, value)` pair, mirroring
    /// `std::env::vars_os`.
    fn vars(&self) -> Vec<(OsString, OsString)>;
    /// Current working directory at supervisor launch.
    fn current_dir(&self) -> io::Result<PathBuf>;
}

/// `EnvSource` backed by the real process environment.
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Result<String, VarError> {
        env::var(key)
    }
    fn vars(&self) -> Vec<(OsString, OsString)> {
        env::vars_os().collect()
    }
    fn current_dir(&self) -> io::Result<PathBuf> {
        env::current_dir()
    }
}

impl BootConfig {
    /// Parse the boot environment from the real process.
    pub fn from_process_env() -> Result<Self, ConfigError> {
        Self::from_env(&ProcessEnv)
    }

    /// Parse the boot environment from an arbitrary [`EnvSource`].
    /// Used by tests so they don't fight `std::env`.
    pub fn from_env<E: EnvSource>(env: &E) -> Result<Self, ConfigError> {
        let tunnel_url = required(env, ENV_TUNNEL_URL)?;
        let jwt = required(env, ENV_JWT)?;
        let agent_id_raw = required(env, ENV_AGENT_ID)?;
        let agent_id = Uuid::parse_str(&agent_id_raw)
            .map_err(|source| ConfigError::InvalidAgentId { source })?;

        let adapter = optional(env, ENV_ADAPTER)?
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_ADAPTER.to_owned());

        let workspace_dir = match optional(env, ENV_WORKSPACE_DIR)? {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => env
                .current_dir()
                .map_err(|source| ConfigError::Cwd { source })?,
        };

        let reconnect_backoff_ms = match optional(env, ENV_RECONNECT_BACKOFF_MS)? {
            Some(v) if !v.is_empty() => {
                let n: u64 = v.parse().map_err(|_| ConfigError::InvalidBackoff)?;
                if n == 0 {
                    return Err(ConfigError::InvalidBackoff);
                }
                Some(n)
            }
            _ => None,
        };

        let mut child_env = BTreeMap::new();
        for (k, v) in env.vars() {
            let (Some(ks), Some(vs)) = (k.to_str(), v.to_str()) else {
                continue;
            };
            if ks.starts_with(OVERACP_PREFIX) {
                continue;
            }
            child_env.insert(ks.to_owned(), vs.to_owned());
        }

        Ok(Self {
            tunnel_url,
            jwt,
            agent_id,
            adapter,
            workspace_dir,
            reconnect_backoff_ms,
            child_env,
        })
    }
}

fn required<E: EnvSource>(env: &E, name: &'static str) -> Result<String, ConfigError> {
    match env.var(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        Ok(_) | Err(VarError::NotPresent) => Err(ConfigError::MissingRequired(name)),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::NotUnicode { name }),
    }
}

fn optional<E: EnvSource>(env: &E, name: &'static str) -> Result<Option<String>, ConfigError> {
    match env.var(name) {
        Ok(v) => Ok(Some(v)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::NotUnicode { name }),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BootConfig, ConfigError, EnvSource, DEFAULT_ADAPTER, ENV_ADAPTER, ENV_AGENT_ID, ENV_JWT,
        ENV_RECONNECT_BACKOFF_MS, ENV_TUNNEL_URL, ENV_WORKSPACE_DIR,
    };
    use std::collections::HashMap;
    use std::env::VarError;
    use std::ffi::OsString;
    use std::io;
    use std::path::PathBuf;

    struct FakeEnv {
        vars: HashMap<String, String>,
        cwd: PathBuf,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                vars: HashMap::new(),
                cwd: PathBuf::from("/tmp/fake-cwd"),
            }
        }
        fn set(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.to_owned(), v.to_owned());
            self
        }
    }

    impl EnvSource for FakeEnv {
        fn var(&self, key: &str) -> Result<String, VarError> {
            self.vars.get(key).cloned().ok_or(VarError::NotPresent)
        }
        fn vars(&self) -> Vec<(OsString, OsString)> {
            self.vars
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect()
        }
        fn current_dir(&self) -> io::Result<PathBuf> {
            Ok(self.cwd.clone())
        }
    }

    const VALID_UUID: &str = "11111111-2222-3333-4444-555555555555";

    fn minimal() -> FakeEnv {
        FakeEnv::new()
            .set(ENV_TUNNEL_URL, "wss://example/tunnel/abc")
            .set(ENV_JWT, "jwt-token")
            .set(ENV_AGENT_ID, VALID_UUID)
    }

    #[test]
    fn defaults_when_only_required_set() {
        let env = minimal();
        let cfg = BootConfig::from_env(&env).expect("parses");
        assert_eq!(cfg.tunnel_url, "wss://example/tunnel/abc");
        assert_eq!(cfg.jwt, "jwt-token");
        assert_eq!(cfg.agent_id.to_string(), VALID_UUID);
        assert_eq!(cfg.adapter, DEFAULT_ADAPTER);
        assert_eq!(cfg.workspace_dir, PathBuf::from("/tmp/fake-cwd"));
        assert!(cfg.reconnect_backoff_ms.is_none());
    }

    #[test]
    fn missing_tunnel_url_errors() {
        let env = FakeEnv::new()
            .set(ENV_JWT, "x")
            .set(ENV_AGENT_ID, VALID_UUID);
        match BootConfig::from_env(&env) {
            Err(ConfigError::MissingRequired(name)) => assert_eq!(name, ENV_TUNNEL_URL),
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn missing_jwt_errors() {
        let env = FakeEnv::new()
            .set(ENV_TUNNEL_URL, "wss://x")
            .set(ENV_AGENT_ID, VALID_UUID);
        match BootConfig::from_env(&env) {
            Err(ConfigError::MissingRequired(name)) => assert_eq!(name, ENV_JWT),
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn missing_agent_id_errors() {
        let env = FakeEnv::new()
            .set(ENV_TUNNEL_URL, "wss://x")
            .set(ENV_JWT, "x");
        match BootConfig::from_env(&env) {
            Err(ConfigError::MissingRequired(name)) => assert_eq!(name, ENV_AGENT_ID),
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn empty_required_treated_as_missing() {
        let env = minimal().set(ENV_JWT, "");
        match BootConfig::from_env(&env) {
            Err(ConfigError::MissingRequired(name)) => assert_eq!(name, ENV_JWT),
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn invalid_agent_id_errors() {
        let env = minimal().set(ENV_AGENT_ID, "not-a-uuid");
        assert!(matches!(
            BootConfig::from_env(&env),
            Err(ConfigError::InvalidAgentId { .. })
        ));
    }

    #[test]
    fn adapter_override_honored() {
        let env = minimal().set(ENV_ADAPTER, "claude");
        assert_eq!(BootConfig::from_env(&env).unwrap().adapter, "claude");
    }

    #[test]
    fn workspace_dir_override_honored() {
        let env = minimal().set(ENV_WORKSPACE_DIR, "/srv/work");
        let cfg = BootConfig::from_env(&env).unwrap();
        assert_eq!(cfg.workspace_dir, PathBuf::from("/srv/work"));
    }

    #[test]
    fn workspace_dir_defaults_to_launch_cwd_not_workspace() {
        let cfg = BootConfig::from_env(&minimal()).unwrap();
        assert_eq!(cfg.workspace_dir, PathBuf::from("/tmp/fake-cwd"));
        // Regression: make sure no one snuck `/workspace` back in.
        assert_ne!(cfg.workspace_dir, PathBuf::from("/workspace"));
    }

    #[test]
    fn reconnect_backoff_parsed() {
        let env = minimal().set(ENV_RECONNECT_BACKOFF_MS, "250");
        assert_eq!(
            BootConfig::from_env(&env).unwrap().reconnect_backoff_ms,
            Some(250)
        );
    }

    #[test]
    fn reconnect_backoff_invalid_errors() {
        let env = minimal().set(ENV_RECONNECT_BACKOFF_MS, "fast");
        assert!(matches!(
            BootConfig::from_env(&env),
            Err(ConfigError::InvalidBackoff)
        ));
    }

    #[test]
    fn reconnect_backoff_zero_rejected() {
        let env = minimal().set(ENV_RECONNECT_BACKOFF_MS, "0");
        assert!(matches!(
            BootConfig::from_env(&env),
            Err(ConfigError::InvalidBackoff)
        ));
    }

    #[test]
    fn forwards_unknown_env_to_child_excludes_overacp() {
        let env = minimal()
            .set("ANTHROPIC_API_KEY", "sk-test")
            .set("FOO", "bar")
            .set(ENV_ADAPTER, "loop");
        let cfg = BootConfig::from_env(&env).unwrap();
        assert_eq!(
            cfg.child_env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-test")
        );
        assert_eq!(cfg.child_env.get("FOO").map(String::as_str), Some("bar"));
        // OVERACP_* must not leak through.
        assert!(!cfg.child_env.contains_key(ENV_TUNNEL_URL));
        assert!(!cfg.child_env.contains_key(ENV_JWT));
        assert!(!cfg.child_env.contains_key(ENV_AGENT_ID));
        assert!(!cfg.child_env.contains_key(ENV_ADAPTER));
    }
}
