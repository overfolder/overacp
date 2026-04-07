//! Child agent subprocess management. Lifted and genericized from
//! `overfolder/overlet/src/process.rs` — the spawned binary is
//! whatever the active `AgentAdapter` returns.

use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::info;

/// Handles for a spawned child agent process.
pub struct AgentProcess {
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub child: Child,
}

/// Spawn the child agent with stdin/stdout piped and stderr
/// inherited (so the child's logs land in the supervisor's stderr).
pub fn spawn(mut command: Command) -> Result<AgentProcess> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = command.spawn().context("failed to spawn agent process")?;

    let stdin = child.stdin.take().context("no stdin on child")?;
    let stdout = child.stdout.take().context("no stdout on child")?;

    info!("agent process spawned");

    Ok(AgentProcess {
        stdin,
        stdout: BufReader::new(stdout),
        child,
    })
}

/// Write a single newline-terminated line to the child's stdin.
pub async fn write_line(stdin: &mut ChildStdin, line: &str) -> Result<()> {
    stdin
        .write_all(line.as_bytes())
        .await
        .context("write to agent stdin")?;
    if !line.ends_with('\n') {
        stdin.write_all(b"\n").await.context("write newline")?;
    }
    stdin.flush().await.context("flush agent stdin")?;
    Ok(())
}

/// Read a single line from the child's stdout. Returns `Ok(None)` on
/// EOF (the child has exited).
pub async fn read_line(stdout: &mut BufReader<ChildStdout>) -> Result<Option<String>> {
    let mut buf = String::new();
    let n = stdout
        .read_line(&mut buf)
        .await
        .context("read from agent stdout")?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_echo_process() {
        // `cat` reads stdin, writes to stdout — minimal AgentAdapter stand-in.
        let cmd = Command::new("cat");
        let mut proc = spawn(cmd).unwrap();
        write_line(&mut proc.stdin, "hello\n").await.unwrap();

        let line = read_line(&mut proc.stdout).await.unwrap();
        assert_eq!(line, Some("hello\n".to_string()));

        drop(proc.stdin);
        let status = proc.child.wait().await.unwrap();
        assert!(status.success());
    }
}
