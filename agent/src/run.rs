//! Top-level orchestration: load config → connect tunnel → spawn
//! child via the chosen `AgentAdapter` → run the bridge.

use anyhow::Result;
use overacp_protocol::jwt::peek_claims_unverified;
use tracing::info;

use crate::adapter::{AgentAdapter, LoopAdapter};
use crate::bridge::run as run_bridge;
use crate::config::Config;
use crate::process::spawn;
use crate::tunnel::connect_with_retry;
use crate::workspace::{NoopSync, WorkspaceSync};

/// Run the supervisor end-to-end with the default `LoopAdapter` and
/// `NoopSync`. Returns when the bridge exits.
pub async fn run(config: Config) -> Result<()> {
    let adapter = LoopAdapter {
        binary: config.agent_binary.clone().into(),
        workspace: config.workspace.clone().into(),
    };
    let sync = NoopSync;
    run_with(config, adapter, sync).await
}

/// Run the supervisor with caller-provided adapter and sync impls.
pub async fn run_with<A: AgentAdapter, S: WorkspaceSync>(
    config: Config,
    adapter: A,
    sync: S,
) -> Result<()> {
    info!(
        server = %config.server_url,
        workspace = %config.workspace,
        "overacp-agent starting"
    );

    // 1. Pull the workspace before the agent starts.
    sync.pull().await?;

    // 2. Decode the conversation ID from the token (no signature check;
    //    the server validates the token authoritatively on the way in).
    let claims = peek_claims_unverified(&config.token)?;
    let session_id = claims.conv.to_string();
    let tunnel_url = config.tunnel_url(&session_id);

    // 3. Open the WebSocket tunnel with retry/backoff.
    let (ws_read, ws_sink) = connect_with_retry(&tunnel_url, &config.token).await;

    // 4. Spawn the child agent.
    let proc = spawn(adapter.command())?;

    info!("bridge starting: child stdio ↔ WebSocket tunnel");

    // 5. Pump until either side closes.
    let exit = run_bridge(ws_read, ws_sink, proc.stdin, proc.stdout).await;
    info!("bridge exited: {exit:?}");

    // 6. Push the workspace back.
    sync.push().await?;

    Ok(())
}
