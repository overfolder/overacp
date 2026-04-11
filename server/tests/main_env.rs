//! Coverage for the env-driven startup logic in `server/src/main.rs`,
//! exercised through the `build_state_from_env` seam exposed by the
//! library. These tests mutate process env vars and so are serialized
//! through a single mutex; cargo runs integration tests in-process.

use std::env;
use std::sync::{Mutex, MutexGuard};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use overacp_server::{build_state_from_env, router, StartupError};
use tower::ServiceExt;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const VARS: &[&str] = &["OVERACP_JWT_SIGNING_KEY", "OVERACP_JWT_ISSUER"];

/// RAII helper that snapshots the env vars we touch and restores them
/// on drop, so a panicking test can't leak state into its neighbours.
struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let saved = VARS.iter().map(|k| (*k, env::var(k).ok())).collect();
        for k in VARS {
            env::remove_var(k);
        }
        Self { saved, _lock: lock }
    }

    fn set(&self, k: &str, v: &str) {
        env::set_var(k, v);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => env::set_var(k, val),
                None => env::remove_var(k),
            }
        }
    }
}

async fn healthz_status(state: overacp_server::AppState) -> StatusCode {
    let app = router(state);
    app.oneshot(
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test(flavor = "current_thread")]
async fn missing_signing_key_errors() {
    let _g = EnvGuard::new();
    let err = build_state_from_env()
        .err()
        .expect("should fail without signing key");
    assert!(matches!(err, StartupError::MissingSigningKey));
    assert!(err.to_string().contains("OVERACP_JWT_SIGNING_KEY"));
}

#[tokio::test(flavor = "current_thread")]
async fn signing_key_only_uses_default_issuer() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    let state = build_state_from_env().expect("should build");
    assert_eq!(state.authenticator.issuer(), "overacp");
    // And the state produces a live router with a responsive healthz.
    assert_eq!(healthz_status(state).await, StatusCode::OK);
}

#[tokio::test(flavor = "current_thread")]
async fn custom_issuer_is_honoured() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    g.set("OVERACP_JWT_ISSUER", "custom-issuer");
    let state = build_state_from_env().expect("should build");
    assert_eq!(state.authenticator.issuer(), "custom-issuer");
}
