use anyhow::Result;
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use super::ToolResult;

pub async fn tool_read(args: Value) -> ToolResult {
    let path = arg_str(&args, "path")?;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read {}: {}", path, e))?;

    let lines: Vec<&str> = content.lines().collect();
    let start = offset.min(lines.len());
    let end = (start + limit).min(lines.len());

    let numbered: String = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}\t{}", start + i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(numbered)
}

pub async fn tool_write(args: Value) -> ToolResult {
    let path = arg_str(&args, "path")?;
    let content = arg_str(&args, "content")?;

    if let Some(parent) = Path::new(&path).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }

    tokio::fs::write(&path, &content)
        .await
        .map_err(|e| format!("write {}: {}", path, e))?;

    Ok(format!("Wrote {} bytes to {}", content.len(), path))
}

pub async fn tool_exec(args: Value) -> ToolResult {
    let command = arg_str(&args, "command")?;
    let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new("bash")
            .arg("-c")
            .arg(&command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let code = output.status.code().unwrap_or(-1);

            let mut out = String::new();
            if !stdout.is_empty() {
                out.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[stderr]\n");
                out.push_str(&stderr);
            }
            out.push_str(&format!("\n[exit code: {}]", code));
            Ok(out)
        }
        Ok(Err(e)) => Err(format!("exec error: {}", e)),
        Err(_) => Err(format!("command timed out after {}s", timeout_secs)),
    }
}

pub async fn tool_glob(args: Value) -> ToolResult {
    let pattern = arg_str(&args, "pattern")?;
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let pattern = pattern.clone();
    let path = path.to_string();

    let result = tokio::task::spawn_blocking(move || {
        let matcher =
            glob::Pattern::new(&pattern).map_err(|e| format!("invalid pattern: {}", e))?;

        let mut matches = Vec::new();
        for entry in walkdir::WalkDir::new(&path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let entry_path = entry.path();
            if let Some(name) = entry_path.to_str() {
                if matcher.matches(name)
                    || entry_path
                        .file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| matcher.matches(f))
                {
                    matches.push(name.to_string());
                }
            }
        }

        matches.sort();
        if matches.is_empty() {
            Ok("No matches found.".to_string())
        } else {
            Ok(matches.join("\n"))
        }
    })
    .await
    .map_err(|e| format!("glob task error: {}", e))?;

    result
}

pub async fn tool_grep(args: Value) -> ToolResult {
    let pattern = arg_str(&args, "pattern")?;
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let output = Command::new("grep")
        .arg("-rn")
        .arg("--include=*")
        .arg(&pattern)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("grep error: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.is_empty() {
        Ok("No matches found.".to_string())
    } else {
        // Limit output to avoid flooding context
        let lines: Vec<&str> = stdout.lines().take(200).collect();
        Ok(lines.join("\n"))
    }
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing required argument: {}", key))
}
