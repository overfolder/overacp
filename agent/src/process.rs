//! Child agent subprocess management. The spawned binary is
//! whatever the active `AgentAdapter` returns; the supervisor
//! pipes stdin/stdout and inherits stderr so the child's logs
//! land in the supervisor's stderr.

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

    #[tokio::test]
    async fn write_line_adds_newline_when_missing() {
        // `cat` echoes back unchanged. If write_line didn't append
        // the newline, read_line would block forever. Round-tripping
        // through the child proves the newline was appended.
        let cmd = Command::new("cat");
        let mut proc = spawn(cmd).unwrap();
        write_line(&mut proc.stdin, "no-newline").await.unwrap();
        let line = read_line(&mut proc.stdout).await.unwrap();
        assert_eq!(line, Some("no-newline\n".to_string()));
        drop(proc.stdin);
        let _ = proc.child.wait().await;
    }

    #[tokio::test]
    async fn write_line_does_not_double_newline() {
        let cmd = Command::new("cat");
        let mut proc = spawn(cmd).unwrap();
        write_line(&mut proc.stdin, "with-newline\n").await.unwrap();
        // First read_line returns the line. The next read_line should
        // hit EOF immediately after the child is closed; if we had
        // double-written we'd see an extra blank line first.
        let line = read_line(&mut proc.stdout).await.unwrap();
        assert_eq!(line, Some("with-newline\n".to_string()));
        drop(proc.stdin);
        let eof = read_line(&mut proc.stdout).await.unwrap();
        assert_eq!(eof, None);
        let _ = proc.child.wait().await;
    }

    #[tokio::test]
    async fn read_line_returns_none_on_eof() {
        // A child that exits immediately (`true`) closes its stdout
        // without writing anything; read_line must surface that as
        // Ok(None), not hang or error.
        let cmd = Command::new("true");
        let mut proc = spawn(cmd).unwrap();
        let line = read_line(&mut proc.stdout).await.unwrap();
        assert_eq!(line, None);
        let status = proc.child.wait().await.unwrap();
        assert!(status.success());
    }

    #[test]
    fn spawn_errors_on_missing_binary() {
        let cmd = Command::new("/nonexistent/binary/definitely-not-here");
        let result = spawn(cmd);
        assert!(result.is_err(), "spawn of missing binary should fail");
    }
}
