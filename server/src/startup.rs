//! Env-driven construction of [`AppState`] for the `overacp-server`
//! binary. Extracted from `main.rs` so the failure modes can be
//! exercised by integration tests without spawning a process or
//! binding a socket.

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use crate::api::default_registry;
use crate::{AppState, HtpasswdFile, InMemoryStore, StaticJwtAuthenticator};

/// Errors that can occur while assembling [`AppState`] from environment
/// variables. Each variant maps to one of the loud failure modes the
/// binary intentionally exposes at startup.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("OVERACP_JWT_SIGNING_KEY is required (set it in .env or the environment)")]
    MissingSigningKey,
    #[error("failed to load OVERACP_BASIC_AUTH_FILE='{path}': {source}")]
    HtpasswdLoad {
        path: String,
        #[source]
        source: crate::HtpasswdError,
    },
    #[error("OVERACP_DEFAULT_USER_ID is not a valid UUID: {0}")]
    InvalidDefaultUserId(#[source] uuid::Error),
}

/// Build the server's [`AppState`] from process environment variables.
///
/// See `docs/design/` and `main.rs` for the documented contract; the
/// short version:
///
/// - `OVERACP_JWT_SIGNING_KEY` (required)
/// - `OVERACP_JWT_ISSUER` (optional, defaults to `"overacp"`)
/// - `OVERACP_BASIC_AUTH_FILE` (optional; empty string treated as
///   unset; control-plane endpoints return 503 when absent)
/// - `OVERACP_DEFAULT_USER_ID` (optional UUID; empty string treated as
///   unset)
pub fn build_state_from_env() -> Result<AppState, StartupError> {
    let signing_key =
        env::var("OVERACP_JWT_SIGNING_KEY").map_err(|_| StartupError::MissingSigningKey)?;
    let issuer = env::var("OVERACP_JWT_ISSUER").unwrap_or_else(|_| "overacp".to_string());

    let mut state = AppState::new(
        Arc::new(InMemoryStore::new()),
        Arc::new(default_registry()),
        Arc::new(StaticJwtAuthenticator::new(signing_key, issuer)),
    );

    // Optional: htpasswd file for control-plane HTTP Basic auth.
    // If unset the control-plane endpoints return 503 — a deliberately
    // loud failure mode rather than open-by-default.
    match env::var("OVERACP_BASIC_AUTH_FILE") {
        Ok(path) if !path.is_empty() => {
            let file = HtpasswdFile::load(&PathBuf::from(&path)).map_err(|source| {
                StartupError::HtpasswdLoad {
                    path: path.clone(),
                    source,
                }
            })?;
            tracing::info!(
                users = file.user_count(),
                path = %path,
                "loaded control-plane htpasswd file",
            );
            state = state.with_basic_auth(Arc::new(file));
        }
        _ => tracing::warn!(
            "OVERACP_BASIC_AUTH_FILE not set — control-plane endpoints will return 503"
        ),
    }

    // Optional: external tunnel base URL injected into spawned
    // agents as `OVERACP_TUNNEL_URL` (per protocol.md § 2.4).
    if let Ok(raw) = env::var("OVERACP_TUNNEL_BASE_URL") {
        if !raw.is_empty() {
            state = state.with_tunnel_base_url(raw);
        }
    }

    // Optional: default user UUID attributed to control-plane writes.
    if let Ok(raw) = env::var("OVERACP_DEFAULT_USER_ID") {
        if !raw.is_empty() {
            let user = Uuid::parse_str(&raw).map_err(StartupError::InvalidDefaultUserId)?;
            state = state.with_default_user_id(user);
        }
    }

    Ok(state)
}
