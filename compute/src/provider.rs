use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::config::ResolvedConfig;
use crate::error::{ConfigError, ProviderError};
use crate::exec::{ExecRequest, ExecResult};
use crate::logs::LogStream;
use crate::node::{NodeDescription, NodeHandle, NodeId, NodeSpec};

/// A backend that can provision and manage compute nodes for over/ACP agents.
///
/// Implementations live in their own crates (`overacp-compute-local`,
/// `overacp-compute-docker`, `overacp-compute-morph`, ...). The
/// controlplane discovers them via a `ProviderRegistry` at startup.
#[async_trait]
pub trait ComputeProvider: Send + Sync {
    /// Stable identifier matched against `provider.class` in pool config.
    fn provider_type() -> &'static str
    where
        Self: Sized;

    /// Construct from a resolved config map. Called once per pool at load
    /// time. Implementations should validate eagerly.
    fn from_config(config: ResolvedConfig) -> Result<Self, ProviderError>
    where
        Self: Sized;

    /// Pure validation hook for `POST /compute/providers/{type}/config/validate`.
    fn validate_config(config: &Map<String, Value>) -> Result<(), ConfigError>
    where
        Self: Sized;

    /// Provision a new node and return its handle.
    async fn create_node(&self, spec: NodeSpec) -> Result<NodeHandle, ProviderError>;

    /// List every node currently owned by this pool.
    async fn list_nodes(&self) -> Result<Vec<NodeHandle>, ProviderError>;

    /// Describe a single node — status, image, resource usage, network info.
    async fn describe_node(&self, id: &NodeId) -> Result<NodeDescription, ProviderError>;

    /// Tear down a node. Idempotent.
    async fn delete_node(&self, id: &NodeId) -> Result<(), ProviderError>;

    /// One-shot command execution. Mirrors Morph Cloud's `Instance.exec`.
    async fn exec(&self, id: &NodeId, req: ExecRequest) -> Result<ExecResult, ProviderError>;

    /// Stream the node's stdout/stderr as a tokio stream of byte chunks.
    async fn stream_logs(&self, id: &NodeId) -> Result<LogStream, ProviderError>;
}
