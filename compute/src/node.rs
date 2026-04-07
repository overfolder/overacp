use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NodeId {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(NodeId(s.to_owned()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSpec {
    pub image: String,
    #[serde(default)]
    pub cpu: Option<u32>,
    #[serde(default)]
    pub memory_gb: Option<u32>,
    #[serde(default)]
    pub disk_gb: Option<u32>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Agent JWT injected by the controlplane.
    pub jwt: String,
    #[serde(default)]
    pub provider_overrides: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHandle {
    pub id: NodeId,
    #[serde(default)]
    pub provider_metadata: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Running,
    Idle,
    Exited,
    Errored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInfo {
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub ssh_url: Option<String>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDescription {
    pub handle: NodeHandle,
    pub status: NodeStatus,
    pub image: String,
    #[serde(default)]
    pub cpu: Option<u32>,
    #[serde(default)]
    pub memory_gb: Option<u32>,
    #[serde(default)]
    pub disk_gb: Option<u32>,
    #[serde(default)]
    pub network: Option<NetworkInfo>,
    pub created_at: DateTime<Utc>,
}
