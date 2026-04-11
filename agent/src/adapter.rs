//! Pluggable adapter for the child agent process.
//!
//! `AgentAdapter` lets the supervisor host any binary that speaks
//! over/ACP on stdio — the reference `overloop`, or (in future
//! milestones) third-party harnesses such as Claude Code or Codex via
//! their existing ACP bridges.
//!
//! 0.3 ships only `LoopAdapter`. Other adapters will be added as
//! separate impls in later milestones; the trait exists now so the
//! supervisor never needs to special-case the child binary.

use std::ffi::OsString;
use std::path::PathBuf;
use tokio::process::Command;

/// A factory for the child agent's tokio `Command`.
///
/// Implementations are expected to be cheap and re-callable — the
/// supervisor may invoke `command()` once per spawn attempt.
pub trait AgentAdapter {
    /// Build the `Command` used to spawn the child agent. The
    /// supervisor will configure stdin/stdout/stderr itself, so impls
    /// should not pre-set those.
    fn command(&self) -> Command;
}

/// Adapter for the reference agent (`overloop`).
///
/// Spawns the configured binary with `OVERACP_WORKSPACE` set to the
/// session workspace path. The binary is expected to speak over/ACP
/// on stdin/stdout, with logs on stderr.
pub struct LoopAdapter {
    /// Path or basename of the `overloop` binary. If a basename, it
    /// is resolved against the supervisor's `PATH`.
    pub binary: PathBuf,
    /// Workspace directory exported to the child as
    /// `OVERACP_WORKSPACE`.
    pub workspace: OsString,
}

impl AgentAdapter for LoopAdapter {
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.env("OVERACP_WORKSPACE", &self.workspace);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_adapter_sets_workspace_env_var() {
        let adapter = LoopAdapter {
            binary: PathBuf::from("overloop"),
            workspace: OsString::from("/tmp/ws"),
        };
        let cmd = adapter.command();
        let std_cmd = cmd.as_std();

        // Program path should be the binary we asked for.
        assert_eq!(std_cmd.get_program(), "overloop");

        // OVERACP_WORKSPACE should be in the spawned env.
        let envs: Vec<_> = std_cmd.get_envs().collect();
        let workspace = envs
            .iter()
            .find(|(k, _)| *k == "OVERACP_WORKSPACE")
            .and_then(|(_, v)| *v)
            .expect("OVERACP_WORKSPACE should be set");
        assert_eq!(workspace, "/tmp/ws");
    }

    #[test]
    fn loop_adapter_command_is_reusable_across_calls() {
        // `AgentAdapter::command()` is documented as cheap +
        // re-callable. Two successive calls must produce equivalent
        // commands so the supervisor can retry a spawn without
        // rebuilding the adapter.
        let adapter = LoopAdapter {
            binary: PathBuf::from("/bin/cat"),
            workspace: OsString::from("/tmp/ws"),
        };
        let cmd_a = adapter.command();
        let cmd_b = adapter.command();
        assert_eq!(cmd_a.as_std().get_program(), cmd_b.as_std().get_program());
    }
}
