use std::error::Error as StdError;
use std::sync::Arc;

use axum::{routing::get, Router};
use overacp_server::{AppState, InMemoryStore};
use tokio::net::TcpListener;
use tracing_subscriber::fmt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn StdError>> {
    fmt::init();

    let state = AppState::new(Arc::new(InMemoryStore::new()));
    let app = Router::new()
        .route("/healthz", get(healthz))
        .with_state(state);

    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("overacp-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}
