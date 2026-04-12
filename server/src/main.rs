use std::error::Error as StdError;

use overacp_server::build_state_from_env;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn StdError>> {
    // Load .env if present. Missing file is fine; missing required
    // vars below is not.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("overacp_server=info".parse().unwrap()),
        )
        .init();

    let state = build_state_from_env()?;
    let app = overacp_server::router(state);

    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("overacp-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
