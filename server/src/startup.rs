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

    #[cfg(feature = "redis")]
    #[error("redis connection failed: {0}")]
    Redis(#[from] redis::RedisError),
}

/// Build the server's [`AppState`] from process environment variables.
///
/// Contract:
///
/// - `OVERACP_JWT_SIGNING_KEY` (required)
/// - `OVERACP_JWT_ISSUER` (optional, defaults to `"overacp"`)
/// - `OVERACP_REDIS_URL` (optional, enables Redis backend for HA)
/// - `OVERACP_INSTANCE_ID` (optional, defaults to hostname or random)
pub async fn build_state_from_env() -> Result<AppState, StartupError> {
    let signing_key =
        env::var("OVERACP_JWT_SIGNING_KEY").map_err(|_| StartupError::MissingSigningKey)?;
    let issuer = env::var("OVERACP_JWT_ISSUER").unwrap_or_else(|_| "overacp".to_string());

    #[allow(unused_mut)]
    let mut state = AppState::new(Arc::new(StaticJwtAuthenticator::new(signing_key, issuer)));

    #[cfg(feature = "redis")]
    if let Ok(redis_url) = env::var("OVERACP_REDIS_URL") {
        use crate::redis_backend;
        tracing::info!(
            redis_url = %redis_url,
            instance_id = %redis_backend::instance_id(),
            "enabling redis backend for multi-instance HA"
        );
        let providers = redis_backend::RedisProviders::from_url(&redis_url).await?;
        state = state.with_redis_providers(providers);
    }

    Ok(state)
}
