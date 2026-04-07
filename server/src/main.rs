use std::env;
use std::error::Error as StdError;
use std::sync::Arc;

use overacp_server::api::default_registry;
use overacp_server::{AppState, InMemoryStore, StaticJwtAuthenticator};
use tokio::net::TcpListener;
use tracing_subscriber::fmt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn StdError>> {
    // Load .env if present. Missing file is fine; missing required
    // vars below is not.
    let _ = dotenvy::dotenv();
    fmt::init();

    let signing_key = env::var("OVERACP_JWT_SIGNING_KEY")
        .map_err(|_| "OVERACP_JWT_SIGNING_KEY is required (set it in .env or the environment)")?;
    let issuer = env::var("OVERACP_JWT_ISSUER").unwrap_or_else(|_| "overacp".to_string());

    let state = AppState::new(
        Arc::new(InMemoryStore::new()),
        Arc::new(default_registry()),
        Arc::new(StaticJwtAuthenticator::new(signing_key, issuer)),
    );
    let app = overacp_server::router(state);

    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("overacp-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
