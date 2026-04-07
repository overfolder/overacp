//! Pool config + secret-reference resolution.
//!
//! See `docs/design/controlplane.md` §3.5 + §3.6.

mod env;
mod file;
mod reference;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ConfigError;

pub use env::EnvConfigProvider;
pub use file::FileConfigProvider;

use reference::{parse, Segment};

/// User-supplied pool config. Values may contain `${scheme:path[:key]}`
/// references that the resolver expands.
#[derive(Debug, Clone, Default)]
pub struct RawConfig(pub BTreeMap<String, String>);

impl RawConfig {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.0.insert(k.into(), v.into());
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for RawConfig {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(iter.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

/// Pool config after secret references have been resolved.
///
/// `original` preserves the user's `${...}` strings so the REST layer's
/// `GET /compute/pools/{name}/config` can echo them back unmodified —
/// resolved values must never leak.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    resolved: BTreeMap<String, String>,
    original: BTreeMap<String, String>,
}

impl ResolvedConfig {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.resolved.get(key).map(String::as_str)
    }
    pub fn require(&self, key: &str) -> Result<&str, ConfigError> {
        self.get(key)
            .ok_or_else(|| ConfigError::MissingKey(key.to_owned()))
    }
    pub fn provider_class(&self) -> Result<&str, ConfigError> {
        self.require("provider.class")
    }
    pub fn original(&self) -> &BTreeMap<String, String> {
        &self.original
    }
    pub fn resolved(&self) -> &BTreeMap<String, String> {
        &self.resolved
    }
}

/// Pluggable secret backend. `env` and `file` ship in this crate;
/// deployments can register their own (Vault, AWS Secrets Manager, ...).
#[async_trait]
pub trait ConfigProvider: Send + Sync {
    fn scheme(&self) -> &'static str;
    async fn lookup(&self, path: &str, key: Option<&str>) -> Result<String, ConfigError>;
}

#[derive(Default)]
pub struct ConfigResolver {
    providers: HashMap<String, Arc<dyn ConfigProvider>>,
}

impl ConfigResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a resolver pre-loaded with `env` and `file` providers.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(EnvConfigProvider));
        r.register(Arc::new(FileConfigProvider::new()));
        r
    }

    pub fn register(&mut self, p: Arc<dyn ConfigProvider>) {
        self.providers.insert(p.scheme().to_owned(), p);
    }

    pub async fn resolve(&self, raw: RawConfig) -> Result<ResolvedConfig, ConfigError> {
        let mut resolved = BTreeMap::new();
        for (k, v) in &raw.0 {
            resolved.insert(k.clone(), self.resolve_value(v).await?);
        }
        Ok(ResolvedConfig {
            resolved,
            original: raw.0,
        })
    }

    async fn resolve_value(&self, value: &str) -> Result<String, ConfigError> {
        let segments = parse(value)?;
        let mut out = String::new();
        for seg in segments {
            match seg {
                Segment::Literal(s) => out.push_str(&s),
                Segment::Ref { scheme, path, key } => {
                    let provider = self
                        .providers
                        .get(&scheme)
                        .ok_or_else(|| ConfigError::UnknownProvider(scheme.clone()))?;
                    let v = provider.lookup(&path, key.as_deref()).await?;
                    out.push_str(&v);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn end_to_end_morph_pool_config_round_trips() {
        use std::env;
        let var = "OVERACP_TEST_MORPH_API_KEY";
        unsafe { env::set_var(var, "sk-test-123") };

        let raw: RawConfig = [
            ("provider.class", "morph"),
            ("morph.api_key", &format!("${{env:{var}}}")),
            ("morph.base_url", "https://api.morph.so"),
            ("default.image", "ghcr.io/overfolder/overacp-agent:latest"),
        ]
        .into_iter()
        .collect();

        let resolver = ConfigResolver::with_defaults();
        let resolved = resolver.resolve(raw).await.unwrap();

        assert_eq!(resolved.provider_class().unwrap(), "morph");
        assert_eq!(resolved.require("morph.api_key").unwrap(), "sk-test-123");
        // Round-trip safety: original keeps the literal reference.
        assert_eq!(
            resolved.original().get("morph.api_key").unwrap(),
            &format!("${{env:{var}}}")
        );

        unsafe { env::remove_var(var) };
    }

    #[tokio::test]
    async fn unknown_scheme_errors() {
        let raw: RawConfig = [("k", "${vault:foo:bar}")].into_iter().collect();
        let resolver = ConfigResolver::with_defaults();
        assert!(matches!(
            resolver.resolve(raw).await,
            Err(ConfigError::UnknownProvider(s)) if s == "vault"
        ));
    }

    /// Compile-time check: `ComputeProvider` is object-safe.
    #[allow(dead_code)]
    fn _object_safety() {
        use crate::provider::ComputeProvider;
        fn _take(_: Box<dyn ComputeProvider>) {}
    }
}
