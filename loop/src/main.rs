use anyhow::Result;
use std::env;
use std::io;
use std::time::Duration;
use tokio::runtime::Builder;
use tracing::{error, info, warn, Instrument};
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use overloop::acp::AcpClient;
use overloop::agentic_loop::{self, LoopConfig};
use overloop::config::Config;
use overloop::llm;
use overloop::tools::{parse_acp_tools, ToolRegistry};
use overloop::traits::{AcpService, NextPush};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

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
            "overloop: SENTRY_DSN is set but this build was compiled without \
             the `sentry` feature; Sentry is disabled. Rebuild with \
             `--features sentry` to enable."
        );
    }

    #[cfg(feature = "sentry")]
    let _sentry_guard = init_sentry(&config);

    init_tracing()?;

    let root = tracing::info_span!(
        "overloop",
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
    let env_filter = EnvFilter::from_default_env().add_directive("overloop=info".parse()?);
    let fmt_layer = fmt::layer().with_writer(io::stderr);

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer);

    #[cfg(feature = "sentry")]
    let registry = registry.with(sentry_tracing::layer());

    registry.init();
    Ok(())
}

#[cfg(feature = "sentry")]
fn init_sentry(config: &Config) -> Option<sentry::ClientInitGuard> {
    use sentry::types::Dsn;

    let dsn_str = config.sentry_dsn.as_deref()?;
    // Parse up-front so a malformed DSN is surfaced rather than silently
    // dropped by `sentry::init` (which would leave the user wondering why
    // Sentry is quiet). tracing is not yet initialized here, so use stderr
    // directly.
    let dsn: Dsn = match dsn_str.parse() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("overloop: invalid SENTRY_DSN ({e}); Sentry disabled");
            return None;
        }
    };
    let guard = sentry::init(sentry::ClientOptions {
        dsn: Some(dsn),
        release: sentry::release_name!(),
        traces_sample_rate: config.sentry_traces_sample_rate,
        environment: Some(config.sentry_environment.clone().into()),
        ..Default::default()
    });
    if let Some(name) = config.agent_name.as_ref() {
        sentry::configure_scope(|scope| {
            scope.set_tag("agent_name", name);
        });
    }
    guard.is_enabled().then_some(guard)
}

async fn run(config: Config) -> Result<()> {
    info!(
        "Overloop starting — model={}, workspace={}",
        config.model, config.workspace
    );

    let mut acp = AcpClient::stdio();
    let llm = llm::LlmClient::new(&config.llm_api_url, &config.llm_api_key, &config.model);
    let mut registry = ToolRegistry::new();

    // Connect to MCP servers
    for url in &config.mcp_servers {
        info!("Connecting to MCP server: {}", url);
        if let Err(e) = registry.connect_mcp(url).await {
            error!("Failed to connect to MCP server {}: {}", url, e);
        }
    }

    // Fetch operator-provided tools via ACP `tools/list`.
    match acp.tools_list() {
        Ok(tools_value) => {
            let tools = parse_acp_tools(&tools_value);
            if !tools.is_empty() {
                info!("Discovered {} ACP tool(s)", tools.len());
                registry.set_acp_tools(tools);
            }
        }
        Err(e) => {
            warn!("Failed to fetch ACP tools: {}", e);
        }
    }

    // Set workspace directory
    if config.workspace != "." {
        env::set_current_dir(&config.workspace)?;
    }

    // Single cold-start `initialize` — the broker delegates to
    // BootProvider to return {system_prompt, messages, tools_config}.
    // The agent holds this history in memory for the lifetime of
    // the process and does NOT re-initialize per turn.
    info!("Initializing conversation...");
    let init = acp.initialize()?;
    let mut messages = init.messages;
    llm::resolve_file_urls(&mut messages);

    // Prepend system prompt if not already present.
    if messages.first().map(|m| &m.role) != Some(&llm::Role::System) {
        messages.insert(
            0,
            llm::Message {
                role: llm::Role::System,
                content: Some(llm::Content::Text(init.system_prompt)),
                tool_calls: None,
                tool_call_id: None,
            },
        );
    }

    let loop_config = LoopConfig {
        max_iterations: config.max_iterations,
        timeout: Duration::from_secs(config.timeout_minutes * 60),
    };

    // Outer turn loop: block until the next user message arrives
    // inline in a `session/message` notification, then run a turn.
    // `session/cancel` exits the loop cleanly.
    info!("Waiting for session/message...");
    loop {
        match acp.next_push()? {
            NextPush::Message(mut user_msg) => {
                info!("Received user message, starting turn");
                llm::resolve_file_urls_in_message(&mut user_msg);
                messages.push(user_msg);
                if let Err(e) =
                    agentic_loop::run(&mut acp, &llm, &mut registry, &mut messages, &loop_config)
                        .await
                {
                    error!("Agentic loop error: {}", e);
                }
            }
            NextPush::Cancel => {
                info!("Session cancelled — exiting");
                break;
            }
        }
    }

    Ok(())
}
