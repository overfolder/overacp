//! Hermetic end-to-end test for the full supervisor + loop pipeline.
//!
//! Spins up:
//! 1. `overacp-server` in-process on a random port (default hooks).
//! 2. `wiremock` mock LLM on a random port returning a canned SSE
//!    response with nonzero usage.
//! 3. `overacp-agent` spawned as a real subprocess with env vars
//!    pointing at both servers, hosting a real `overloop` child
//!    on stdio.
//!
//! Then drives the pipeline via REST/SSE and asserts that a
//! `turn/end` notification fans out with the expected payload.
//!
//! Prerequisite: `cargo build --workspace` must have run. The
//! `ensure_workspace_built()` helper re-runs `cargo build` at the
//! start of each test as a belt-and-braces measure — it's a no-op
//! when everything is already up to date.

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::str;
use std::sync::{Arc, Once};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use common::{
    cargo_debug_bin, mint_admin, mint_agent, push_body, spawn_broker, spawn_broker_with_tool_host,
    spawn_mock_llm, spawn_mock_llm_with_tool_call,
};
use overacp_server::auth::Authenticator;

/// RAII guard that ensures a spawned child process is killed if
/// the test panics before reaching the explicit cleanup. Without
/// this, any assertion failure would orphan the supervisor + its
/// overloop grandchild — `tokio::process::Child` does NOT kill on
/// drop, it only closes its handle.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // `start_kill` is the synchronous signal-send method — safe
        // to call from a sync Drop without needing the runtime.
        let _ = self.0.start_kill();
    }
}

static BUILD_ONCE: Once = Once::new();

/// Run `cargo build --workspace` once per test process. Idempotent;
/// a no-op if nothing has changed since the last build.
fn ensure_workspace_built() {
    BUILD_ONCE.call_once(|| {
        let status = StdCommand::new(env!("CARGO"))
            .args(["build", "--workspace"])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect("failed to run `cargo build --workspace`");
        assert!(status.success(), "`cargo build --workspace` failed");
    });
}

// ── Shared pipeline helpers ────────────────────────────────────────

/// Outcome of a full-stack pipeline run: the `turn/end` SSE frame
/// plus the KillOnDrop guard (kept alive until assertions finish).
struct PipelineRun {
    frame: serde_json::Value,
    _supervisor: KillOnDrop,
}

/// Launch the full pipeline (SSE subscriber → supervisor → wait for
/// connected → push message → wait for turn/end) and return the
/// `turn/end` frame for assertion.
async fn run_pipeline(
    base_url: &str,
    auth: &dyn Authenticator,
    llm_url: &str,
    workspace: &Path,
    user_message: &str,
) -> PipelineRun {
    let admin_jwt = mint_admin(auth);
    let agent_id = Uuid::new_v4();
    let agent_jwt = mint_agent(auth, agent_id);

    let client = Client::builder().no_proxy().build().unwrap();

    // SSE subscriber — must start BEFORE the message push.
    let sse_resp = client
        .get(format!("{base_url}/agents/{agent_id}/stream"))
        .bearer_auth(&admin_jwt)
        .send()
        .await
        .unwrap();
    assert!(sse_resp.status().is_success());
    let mut sse_stream = sse_resp.bytes_stream();

    // Launch supervisor.
    let agent_bin = cargo_debug_bin("overacp-agent");
    let overloop_bin = cargo_debug_bin("overloop");
    let supervisor = Command::new(&agent_bin)
        .env("OVERACP_TOKEN", &agent_jwt)
        .env("OVERACP_SERVER_URL", base_url)
        .env("OVERACP_WORKSPACE", workspace)
        .env("OVERACP_AGENT_BINARY", &overloop_bin)
        .env("LLM_API_KEY", "mock-key")
        .env("LLM_API_URL", llm_url)
        .env("OVERFOLDER_MODEL", "mock-model")
        .env("RUST_LOG", "error")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn overacp-agent");
    let supervisor = KillOnDrop(supervisor);

    // Wait for tunnel.
    let connected = wait_for_connected(&client, base_url, agent_id, &admin_jwt).await;
    assert!(connected, "tunnel never came up within 10s");

    // Push user message.
    let push_resp = client
        .post(format!("{base_url}/agents/{agent_id}/messages"))
        .bearer_auth(&admin_jwt)
        .json(&push_body(user_message))
        .send()
        .await
        .unwrap();
    assert!(push_resp.status().is_success());

    // Wait for turn/end.
    let frame = match timeout(Duration::from_secs(30), find_turn_end(&mut sse_stream)).await {
        Ok(Ok(Some(v))) => v,
        Ok(Ok(None)) => panic!("SSE closed without turn/end"),
        Ok(Err(e)) => panic!("SSE error: {e}"),
        Err(_) => panic!("timeout waiting for turn/end after 30s"),
    };

    assert_eq!(frame["method"], "turn/end");
    PipelineRun {
        frame,
        _supervisor: supervisor,
    }
}

/// Check that `messages` contains a message with the given role
/// whose content includes `needle`.
fn assert_message_contains(messages: &[serde_json::Value], role: &str, needle: &str, label: &str) {
    let found = messages.iter().any(|m| {
        m["role"] == role
            && m["content"]
                .as_str()
                .map(|s| s.contains(needle))
                .unwrap_or(false)
    });
    assert!(found, "{label}, got: {messages:?}");
}

