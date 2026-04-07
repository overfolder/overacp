//! Typed pool config — *parse, don't validate*.
//!
//! The wire shape is a Kafka-Connect-style flat object whose
//! values are all strings (see `docs/design/controlplane.md`
//! § 3.2.1). Rather than passing a raw `serde_json::Value` around
//! the handler layer and re-validating its shape at every step,
//! we parse it once at the boundary into [`PoolConfig`] — a type
//! that *encodes* the invariants:
//!
//! 1. The body is a JSON object.
//! 2. Every value is a string (no nested objects, no numbers).
//! 3. The required `provider.class` key is present and non-empty.
//!
//! Anything that holds a `PoolConfig` can rely on those facts
//! without rechecking. Secret references (`${...}`) are kept as
//! opaque strings — see § 3.5 — and round-trip verbatim through
//! the JSON conversion below, which is what makes the GET pool
//! response GitOps-safe.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use thiserror::Error;

/// Well-known key whose value selects the provider impl. Mirrors
/// Kafka Connect's `connector.class`.
pub const PROVIDER_CLASS_KEY: &str = "provider.class";

/// A parsed, well-formed pool config.
///
/// Internally a `BTreeMap` so JSON serialisation has a stable
/// key order — required for fixture round-trip tests and for
/// GitOps diffs that compare config snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolConfig {
    entries: BTreeMap<String, String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PoolConfigParseError {
    #[error("config must be a JSON object")]
    NotAnObject,
    #[error(
        "value for key '{key}' must be a string (Kafka-Connect-style flat map); got {kind}"
    )]
    NonStringValue { key: String, kind: &'static str },
    #[error("required key '{0}' is missing")]
    MissingProviderClass(&'static str),
    #[error("required key '{0}' must not be empty")]
    EmptyProviderClass(&'static str),
}

impl PoolConfig {
    /// Parse a JSON value into a [`PoolConfig`]. Fails fast on
    /// the first shape violation; the caller maps the error to
    /// HTTP 400.
    pub fn parse(value: &Value) -> Result<Self, PoolConfigParseError> {
        let object = value
            .as_object()
            .ok_or(PoolConfigParseError::NotAnObject)?;
        Self::parse_object(object)
    }

    fn parse_object(object: &Map<String, Value>) -> Result<Self, PoolConfigParseError> {
        let mut entries = BTreeMap::new();
        for (k, v) in object {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Null => continue,
                Value::Bool(_) => return Err(non_string(k, "bool")),
                Value::Number(_) => return Err(non_string(k, "number")),
                Value::Array(_) => return Err(non_string(k, "array")),
                Value::Object(_) => return Err(non_string(k, "object")),
            };
            entries.insert(k.clone(), s);
        }
        match entries.get(PROVIDER_CLASS_KEY) {
            None => Err(PoolConfigParseError::MissingProviderClass(PROVIDER_CLASS_KEY)),
            Some(v) if v.is_empty() => {
                Err(PoolConfigParseError::EmptyProviderClass(PROVIDER_CLASS_KEY))
            }
            Some(_) => Ok(Self { entries }),
        }
    }

    /// The provider type — guaranteed present and non-empty by
    /// the parser.
    pub fn provider_class(&self) -> &str {
        // Safe: parser established this invariant.
        self.entries.get(PROVIDER_CLASS_KEY).map(String::as_str).unwrap()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Serialise back to a JSON object. Secret references — values
    /// of the form `${...}` — are stored verbatim as plain strings,
    /// so this round-trips them unchanged.
    pub fn to_json_value(&self) -> Value {
        let mut out = Map::with_capacity(self.entries.len());
        for (k, v) in &self.entries {
            out.insert(k.clone(), Value::String(v.clone()));
        }
        Value::Object(out)
    }
}

fn non_string(key: &str, kind: &'static str) -> PoolConfigParseError {
    PoolConfigParseError::NonStringValue {
        key: key.to_string(),
        kind,
    }
}

/// True for any value of the form `${...}`. Per § 3.5 the
/// controlplane treats these as opaque secret references and
/// never resolves them — resolution happens at pool load time
/// inside the provider impl.
pub fn is_secret_ref(value: &str) -> bool {
    value.len() >= 3 && value.starts_with("${") && value.ends_with('}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_flat_string_map() {
        let v = json!({ "provider.class": "morph", "k": "v" });
        let cfg = PoolConfig::parse(&v).unwrap();
        assert_eq!(cfg.provider_class(), "morph");
        assert_eq!(cfg.get("k"), Some("v"));
    }

    #[test]
    fn rejects_non_string_values() {
        let v = json!({ "provider.class": "morph", "n": 1 });
        assert!(matches!(
            PoolConfig::parse(&v),
            Err(PoolConfigParseError::NonStringValue { .. })
        ));
    }

    #[test]
    fn requires_provider_class() {
        let v = json!({ "k": "v" });
        assert!(matches!(
            PoolConfig::parse(&v),
            Err(PoolConfigParseError::MissingProviderClass(_))
        ));
    }

    #[test]
    fn round_trips_secret_refs_verbatim() {
        let v = json!({
            "provider.class": "morph",
            "morph.api_key": "${env:MORPH_API_KEY}",
        });
        let cfg = PoolConfig::parse(&v).unwrap();
        assert_eq!(cfg.to_json_value(), v);
        assert!(is_secret_ref("${env:X}"));
    }
}
