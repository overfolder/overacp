//! Binary entry point for `overacp-agent`.

use anyhow::Result;
use rustls::crypto::ring;
use std::io::stderr;
#[cfg(feature = "sentry")]
use std::time::Duration;
use tokio::runtime::Builder;
use tracing::Instrument;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use overacp_agent::config::Config;
use overacp_agent::run::run;

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    // Load config before tracing so Sentry can see its env vars, and so
    // sentry::init runs before the tracing subscriber is installed (the
    // sentry-tracing layer routes events through the global Hub, which
    // must have a client by then).
    let config = Config::from_env()?;

    // Loud, early warning if the operator set SENTRY_DSN but this build
    // wasn't compiled with the `sentry` feature — avoids the silent-miss
    // footgun where errors quietly never reach Sentry.
    #[cfg(not(feature = "sentry"))]
    if config.sentry_dsn.is_some() {
        eprintln!(
            "overacp-agent: SENTRY_DSN is set but this build was compiled \
             without the `sentry` feature; Sentry is disabled. Rebuild \
             with `--features sentry` to enable."
        );
    }

    #[cfg(feature = "sentry")]
    let _sentry_guard = init_sentry(&config);

    init_tracing()?;

    // Install rustls default crypto provider — required by
    // tokio-tungstenite when multiple providers are linked indirectly.
    let _ = ring::default_provider().install_default();

    let root = tracing::info_span!(
        "overacp_agent",
        agent_name = config.agent_name.as_deref().unwrap_or("")
    );

    let result = Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(config).instrument(root));

    #[cfg(feature = "sentry")]
    if let Some(client) = sentry::Hub::current().client() {
        client.flush(Some(Duration::from_secs(2)));
    }

    result
}

/// Build the tracing subscriber. When the `sentry` feature is enabled the
/// sentry-tracing layer is attached so every `tracing::error!` becomes a
/// Sentry event (subject to runtime DSN presence).
fn init_tracing() -> Result<()> {
    let filter = EnvFilter::from_default_env().add_directive("overacp_agent=info".parse()?);
    let fmt_layer = fmt::layer().with_writer(stderr);

    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

    #[cfg(feature = "sentry")]
    let registry = registry.with(sentry_tracing::layer());

    registry.init();
    Ok(())
}

#[cfg(feature = "sentry")]
fn init_sentry(config: &Config) -> Option<sentry::ClientInitGuard> {
    use overacp_common::sentry_rate_limit;
    use sentry::types::Dsn;
    use std::sync::Arc;

    let dsn_str = config.sentry_dsn.as_deref()?;
    // Parse up-front so a malformed DSN is surfaced rather than silently
    // dropped by `sentry::init` (which would leave the user wondering why
    // Sentry is quiet). tracing is not yet initialized here, so use
    // stderr directly.
    let dsn: Dsn = match dsn_str.parse() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("overacp-agent: invalid SENTRY_DSN ({e}); Sentry disabled");
            return None;
        }
    };
    let guard = sentry::init(sentry::ClientOptions {
        dsn: Some(dsn),
        release: sentry::release_name!(),
        traces_sample_rate: config.sentry_traces_sample_rate,
        environment: Some(config.sentry_environment.clone().into()),
        server_name: config.agent_name.clone().map(Into::into),
        before_send: Some(Arc::new(sentry_rate_limit::before_send)),
        ..Default::default()
    });
    if let Some(name) = config.agent_name.as_ref() {
        sentry::configure_scope(|scope| {
            scope.set_tag("agent_name", name);
        });
    }
    guard.is_enabled().then_some(guard)
}
