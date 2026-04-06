use anyhow::Result;
use std::time::Duration;
use tracing::{error, info};

use overloop::acp::AcpClient;
use overloop::config::Config;
use overloop::llm;
use overloop::tools::ToolRegistry;

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("overloop=info".parse()?),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = Config::from_env()?;
    info!(
        "Overloop starting — model={}, workspace={}",
        config.model, config.workspace
    );

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(config))
}

async fn run(config: Config) -> Result<()> {
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

    // Set workspace directory
    if config.workspace != "." {
        std::env::set_current_dir(&config.workspace)?;
    }

    // Wait for session/message notification
    info!("Waiting for session/message...");
    loop {
        let notification = acp.recv_notification()?;

        match notification.method.as_str() {
            "session/message" => {
                info!("Received session/message");

                // Initialize to get system prompt + history
                let init = acp.initialize()?;
                let mut messages = init.messages;

                // Prepend system prompt if not already present
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

                let loop_config = overloop::agentic_loop::LoopConfig {
                    max_iterations: config.max_iterations,
                    timeout: Duration::from_secs(config.timeout_minutes * 60),
                };

                if let Err(e) = overloop::agentic_loop::run(
                    &mut acp,
                    &llm,
                    &mut registry,
                    &mut messages,
                    &loop_config,
                )
                .await
                {
                    error!("Agentic loop error: {}", e);
                }
            }
            "session/cancel" => {
                info!("Session cancelled");
                break;
            }
            other => {
                info!("Ignoring unknown notification: {}", other);
            }
        }
    }

    Ok(())
}
