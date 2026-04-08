//! Coverage for the env-driven startup logic in `server/src/main.rs`,
//! exercised through the `build_state_from_env` seam exposed by the
//! library. These tests mutate process env vars and so are serialized
//! through a single mutex; cargo runs integration tests in-process.

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::{Mutex, MutexGuard};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use overacp_server::{build_state_from_env, router, StartupError};
use tower::ServiceExt;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const VARS: &[&str] = &[
    "OVERACP_JWT_SIGNING_KEY",
    "OVERACP_JWT_ISSUER",
    "OVERACP_BASIC_AUTH_FILE",
    "OVERACP_DEFAULT_USER_ID",
];

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

/// Write a temporary htpasswd file with one user `alice:hunter2`.
/// Returned path is unique per call so parallel test binaries don't
/// collide. Caller is responsible for keeping the path alive.
fn write_htpasswd() -> PathBuf {
    let hash = bcrypt::hash("hunter2", 4).unwrap();
    let mut path = env::temp_dir();
    path.push(format!(
        "overacp-test-htpasswd-{}-{}",
        process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut f = fs::File::create(&path).unwrap();
    writeln!(f, "alice:{hash}").unwrap();
    path
}

async fn control_plane_status(state: overacp_server::AppState) -> StatusCode {
    let app = router(state);
    app.oneshot(
        Request::builder()
            .uri("/compute/providers")
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
    let err = build_state_from_env().err().expect("should fail without signing key");
    assert!(matches!(err, StartupError::MissingSigningKey));
    assert!(err.to_string().contains("OVERACP_JWT_SIGNING_KEY"));
}

#[tokio::test(flavor = "current_thread")]
async fn signing_key_only_uses_default_issuer_and_no_basic_auth() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    let state = build_state_from_env().expect("should build");
    // Control plane is unprotected -> 503 per the documented loud
    // failure mode in main.rs.
    assert_eq!(
        control_plane_status(state).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test(flavor = "current_thread")]
async fn empty_basic_auth_file_treated_as_unset() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    g.set("OVERACP_BASIC_AUTH_FILE", "");
    let state = build_state_from_env().expect("should build");
    assert_eq!(
        control_plane_status(state).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test(flavor = "current_thread")]
async fn missing_basic_auth_file_errors() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    let bogus = env::temp_dir().join("overacp-this-file-should-not-exist-xyz.htpasswd");
    let _ = fs::remove_file(&bogus);
    g.set("OVERACP_BASIC_AUTH_FILE", bogus.to_str().unwrap());
    let err = build_state_from_env().err().expect("should fail");
    let msg = err.to_string();
    assert!(matches!(err, StartupError::HtpasswdLoad { .. }));
    assert!(
        msg.contains(bogus.to_str().unwrap()),
        "error should mention path: {msg}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn loaded_basic_auth_file_protects_control_plane() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    let path = write_htpasswd();
    g.set("OVERACP_BASIC_AUTH_FILE", path.to_str().unwrap());
    let state = build_state_from_env().expect("should build");
    let status = control_plane_status(state).await;
    let _ = fs::remove_file(&path);
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn empty_default_user_id_is_ignored() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    g.set("OVERACP_DEFAULT_USER_ID", "");
    build_state_from_env().expect("empty value should be tolerated");
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_default_user_id_errors() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    g.set("OVERACP_DEFAULT_USER_ID", "not-a-uuid");
    let err = build_state_from_env().err().expect("malformed UUID should fail");
    assert!(matches!(err, StartupError::InvalidDefaultUserId(_)));
    assert!(err.to_string().contains("valid UUID"));
}

#[tokio::test(flavor = "current_thread")]
async fn valid_default_user_id_accepted() {
    let g = EnvGuard::new();
    g.set("OVERACP_JWT_SIGNING_KEY", "k");
    g.set(
        "OVERACP_DEFAULT_USER_ID",
        "11111111-1111-1111-1111-111111111111",
    );
    build_state_from_env().expect("valid UUID should build");
}
