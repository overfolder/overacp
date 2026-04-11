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

    // 2. Decode the agent_id from the token's `sub` claim (no
    //    signature check; the broker validates the token
    //    authoritatively on the WebSocket upgrade). For agent tokens
    //    the `sub` claim is the agent_id, which is also the routing
    //    key on the broker's `/tunnel/<agent_id>` endpoint.
    let claims = peek_claims_unverified(&config.token)?;
    let agent_id = claims.sub.to_string();
    let tunnel_url = config.tunnel_url(&agent_id);

    // 3. Open the WebSocket tunnel with retry/backoff.
    let (ws_read, ws_sink) = connect_with_retry(&tunnel_url, &config.token).await;

    // 4. Spawn the child agent.
    let mut proc = spawn(adapter.command())?;

    info!("bridge starting: child stdio ↔ WebSocket tunnel");

    // 5. Pump until either side closes.
    let exit = run_bridge(ws_read, ws_sink, proc.stdin, proc.stdout).await;
    info!("bridge exited: {exit:?}");

    // 6. Reap the child. tokio::process::Child does NOT kill or wait
    //    on drop, so we must do it explicitly to avoid orphans/zombies.
    //    Try a graceful wait first; if the bridge exited because the
    //    tunnel closed (not the child), kill the child too.
    match proc.child.try_wait() {
        Ok(Some(status)) => info!("child exited: {status}"),
        Ok(None) => {
            info!("child still running after bridge exit; killing");
            if let Err(e) = proc.child.kill().await {
                tracing::warn!("failed to kill child: {e}");
            }
            let _ = proc.child.wait().await;
        }
        Err(e) => tracing::warn!("try_wait failed: {e}"),
    }

    // 7. Push the workspace back.
    sync.push().await?;

    Ok(())
}
