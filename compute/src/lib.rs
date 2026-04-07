//! Core types and traits for over/ACP compute providers.
//!
//! See `docs/design/controlplane.md` §3.5 + §4 for the design.

pub mod config;
pub mod error;
pub mod exec;
pub mod logs;
pub mod node;
pub mod provider;
pub mod providers;

pub use config::{
    ConfigProvider, ConfigResolver, EnvConfigProvider, FileConfigProvider, RawConfig,
    ResolvedConfig,
};
pub use error::{ConfigError, ProviderError};
pub use exec::{ExecRequest, ExecResult};
pub use logs::LogStream;
pub use node::{NetworkInfo, NodeDescription, NodeHandle, NodeId, NodeSpec, NodeStatus};
pub use provider::ComputeProvider;
