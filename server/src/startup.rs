//! Env-driven construction of [`AppState`] for the `overacp-server`
//! binary. Extracted from `main.rs` so the failure modes can be
//! exercised by integration tests without spawning a process or
//! binding a socket.

use std::env;
use std::sync::Arc;

use crate::{AppState, StaticJwtAuthenticator};

/// Errors that can occur while assembling [`AppState`] from environment
/// variables.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("OVERACP_JWT_SIGNING_KEY is required (set it in .env or the environment)")]
    MissingSigningKey,
}

/// Build the server's [`AppState`] from process environment variables.
///
/// Contract:
///
/// - `OVERACP_JWT_SIGNING_KEY` (required)
/// - `OVERACP_JWT_ISSUER` (optional, defaults to `"overacp"`)
pub fn build_state_from_env() -> Result<AppState, StartupError> {
    let signing_key =
        env::var("OVERACP_JWT_SIGNING_KEY").map_err(|_| StartupError::MissingSigningKey)?;
    let issuer = env::var("OVERACP_JWT_ISSUER").unwrap_or_else(|_| "overacp".to_string());

    Ok(AppState::new(Arc::new(StaticJwtAuthenticator::new(
        signing_key,
        issuer,
    ))))
}
