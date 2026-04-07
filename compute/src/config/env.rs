use std::env;

use async_trait::async_trait;

use crate::config::ConfigProvider;
use crate::error::ConfigError;

/// Resolves `${env:VAR_NAME}` references against the process environment.
#[derive(Debug, Default, Clone)]
pub struct EnvConfigProvider;

#[async_trait]
impl ConfigProvider for EnvConfigProvider {
    fn scheme(&self) -> &'static str {
        "env"
    }

    async fn lookup(&self, path: &str, key: Option<&str>) -> Result<String, ConfigError> {
        if key.is_some() {
            return Err(ConfigError::InvalidValue {
                key: path.to_owned(),
                msg: "env references take a single segment: ${env:VAR}".into(),
            });
        }
        env::var(path).map_err(|_| ConfigError::MissingKey(format!("env:{path}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn looks_up_env_var() {
        // Use a unique name to avoid races with other tests.
        let name = "OVERACP_COMPUTE_CORE_ENV_TEST_VAR";
        // SAFETY: tests in this crate use unique names per test.
        unsafe { env::set_var(name, "hello") };
        let p = EnvConfigProvider;
        assert_eq!(p.lookup(name, None).await.unwrap(), "hello");
        unsafe { env::remove_var(name) };
        assert!(matches!(
            p.lookup(name, None).await,
            Err(ConfigError::MissingKey(_))
        ));
    }

    #[tokio::test]
    async fn rejects_key_segment() {
        let p = EnvConfigProvider;
        assert!(matches!(
            p.lookup("FOO", Some("bar")).await,
            Err(ConfigError::InvalidValue { .. })
        ));
    }
}
