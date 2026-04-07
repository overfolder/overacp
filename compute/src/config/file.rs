use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::fs;
use tokio::sync::Mutex;

use crate::config::ConfigProvider;
use crate::error::ConfigError;

/// Resolves `${file:/path/to/file:dotted.key}` references.
///
/// The file is read once on first lookup and cached. Both TOML and JSON
/// are supported (auto-detected by extension).
#[derive(Debug, Default)]
pub struct FileConfigProvider {
    cache: Mutex<HashMap<PathBuf, Arc<Value>>>,
}

impl FileConfigProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConfigProvider for FileConfigProvider {
    fn scheme(&self) -> &'static str {
        "file"
    }

    async fn lookup(&self, path: &str, key: Option<&str>) -> Result<String, ConfigError> {
        let key = key.ok_or_else(|| ConfigError::InvalidValue {
            key: path.to_owned(),
            msg: "file references require a key: ${file:/path:key}".into(),
        })?;
        let pb = PathBuf::from(path);
        let value = self.load(&pb).await?;
        let resolved = lookup_dotted(&value, key)
            .ok_or_else(|| ConfigError::MissingKey(format!("file:{path}:{key}")))?;
        value_to_string(&resolved).ok_or_else(|| ConfigError::InvalidValue {
            key: format!("file:{path}:{key}"),
            msg: "value is not a scalar".into(),
        })
    }
}

impl FileConfigProvider {
    async fn load(&self, path: &Path) -> Result<Arc<Value>, ConfigError> {
        let mut guard = self.cache.lock().await;
        if let Some(v) = guard.get(path) {
            return Ok(v.clone());
        }
        let raw = fs::read_to_string(path)
            .await
            .map_err(|e| ConfigError::Resolution {
                reference: format!("file:{}", path.display()),
                source: Box::new(e),
            })?;
        let parsed = match path.extension().and_then(|e| e.to_str()) {
            Some("json") => {
                serde_json::from_str::<Value>(&raw).map_err(|e| ConfigError::Resolution {
                    reference: format!("file:{}", path.display()),
                    source: Box::new(e),
                })?
            }
            Some("toml") => {
                let v: toml::Value = toml::from_str(&raw).map_err(|e| ConfigError::Resolution {
                    reference: format!("file:{}", path.display()),
                    source: Box::new(e),
                })?;
                toml_to_json(v)
            }
            _ => {
                return Err(ConfigError::InvalidValue {
                    key: path.display().to_string(),
                    msg: "file must have .toml or .json extension".into(),
                })
            }
        };
        let arc = Arc::new(parsed);
        guard.insert(path.to_owned(), arc.clone());
        Ok(arc)
    }
}

fn lookup_dotted(root: &Value, dotted: &str) -> Option<Value> {
    let mut cur = root;
    for part in dotted.split('.') {
        cur = cur.as_object()?.get(part)?;
    }
    Some(cur.clone())
}

fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn toml_to_json(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
        toml::Value::Array(a) => Value::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn reads_toml_nested_key() {
        let mut f = tempfile::Builder::new().suffix(".toml").tempfile().unwrap();
        writeln!(f, "[db]\npassword = \"s3cret\"").unwrap();
        let p = FileConfigProvider::new();
        let v = p
            .lookup(f.path().to_str().unwrap(), Some("db.password"))
            .await
            .unwrap();
        assert_eq!(v, "s3cret");
    }

    #[tokio::test]
    async fn reads_json_nested_key() {
        let mut f = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
        writeln!(f, r#"{{"db":{{"password":"s3cret"}}}}"#).unwrap();
        let p = FileConfigProvider::new();
        let v = p
            .lookup(f.path().to_str().unwrap(), Some("db.password"))
            .await
            .unwrap();
        assert_eq!(v, "s3cret");
    }

    #[tokio::test]
    async fn missing_key_errors() {
        let mut f = tempfile::Builder::new().suffix(".toml").tempfile().unwrap();
        writeln!(f, "x = 1").unwrap();
        let p = FileConfigProvider::new();
        assert!(matches!(
            p.lookup(f.path().to_str().unwrap(), Some("nope")).await,
            Err(ConfigError::MissingKey(_))
        ));
    }

    #[tokio::test]
    async fn rejects_unknown_extension() {
        let f = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
        let p = FileConfigProvider::new();
        assert!(matches!(
            p.lookup(f.path().to_str().unwrap(), Some("k")).await,
            Err(ConfigError::InvalidValue { .. })
        ));
    }
}
