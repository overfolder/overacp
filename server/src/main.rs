use std::env;
use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::Arc;

use overacp_server::api::default_registry;
use overacp_server::{AppState, HtpasswdFile, InMemoryStore, StaticJwtAuthenticator};
use tokio::net::TcpListener;
use tracing_subscriber::fmt;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn StdError>> {
    // Load .env if present. Missing file is fine; missing required
    // vars below is not.
    let _ = dotenvy::dotenv();
    fmt::init();

    let signing_key = env::var("OVERACP_JWT_SIGNING_KEY")
        .map_err(|_| "OVERACP_JWT_SIGNING_KEY is required (set it in .env or the environment)")?;
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
            let file = HtpasswdFile::load(&PathBuf::from(&path)).map_err(|e| {
                format!("failed to load OVERACP_BASIC_AUTH_FILE='{path}': {e}")
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

    // Optional: default user UUID attributed to control-plane writes.
    if let Ok(raw) = env::var("OVERACP_DEFAULT_USER_ID") {
        if !raw.is_empty() {
            let user = Uuid::parse_str(&raw)
                .map_err(|e| format!("OVERACP_DEFAULT_USER_ID is not a valid UUID: {e}"))?;
            state = state.with_default_user_id(user);
        }
    }

    let app = overacp_server::router(state);

    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("overacp-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
