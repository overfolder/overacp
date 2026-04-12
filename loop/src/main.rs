use anyhow::Result;
use std::env;
use std::io;
use std::time::Duration;
use tokio::runtime::Builder;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use overloop::acp::AcpClient;
use overloop::agentic_loop::{self, LoopConfig};
use overloop::config::Config;
use overloop::llm;
use overloop::tools::{parse_acp_tools, ToolRegistry};
use overloop::traits::{AcpService, NextPush};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("overloop=info".parse()?))
        .with_writer(io::stderr)
        .init();

    let config = Config::from_env()?;
    info!(
        "Overloop starting — model={}, workspace={}",
        config.model, config.workspace
    );

    Builder::new_multi_thread()
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
