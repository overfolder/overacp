//! axum routes for `/api/v1/compute/pools/{pool}/nodes`.
//!
//! Implements `docs/design/controlplane.md` § 3.3. Each handler
//! resolves the named pool to a live `ComputeProvider` via
//! [`AppState::pool_runtime`] and dispatches the call through it.
//! Node creation is intentionally absent — nodes are spawned by the
//! agent-creation flow (§ 3.4), not directly by REST.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::{Stream, StreamExt};
use overacp_compute_core::{
    ComputeProvider, ExecRequest, ExecResult, NodeDescription, NodeHandle, NodeId,
};

use crate::api::error::ApiError;
use crate::state::AppState;

/// Mount the node REST routes. Caller is expected to merge this
/// alongside the `compute_router()` providers/pools surface.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/compute/pools/:pool/nodes", get(list_nodes))
        .route(
            "/api/v1/compute/pools/:pool/nodes/:node_id",
            get(describe_node).delete(delete_node),
        )
        .route(
            "/api/v1/compute/pools/:pool/nodes/:node_id/exec",
            post(exec_node),
        )
        .route(
            "/api/v1/compute/pools/:pool/nodes/:node_id/logs",
            get(stream_node_logs),
        )
}

fn require_runtime(state: &AppState, pool: &str) -> Result<Arc<dyn ComputeProvider>, ApiError> {
    state
        .pool_runtime(pool)
        .ok_or_else(|| ApiError::NotFound(format!("pool '{pool}' has no live runtime")))
}

pub(crate) async fn list_nodes(
    State(state): State<AppState>,
    Path(pool): Path<String>,
) -> Result<Json<Vec<NodeHandle>>, ApiError> {
    let provider = require_runtime(&state, &pool)?;
    Ok(Json(provider.list_nodes().await?))
}

pub(crate) async fn describe_node(
    State(state): State<AppState>,
    Path((pool, node_id)): Path<(String, String)>,
) -> Result<Json<NodeDescription>, ApiError> {
    let provider = require_runtime(&state, &pool)?;
    Ok(Json(provider.describe_node(&NodeId(node_id)).await?))
}

