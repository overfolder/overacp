//! End-to-end JSON tests for the agents REST surface (§ 3.4) against
//! a tempdir-backed `local-process` pool. Drives the full top-level
//! router via `tower::ServiceExt::oneshot` so we exercise the same
//! wiring `main.rs` would.

use std::path::Path;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use overacp_server::api::{default_registry, AgentStatusResponse, AgentView, CreateAgentResponse};
use overacp_server::{
    router, AgentStatus, AppState, Authenticator, InMemoryStore, StaticJwtAuthenticator,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const SIGNING_KEY: &str = "test-key";
const ISSUER: &str = "overacp";

struct Harness {
    app: Router,
    auth: Arc<StaticJwtAuthenticator>,
    _workspace: TempDir,
}

fn harness() -> Harness {
    let workspace = tempfile::tempdir().expect("tempdir");
    let auth = Arc::new(StaticJwtAuthenticator::new(SIGNING_KEY, ISSUER));
    let state = AppState::new(
        Arc::new(InMemoryStore::new()),
        Arc::new(default_registry()),
        auth.clone(),
    );
    let app = router(state);
    Harness {
        app,
        auth,
        _workspace: workspace,
    }
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Bytes) {
    let mut req = Request::builder().method(method).uri(uri);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let req = req
        .body(Body::from(body.map(str::to_owned).unwrap_or_default()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, bytes)
}

fn parse<T: DeserializeOwned>(b: &Bytes) -> T {
    serde_json::from_slice(b)
        .unwrap_or_else(|e| panic!("decode failed: {e}\nbody: {}", String::from_utf8_lossy(b)))
}

fn local_pool_body(name: &str, workspace: &Path) -> String {
    json!({
        "name": name,
        "config": {
            "provider.class": "local-process",
            // `/bin/sleep` with no args exits ~immediately on Linux,
            // but the local provider only needs the spawned PID for
            // bookkeeping. Same trick used by the nodes REST tests.
            "local.agent_binary": "/bin/sleep",
            "local.workspace_root": workspace.to_string_lossy(),
            "default.image": "overacp/loop:latest"
        }
    })
    .to_string()
}

async fn create_local_pool(app: &Router, name: &str, workspace: &Path) {
    let body = local_pool_body(name, workspace);
    let (status, body_bytes) = send(app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "pool create body: {}",
        String::from_utf8_lossy(&body_bytes)
    );
}

#[tokio::test]
async fn full_agent_lifecycle_against_local_pool() {
    let h = harness();
    let pool_workspace = tempfile::tempdir().unwrap();
    create_local_pool(&h.app, "local", pool_workspace.path()).await;

    let user = Uuid::new_v4();
    let create_body = json!({
        "pool": "local",
        "user": user,
        "metadata": { "tag": "smoke" }
    })
    .to_string();

    // POST /agents → 201
    let (status, body) = send(&h.app, "POST", "/agents", Some(&create_body)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create body: {}",
        String::from_utf8_lossy(&body)
    );
    let created: CreateAgentResponse = parse(&body);
    assert!(
        created.agent.id.starts_with("ag_"),
        "agent id should be prefixed: {}",
        created.agent.id
    );
    assert_eq!(created.agent.compute.provider_type, "local-process");
    assert_eq!(created.agent.compute.pool, "local");
    assert!(!created.agent.compute.node_id.is_empty());
    assert_eq!(created.agent.user, user);
    assert_eq!(created.agent.image, "overacp/loop:latest");
    assert_eq!(created.agent.status, AgentStatus::Idle);
    assert_eq!(created.agent.metadata, json!({ "tag": "smoke" }));
    assert!(!created.jwt.is_empty());

    // The minted JWT must round-trip through the authenticator and
    // its `conv` claim must point at this agent's conversation.
    let claims = h.auth.validate(&created.jwt).expect("jwt validates");
    assert_eq!(claims.conv, created.agent.conversation_id);
    assert_eq!(claims.user, user);

    // GET /agents → list of one.
    let (status, body) = send(&h.app, "GET", "/agents", None).await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<AgentView> = parse(&body);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, created.agent.id);

    // GET /agents?user=<other> → empty.
    let other = Uuid::new_v4();
    let (status, body) = send(&h.app, "GET", &format!("/agents?user={other}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<AgentView> = parse(&body);
    assert!(list.is_empty());

    // GET /agents/{id} → describe matches create.
    let (status, body) = send(
        &h.app,
        "GET",
        &format!("/agents/{}", created.agent.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let described: AgentView = parse(&body);
    assert_eq!(described.id, created.agent.id);
    assert_eq!(described.compute.node_id, created.agent.compute.node_id);

    // GET /agents/{id}/status → idle.
    let (status, body) = send(
        &h.app,
        "GET",
        &format!("/agents/{}/status", created.agent.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let s: AgentStatusResponse = parse(&body);
    assert_eq!(s.status, AgentStatus::Idle);

    // DELETE /agents/{id} → 204.
    let (status, _) = send(
        &h.app,
        "DELETE",
        &format!("/agents/{}", created.agent.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Subsequent GET → 404.
    let (status, _) = send(
        &h.app,
        "GET",
        &format!("/agents/{}", created.agent.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_against_unknown_pool_is_404() {
    let h = harness();
    let body = json!({ "pool": "nope", "user": Uuid::new_v4() }).to_string();
    let (status, _) = send(&h.app, "POST", "/agents", Some(&body)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_against_paused_pool_is_400() {
    let h = harness();
    let pool_workspace = tempfile::tempdir().unwrap();
    create_local_pool(&h.app, "local", pool_workspace.path()).await;
    let (status, _) = send(&h.app, "POST", "/compute/pools/local/pause", Some("{}")).await;
    assert_eq!(status, StatusCode::OK);

    let body = json!({ "pool": "local", "user": Uuid::new_v4() }).to_string();
    let (status, _) = send(&h.app, "POST", "/agents", Some(&body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_without_image_or_default_is_400() {
    let h = harness();
    let pool_workspace = tempfile::tempdir().unwrap();
    // Pool config with no `default.image`.
    let body = json!({
        "name": "bare",
        "config": {
            "provider.class": "local-process",
            "local.agent_binary": "/bin/sleep",
            "local.workspace_root": pool_workspace.path().to_string_lossy()
        }
    })
    .to_string();
    let (status, _) = send(&h.app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(status, StatusCode::CREATED);

    let body = json!({ "pool": "bare", "user": Uuid::new_v4() }).to_string();
    let (status, body_bytes) = send(&h.app, "POST", "/agents", Some(&body)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "body: {}",
        String::from_utf8_lossy(&body_bytes)
    );
}

#[tokio::test]
async fn empty_pool_field_is_400() {
    let h = harness();
    let body = json!({ "pool": "", "user": Uuid::new_v4() }).to_string();
    let (status, _) = send(&h.app, "POST", "/agents", Some(&body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