/// Poll `GET /agents/{id}` until the tunnel is connected. Gives up
/// after ~10 seconds and returns false.
async fn wait_for_connected(
    client: &Client,
    base_url: &str,
    agent_id: Uuid,
    admin_jwt: &str,
) -> bool {
    for _ in 0..50 {
        let resp = client
            .get(format!("{base_url}/agents/{agent_id}"))
            .bearer_auth(admin_jwt)
            .send()
            .await;
        if let Ok(r) = resp {
            if r.status().is_success() {
                if let Ok(body) = r.json::<serde_json::Value>().await {
                    if body["connected"].as_bool().unwrap_or(false) {
                        return true;
                    }
                }
            }
        }
        sleep(Duration::from_millis(200)).await;
    }
    false
}

/// Drain an SSE byte-stream and return the first parsed JSON frame
/// whose `method` field is `"turn/end"`.
async fn find_turn_end<S>(stream: &mut S) -> Result<Option<serde_json::Value>, reqwest::Error>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk: Bytes = chunk?;
        match str::from_utf8(&chunk) {
            Ok(s) => buf.push_str(s),
            Err(e) => {
                eprintln!("find_turn_end: skipping non-UTF-8 chunk: {e}");
                continue;
            }
        }
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim_end().to_string();
            buf.drain(..=nl);
            if let Some(json_str) = line.strip_prefix("data: ") {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if value["method"] == "turn/end" {
                        return Ok(Some(value));
                    }
                }
            }
        }
    }
    Ok(None)
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn full_stack_turn_end_over_real_tunnel() {
    ensure_workspace_built();

    let (broker_addr, auth, _h) = spawn_broker().await;
    let base_url = format!("http://{broker_addr}");
    let llm = spawn_mock_llm("HELLO").await;
    let workspace = tempfile::tempdir().expect("mktemp workspace");

    let run = run_pipeline(
        &base_url,
        auth.as_ref(),
        &llm.uri(),
        workspace.path(),
        "Reply with HELLO.",
    )
    .await;

    let messages = run.frame["params"]["messages"].as_array().unwrap();
    let input_tokens = run.frame["params"]["usage"]["input_tokens"]
        .as_u64()
        .unwrap_or(0);
    let output_tokens = run.frame["params"]["usage"]["output_tokens"]
        .as_u64()
        .unwrap_or(0);
    assert!(input_tokens > 0, "input_tokens not reported");
    assert!(output_tokens > 0, "output_tokens not reported");
    assert_message_contains(messages, "assistant", "HELLO", "expected HELLO");
}

// ── Operator-provided tool e2e ──────────────────────────────────────

#[tokio::test]
async fn full_stack_operator_tool_round_trip() {
    use async_trait::async_trait;
    use overacp_server::{ToolError, ToolHost};

    struct WeatherToolHost;

    #[async_trait]
    impl ToolHost for WeatherToolHost {
        async fn list(
            &self,
            _claims: &overacp_server::Claims,
        ) -> Result<serde_json::Value, ToolError> {
            Ok(serde_json::json!({
                "tools": [{
                    "name": "get_weather",
                    "description": "Get weather for a city",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"]
                    }
                }]
            }))
        }

        async fn call(
            &self,
            _claims: &overacp_server::Claims,
            req: serde_json::Value,
        ) -> Result<serde_json::Value, ToolError> {
            let city = req["arguments"]["city"].as_str().unwrap_or("unknown");
            Ok(serde_json::json!({
                "content": [{"type": "text", "text": format!("Weather in {city}: sunny, 22C")}],
                "isError": false
            }))
        }
    }

    ensure_workspace_built();

    let (broker_addr, auth, _h) = spawn_broker_with_tool_host(Arc::new(WeatherToolHost)).await;
    let base_url = format!("http://{broker_addr}");
    let llm =
        spawn_mock_llm_with_tool_call("get_weather", r#"{\"city\":\"London\"}"#, "WEATHER_RESULT")
            .await;
    let workspace = tempfile::tempdir().expect("mktemp workspace");

    let run = run_pipeline(
        &base_url,
        auth.as_ref(),
        &llm.uri(),
        workspace.path(),
        "What is the weather in London?",
    )
    .await;

    let messages = run.frame["params"]["messages"].as_array().unwrap();
    assert_message_contains(messages, "tool", "sunny", "expected weather tool result");
    assert_message_contains(
        messages,
        "assistant",
        "WEATHER_RESULT",
        "expected final text",
    );
}

// ── Built-in tool e2e ───────────────────────────────────────────────

#[tokio::test]
async fn full_stack_builtin_tool_round_trip() {
    ensure_workspace_built();

    let (broker_addr, auth, _h) = spawn_broker().await;
    let base_url = format!("http://{broker_addr}");

    let workspace = tempfile::tempdir().expect("mktemp workspace");
    let marker_path = workspace.path().join("marker.txt");
    fs::write(&marker_path, "CANARY_VALUE").unwrap();

    let path_str = marker_path.to_str().unwrap();
    let tool_args = format!(r#"{{\"path\":\"{path_str}\"}}"#);
    let llm = spawn_mock_llm_with_tool_call("read", &tool_args, "BUILTIN_OK").await;

    let run = run_pipeline(
        &base_url,
        auth.as_ref(),
        &llm.uri(),
        workspace.path(),
        "Read the marker file.",
    )
    .await;

    let messages = run.frame["params"]["messages"].as_array().unwrap();
    assert_message_contains(messages, "tool", "CANARY_VALUE", "expected marker content");
    assert_message_contains(messages, "assistant", "BUILTIN_OK", "expected final text");
}