pub(crate) async fn delete_node(
    State(state): State<AppState>,
    Path((pool, node_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let provider = require_runtime(&state, &pool)?;
    provider.delete_node(&NodeId(node_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn exec_node(
    State(state): State<AppState>,
    Path((pool, node_id)): Path<(String, String)>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResult>, ApiError> {
    if req.command.is_empty() {
        return Err(ApiError::BadRequest(
            "'command' must be a non-empty argv array".into(),
        ));
    }
    let provider = require_runtime(&state, &pool)?;
    Ok(Json(provider.exec(&NodeId(node_id), req).await?))
}

pub(crate) async fn stream_node_logs(
    State(state): State<AppState>,
    Path((pool, node_id)): Path<(String, String)>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let provider = require_runtime(&state, &pool)?;
    let log_stream = provider.stream_logs(&NodeId(node_id)).await?;
    let event_stream = log_stream.map(|chunk| {
        let event = match chunk {
            Ok(bytes) => Event::default().data(&*String::from_utf8_lossy(&bytes)),
            Err(e) => Event::default().event("error").data(e.to_string()),
        };
        Ok::<_, Infallible>(event)
    });
    Ok(Sse::new(event_stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::Json;
    use overacp_compute_core::providers::local::LocalProvider;
    use overacp_compute_core::{ComputeProvider, ExecRequest, NodeSpec};

    use super::*;
    use crate::api::default_registry;
    use crate::auth::StaticJwtAuthenticator;
    use crate::state::AppState;
    use crate::store::InMemoryStore;

    fn test_state() -> (AppState, Arc<LocalProvider>) {
        let store = Arc::new(InMemoryStore::new());
        let state = AppState::new(
            store,
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test", "overacp")),
        );
        // Use `/bin/sleep` so the spawned child stays alive for the
        // duration of the test without producing output we'd need to
        // synchronize against.
        let provider = Arc::new(LocalProvider::new(
            "/bin/sleep",
            env::temp_dir().join("overacp-nodes-test"),
        ));
        state.register_pool_runtime("pool-a", provider.clone() as Arc<dyn ComputeProvider>);
        (state, provider)
    }

    async fn spawn_one(provider: &Arc<LocalProvider>) -> String {
        // `sleep` reads its duration from argv, but the local
        // provider can't pass extra args today; the binary itself
        // running with no args will exit instantly on Linux. That's
        // fine for our purposes — `list_nodes`/`describe_node`/
        // `delete_node` only need the node to be *registered* with
        // the provider, not still running.
        let handle = provider
            .create_node(NodeSpec {
                image: "test".into(),
                cpu: None,
                memory_gb: None,
                disk_gb: None,
                env: Default::default(),
                jwt: "test-jwt".into(),
                provider_overrides: Default::default(),
            })
            .await
            .expect("create_node");
        handle.id.0
    }

    #[tokio::test]
    async fn list_describe_delete_roundtrip() {
        let (state, provider) = test_state();
        let id = spawn_one(&provider).await;

        let Json(list) = list_nodes(State(state.clone()), Path("pool-a".into()))
            .await
            .expect("list_nodes");
        assert!(list.iter().any(|h| h.id.0 == id));

        let Json(desc) = describe_node(State(state.clone()), Path(("pool-a".into(), id.clone())))
            .await
            .expect("describe_node");
        assert_eq!(desc.handle.id.0, id);

        let status = delete_node(State(state.clone()), Path(("pool-a".into(), id.clone())))
            .await
            .expect("delete_node");
        assert_eq!(status, StatusCode::NO_CONTENT);

        // After delete, the provider's list should no longer include it.
        let Json(list2) = list_nodes(State(state), Path("pool-a".into()))
            .await
            .expect("list_nodes after delete");
        assert!(!list2.iter().any(|h| h.id.0 == id));
    }

    #[tokio::test]
    async fn exec_returns_stdout() {
        // Build a provider whose agent_binary is `/bin/echo` so the
        // spawned node is harmless and exec dispatches a real
        // command on the host.
        let store = Arc::new(InMemoryStore::new());
        let state = AppState::new(
            store,
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test", "overacp")),
        );
        let provider = Arc::new(LocalProvider::new(
            "/bin/echo",
            env::temp_dir().join("overacp-nodes-test-exec"),
        ));
        state.register_pool_runtime("pool-a", provider.clone() as Arc<dyn ComputeProvider>);
        let id = spawn_one(&provider).await;

        let Json(result) = exec_node(
            State(state),
            Path(("pool-a".into(), id)),
            Json(ExecRequest {
                command: vec!["/bin/echo".into(), "hello-overacp".into()],
                cwd: None,
                env: None,
                timeout_s: Some(5),
            }),
        )
        .await
        .expect("exec_node");

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello-overacp"));
    }

    #[tokio::test]
    async fn exec_rejects_empty_command() {
        let (state, provider) = test_state();
        let id = spawn_one(&provider).await;
        let err = exec_node(
            State(state),
            Path(("pool-a".into(), id)),
            Json(ExecRequest {
                command: vec![],
                cwd: None,
                env: None,
                timeout_s: None,
            }),
        )
        .await
        .expect_err("empty command should be rejected");
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[tokio::test]
    async fn unknown_pool_is_404() {
        let (state, _) = test_state();
        let err = list_nodes(State(state), Path("nope".into()))
            .await
            .expect_err("unknown pool");
        assert!(matches!(err, ApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn unknown_node_is_404() {
        let (state, _) = test_state();
        let err = describe_node(
            State(state),
            Path(("pool-a".into(), "does-not-exist".into())),
        )
        .await
        .expect_err("unknown node");
        assert!(matches!(
            err,
            ApiError::Provider(overacp_compute_core::ProviderError::NotFound(_))
        ));
    }
}
