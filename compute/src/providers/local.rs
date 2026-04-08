//! `local-process` provider — spawns `overacp-agent` as a local
//! subprocess. Zero infra; this is the default that makes the demo
//! work without Docker, Morph, or k8s.
//!
//! Each "node" is a child process. The PID is the [`NodeId`].
//! `exec` runs subcommands in the node's per-node workspace dir;
//! `stream_logs` replays captured stdout/stderr.
//!
//! See `docs/design/controlplane.md` § 5.

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::{json, Map, Value};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, Mutex};
use tokio::time;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use crate::config::ResolvedConfig;
use crate::error::{ConfigError, ProviderError};
use crate::exec::{ExecRequest, ExecResult};
use crate::logs::LogStream;
use crate::node::{NodeDescription, NodeHandle, NodeId, NodeSpec, NodeStatus};
use crate::provider::ComputeProvider;

/// `provider.class` value matched against pool configs.
pub const PROVIDER_TYPE: &str = "local-process";

/// Default agent binary if `local.agent_binary` is not set.
pub const DEFAULT_AGENT_BINARY: &str = "overacp-agent";

const LOG_CHANNEL_CAPACITY: usize = 1024;

struct LocalNode {
    handle: NodeHandle,
    workspace: PathBuf,
    image: String,
    cpu: Option<u32>,
    memory_gb: Option<u32>,
    disk_gb: Option<u32>,
    created_at: DateTime<Utc>,
    child: Mutex<Option<Child>>,
    logs_tx: broadcast::Sender<Bytes>,
    status: Mutex<NodeStatus>,
}

/// Local-process compute provider. One instance per pool.
pub struct LocalProvider {
    agent_binary: String,
    workspace_root: PathBuf,
    nodes: Mutex<BTreeMap<String, Arc<LocalNode>>>,
}

impl LocalProvider {
    pub fn new(agent_binary: impl Into<String>, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            agent_binary: agent_binary.into(),
            workspace_root: workspace_root.into(),
            nodes: Mutex::new(BTreeMap::new()),
        }
    }

    async fn get(&self, id: &NodeId) -> Result<Arc<LocalNode>, ProviderError> {
        self.nodes
            .lock()
            .await
            .get(&id.0)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(id.clone()))
    }
}

