//! End-to-end check that control-plane endpoints sit behind HTTP
//! Basic auth, while `/healthz` and the agent-facing routes do not.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use overacp_server::api::default_registry;
use overacp_server::{router, AppState, HtpasswdFile, InMemoryStore, StaticJwtAuthenticator};
use tower::ServiceExt;

fn auth_header(user: &str, pass: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{user}:{pass}")))
}

fn base_state() -> AppState {
    AppState::new(
        Arc::new(InMemoryStore::new()),
        Arc::new(default_registry()),
        Arc::new(StaticJwtAuthenticator::new("k", "overacp")),
    )
}

fn loaded_htpasswd() -> Arc<HtpasswdFile> {
    let hash = bcrypt::hash("hunter2", 4).unwrap();
    Arc::new(HtpasswdFile::parse(&format!("alice:{hash}\n")).unwrap())
}

#[tokio::test]
async fn control_plane_returns_503_when_no_htpasswd_loaded() {
    let app = router(base_state());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/compute/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn control_plane_rejects_missing_credentials() {
    let app = router(base_state().with_basic_auth(loaded_htpasswd()));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/compute/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers().get("www-authenticate").unwrap(),
        "Basic realm=\"overacp\""
    );
}

#[tokio::test]
async fn control_plane_rejects_wrong_password() {
    let app = router(base_state().with_basic_auth(loaded_htpasswd()));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/compute/providers")
                .header("authorization", auth_header("alice", "wrong"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn control_plane_accepts_correct_credentials() {
    let app = router(base_state().with_basic_auth(loaded_htpasswd()));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/compute/providers")
                .header("authorization", auth_header("alice", "hunter2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn healthz_is_not_protected() {
    let app = router(base_state());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
