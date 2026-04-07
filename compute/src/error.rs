use std::error::Error as StdError;
use std::io;

use thiserror::Error;

use crate::node::NodeId;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("node not found: {0}")]
    NotFound(NodeId),
    #[error("invalid config: {0}")]
    Config(#[from] ConfigError),
    #[error("provider backend: {0}")]
    Backend(String),
    #[error("timeout")]
    Timeout,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("missing required key: {0}")]
    MissingKey(String),
    #[error("invalid value for {key}: {msg}")]
    InvalidValue { key: String, msg: String },
    #[error("unknown secret provider: {0}")]
    UnknownProvider(String),
    #[error("secret lookup failed for {reference}: {source}")]
    Resolution {
        reference: String,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
    #[error("malformed reference: {0}")]
    BadReference(String),
}