#[async_trait]
impl ComputeProvider for LocalProvider {
    fn provider_type() -> &'static str {
        PROVIDER_TYPE
    }

    // `supports_multi_agent_nodes` and `supports_node_reuse` use the
    // trait defaults (`false`/`false`) — see `docs/design/controlplane.md`
    // § 4. The `capability_flags_match_design_doc_defaults` test pins the
    // values so a future change here is intentional.

    fn from_config(config: ResolvedConfig) -> Result<Self, ProviderError> {
        let agent_binary = config
            .get("local.agent_binary")
            .unwrap_or(DEFAULT_AGENT_BINARY)
            .to_owned();
        let workspace_root = config
            .get("local.workspace_root")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir().join("overacp-local"));
        Ok(Self::new(agent_binary, workspace_root))
    }

    fn validate_config(config: &Map<String, Value>) -> Result<(), ConfigError> {
        for key in ["local.agent_binary", "local.workspace_root"] {
            if let Some(v) = config.get(key) {
                if !v.is_string() {
                    return Err(ConfigError::InvalidValue {
                        key: key.to_owned(),
                        msg: "must be a string".to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    async fn create_node(&self, spec: NodeSpec) -> Result<NodeHandle, ProviderError> {
        let workspace = self
            .workspace_root
            .join(format!("node-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&workspace).await?;

        let mut cmd = Command::new(&self.agent_binary);
        cmd.current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        cmd.env("OVERACP_AGENT_JWT", &spec.jwt);

        let mut child = cmd
            .spawn()
            .map_err(|e| ProviderError::Backend(format!("spawn {}: {e}", self.agent_binary)))?;

        let pid = child
            .id()
            .ok_or_else(|| ProviderError::Backend("child has no pid".into()))?;
        let id = NodeId(pid.to_string());

        let (logs_tx, _) = broadcast::channel::<Bytes>(LOG_CHANNEL_CAPACITY);
        if let Some(stdout) = child.stdout.take() {
            spawn_log_pump(stdout, logs_tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_pump(stderr, logs_tx.clone());
        }

        let handle = NodeHandle {
            id: id.clone(),
            provider_metadata: json!({
                "pid": pid,
                "workspace": workspace,
            }),
        };

        let node = Arc::new(LocalNode {
            handle: handle.clone(),
            workspace,
            image: spec.image,
            cpu: spec.cpu,
            memory_gb: spec.memory_gb,
            disk_gb: spec.disk_gb,
            created_at: Utc::now(),
            child: Mutex::new(Some(child)),
            logs_tx,
            status: Mutex::new(NodeStatus::Running),
        });

        self.nodes.lock().await.insert(id.0.clone(), node);
        tracing::info!(node = %id, "spawned local node");
        Ok(handle)
    }

    async fn list_nodes(&self) -> Result<Vec<NodeHandle>, ProviderError> {
        Ok(self
            .nodes
            .lock()
            .await
            .values()
            .map(|n| n.handle.clone())
            .collect())
    }

    async fn describe_node(&self, id: &NodeId) -> Result<NodeDescription, ProviderError> {
        let node = self.get(id).await?;
        let status = *node.status.lock().await;
        Ok(NodeDescription {
            handle: node.handle.clone(),
            status,
            image: node.image.clone(),
            cpu: node.cpu,
            memory_gb: node.memory_gb,
            disk_gb: node.disk_gb,
            network: None,
            created_at: node.created_at,
        })
    }

    async fn delete_node(&self, id: &NodeId) -> Result<(), ProviderError> {
        let node = match self.nodes.lock().await.remove(&id.0) {
            Some(n) => n,
            None => return Ok(()),
        };
        if let Some(mut child) = node.child.lock().await.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        *node.status.lock().await = NodeStatus::Exited;
        let _ = fs::remove_dir_all(&node.workspace).await;
        tracing::info!(node = %id, "deleted local node");
        Ok(())
    }

    async fn exec(&self, id: &NodeId, req: ExecRequest) -> Result<ExecResult, ProviderError> {
        let node = self.get(id).await?;
        if req.command.is_empty() {
            return Err(ProviderError::Backend(
                "command must be a non-empty argv array".into(),
            ));
        }

        let cwd = req
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| node.workspace.clone());
        let mut cmd = Command::new(&req.command[0]);
        cmd.args(&req.command[1..])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(env) = &req.env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        let run = async move {
            let output = cmd.output().await?;
            Ok::<_, ProviderError>(ExecResult {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
            })
        };

        match req.timeout_s {
            Some(secs) => time::timeout(Duration::from_secs(secs.into()), run)
                .await
                .map_err(|_| ProviderError::Timeout)?,
            None => run.await,
        }
    }

    async fn stream_logs(&self, id: &NodeId) -> Result<LogStream, ProviderError> {
        let node = self.get(id).await?;
        let rx = node.logs_tx.subscribe();
        let node_id = id.clone();
        // Map BroadcastStream's `Lagged(n)` errors into a tracing
        // warning so slow consumers don't silently lose log lines.
        let stream = BroadcastStream::new(rx).filter_map(move |item| {
            let node_id = node_id.clone();
            async move {
                match item {
                    Ok(bytes) => Some(Ok(bytes)),
                    Err(BroadcastStreamRecvError::Lagged(n)) => {
                        tracing::warn!(
                            node = %node_id,
                            dropped = n,
                            "log stream consumer lagged; dropped lines"
                        );
                        None
                    }
                }
            }
        });
        Ok(Box::pin(stream))
    }
}

fn spawn_log_pump<R>(reader: R, tx: broadcast::Sender<Bytes>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let _ = tx.send(Bytes::copy_from_slice(line.as_bytes()));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(image: &str) -> NodeSpec {
        NodeSpec {
            image: image.to_owned(),
            cpu: None,
            memory_gb: None,
            disk_gb: None,
            env: BTreeMap::new(),
            jwt: "test-jwt".to_owned(),
            provider_overrides: Map::new(),
        }
    }

    #[tokio::test]
    async fn lifecycle() {
        let root = tempfile::tempdir().unwrap();
        // `cat` stands in for the (not-yet-built) overacp-agent: stdin
        // is null so it exits immediately, but we still get a real
        // PID, workspace, and log channel.
        let provider = LocalProvider::new("cat", root.path());
        let handle = provider.create_node(spec("test:latest")).await.unwrap();
        assert!(!handle.id.0.is_empty());

        let listed = provider.list_nodes().await.unwrap();
        assert_eq!(listed.len(), 1);

        let desc = provider.describe_node(&handle.id).await.unwrap();
        assert_eq!(desc.image, "test:latest");

        let exec = provider
            .exec(
                &handle.id,
                ExecRequest {
                    command: vec!["sh".into(), "-c".into(), "echo hello".into()],
                    cwd: None,
                    env: None,
                    timeout_s: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(exec.exit_code, 0);
        assert!(exec.stdout.contains("hello"));

        provider.delete_node(&handle.id).await.unwrap();
        assert!(provider.list_nodes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn exec_on_unknown_node_is_not_found() {
        let root = tempfile::tempdir().unwrap();
        let provider = LocalProvider::new("cat", root.path());
        let err = provider
            .exec(
                &NodeId("nope".into()),
                ExecRequest {
                    command: vec!["true".into()],
                    cwd: None,
                    env: None,
                    timeout_s: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
    }

    #[tokio::test]
    async fn exec_timeout_returns_timeout_error() {
        let root = tempfile::tempdir().unwrap();
        let provider = LocalProvider::new("cat", root.path());
        let handle = provider.create_node(spec("test")).await.unwrap();
        let err = provider
            .exec(
                &handle.id,
                ExecRequest {
                    command: vec!["sleep".into(), "5".into()],
                    cwd: None,
                    env: None,
                    timeout_s: Some(1),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Timeout));
    }

    #[test]
    fn capability_flags_match_design_doc_defaults() {
        assert!(!LocalProvider::supports_multi_agent_nodes());
        assert!(!LocalProvider::supports_node_reuse());
    }

    #[test]
    fn validate_config_rejects_non_string() {
        let mut cfg = Map::new();
        cfg.insert("local.agent_binary".into(), json!(42));
        assert!(LocalProvider::validate_config(&cfg).is_err());
    }
}
