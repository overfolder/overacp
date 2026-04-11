//! Top-level orchestration: load config → connect tunnel → spawn
//! child via the chosen `AgentAdapter` → run the bridge.

use anyhow::Result;
use overacp_protocol::jwt::peek_claims_unverified;
use tokio::process::Child;
use tracing::{info, warn};

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
    //    on drop, so we must do it explicitly to avoid orphans /
    //    zombies. Delegated to `reap_child` so the branches are
    //    unit-testable against fake child processes.
    reap_child(&mut proc.child).await;

    // 7. Push the workspace back.
    sync.push().await?;

    Ok(())
}

/// Reason `reap_child` returned, for tests and tracing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReapOutcome {
    /// Child had already exited by the time we checked.
    AlreadyExited,
    /// Child was still running and we killed it.
    Killed,
    /// `try_wait` itself errored — child state is unknown.
    TryWaitErrored,
}

/// Try a graceful wait on the child, killing it if it's still
/// running. Returns which branch was taken so callers (and tests)
/// can distinguish them. Errors from `kill`/`wait` are logged but
/// not surfaced — we always want the supervisor to proceed to the
/// workspace push even on a reap failure.
pub(crate) async fn reap_child(child: &mut Child) -> ReapOutcome {
    match child.try_wait() {
        Ok(Some(status)) => {
            info!("child exited: {status}");
            ReapOutcome::AlreadyExited
        }
        Ok(None) => {
            info!("child still running after bridge exit; killing");
            if let Err(e) = child.kill().await {
                warn!("failed to kill child: {e}");
            }
            let _ = child.wait().await;
            ReapOutcome::Killed
        }
        Err(e) => {
            warn!("try_wait failed: {e}");
            ReapOutcome::TryWaitErrored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::process::Command;
    use tokio::time::sleep;

    #[tokio::test]
    async fn reap_child_already_exited() {
        // A short-lived child that will have already exited by the
        // time we call reap_child — exercises the Ok(Some(status))
        // branch.
        let mut child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
        // Poll try_wait until the shell exits (usually ≤ 50 ms).
        for _ in 0..50 {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(reap_child(&mut child).await, ReapOutcome::AlreadyExited);
    }

    #[tokio::test]
    async fn reap_child_kills_long_running_process() {
        // A child that blocks forever — reap_child should kill it
        // and take the `Killed` branch.
        let mut child = Command::new("sleep").arg("3600").spawn().unwrap();
        let outcome = reap_child(&mut child).await;
        assert_eq!(outcome, ReapOutcome::Killed);
        // And the child is no longer running.
        assert!(child.try_wait().unwrap().is_some() || child.id().is_none());
    }
}
