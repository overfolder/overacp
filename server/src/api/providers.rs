//! Server-side compute-provider catalogue.
//!
//! `compute::ComputeProvider` (the runtime trait in `compute/`)
//! can't be a trait object — its `validate_config` and
//! `from_config` methods are `where Self: Sized`. So the server
//! holds its own dyn-friendly shim trait, [`ProviderPlugin`], that
//! pairs the catalogue metadata with a validation hook taking a
//! parsed [`PoolConfig`]. Each plugin can delegate internally to
//! `compute::ComputeProvider::validate_config` once the real
//! provider crates land.
//!
//! For now `local-process` and `morph` ship as inline stubs that
//! match the fixtures in `docs/design/controlplane.md` § 3.2.1.

use std::collections::BTreeMap;

use crate::api::dto::{ProviderInfo, ValidationFieldError};
use crate::api::pool_config::{is_secret_ref, PoolConfig};

/// A compiled-in compute provider type, as seen by the REST
/// surface. Wraps a `compute::ComputeProvider`'s static metadata
/// and validation hook in a dyn-compatible shape.
pub trait ProviderPlugin: Send + Sync {
    fn provider_type(&self) -> &'static str;
    fn info(&self) -> ProviderInfo;

    /// Validate a parsed [`PoolConfig`]. Pure: no I/O, no secret
    /// resolution. Returns the empty vec for "valid".
    fn validate(&self, config: &PoolConfig) -> Vec<ValidationFieldError>;
}

/// In-memory registry of provider plugins, populated at startup.
#[derive(Default)]
pub struct ProviderRegistry {
    by_type: BTreeMap<&'static str, Box<dyn ProviderPlugin>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: Box<dyn ProviderPlugin>) {
        self.by_type.insert(plugin.provider_type(), plugin);
    }

    pub fn get(&self, ty: &str) -> Option<&dyn ProviderPlugin> {
        self.by_type.get(ty).map(|b| b.as_ref())
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        self.by_type.values().map(|p| p.info()).collect()
    }
}

/// Default registry preloaded with the providers compiled into
/// this binary. Production deployments should compose their own
/// registry from external provider crates.
pub fn default_registry() -> ProviderRegistry {
    let mut r = ProviderRegistry::new();
    r.register(Box::new(LocalProcessPlugin));
    r.register(Box::new(MorphPlugin));
    r
}

// ── built-in plugins ─────────────────────────────────────────────

/// Stub plugin for `overacp-compute-local`. The real
/// `ComputeProvider` impl lives in its own crate; this catalogue
/// entry only needs the metadata + the validate hook.
pub struct LocalProcessPlugin;

impl ProviderPlugin for LocalProcessPlugin {
    fn provider_type(&self) -> &'static str {
        "local-process"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider_type: "local-process".into(),
            display_name: "Local process".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    fn validate(&self, config: &PoolConfig) -> Vec<ValidationFieldError> {
        let mut errors = Vec::new();
        if config.provider_class() != "local-process" {
            errors.push(ValidationFieldError {
                key: "provider.class".into(),
                messages: vec![format!(
                    "must equal 'local-process', got '{}'",
                    config.provider_class()
                )],
            });
        }
        errors
    }
}

/// Stub plugin for `overacp-compute-morph`. Validates the keys
/// the design doc's § 3.2.1 fixture uses. Numeric keys accept
/// either a parseable integer or a secret-ref placeholder.
pub struct MorphPlugin;

impl ProviderPlugin for MorphPlugin {
    fn provider_type(&self) -> &'static str {
        "morph"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider_type: "morph".into(),
            display_name: "Morph Cloud".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    fn validate(&self, config: &PoolConfig) -> Vec<ValidationFieldError> {
        let mut errors = Vec::new();
        if config.provider_class() != "morph" {
            errors.push(ValidationFieldError {
                key: "provider.class".into(),
                messages: vec![format!(
                    "must equal 'morph', got '{}'",
                    config.provider_class()
                )],
            });
        }
        require_present(config, "morph.api_key", &mut errors);
        require_present(config, "default.image", &mut errors);
        for key in [
            "default.cpu",
            "default.memory_gb",
            "default.disk_gb",
            "max_nodes",
            "idle_ttl_s",
        ] {
            if let Some(v) = config.get(key) {
                if !is_secret_ref(v) && v.parse::<u64>().is_err() {
                    errors.push(ValidationFieldError {
                        key: key.to_string(),
                        messages: vec![format!(
                            "expected an unsigned integer, got '{v}'"
                        )],
                    });
                }
            }
        }
        errors
    }
}

fn require_present(
    config: &PoolConfig,
    key: &str,
    errors: &mut Vec<ValidationFieldError>,
) {
    match config.get(key) {
        None => errors.push(ValidationFieldError {
            key: key.to_string(),
            messages: vec![format!("required key '{key}' is missing")],
        }),
        Some("") => errors.push(ValidationFieldError {
            key: key.to_string(),
            messages: vec![format!("required key '{key}' must not be empty")],
        }),
        Some(_) => {}
    }
}
