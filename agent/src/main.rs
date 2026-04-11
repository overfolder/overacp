//! Binary entry point for `overacp-agent`.

use anyhow::Result;
use rustls::crypto::ring;
use std::io::stderr;
use tokio::runtime::Builder;
use tracing_subscriber::EnvFilter;

use overacp_agent::config::Config;
use overacp_agent::run::run;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("overacp_agent=info".parse()?))
        .with_writer(stderr)
        .init();

    // Install rustls default crypto provider — required by
    // tokio-tungstenite when multiple providers are linked indirectly.
    let _ = ring::default_provider().install_default();

    let _ = dotenvy::dotenv();
    let config = Config::from_env()?;

    Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(config))
}
