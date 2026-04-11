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

use std::process::{Command as StdCommand, Stdio};
use std::str;
use std::sync::Once;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

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

impl KillOnDrop {
    async fn kill_and_wait(mut self) {
        let _ = self.0.kill().await;
        let _ = self.0.wait().await;
    }
}

use common::{cargo_debug_bin, mint_admin, mint_agent, push_body, spawn_broker, spawn_mock_llm};

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

#[tokio::test]
async fn full_stack_turn_end_over_real_tunnel() {
    ensure_workspace_built();

    // 1. Broker in-process.
    let (broker_addr, auth, _broker_handle) = spawn_broker().await;
    let base_url = format!("http://{broker_addr}");

    // 2. Mock LLM.
    let llm = spawn_mock_llm("HELLO").await;
    let llm_url = llm.uri();

    // 3. Mint tokens.
    let admin_jwt = mint_admin(auth.as_ref());
    let agent_id = Uuid::new_v4();
    let agent_jwt = mint_agent(auth.as_ref(), agent_id);

    // 4. Locate binaries + set up an ephemeral workspace.
    let agent_bin = cargo_debug_bin("overacp-agent");
    let overloop_bin = cargo_debug_bin("overloop");
    let workspace = tempfile::tempdir().expect("mktemp workspace");

    // 5. Start the SSE subscriber BEFORE pushing the message so we
    //    don't race the turn/end fan-out.
    let client = Client::builder()
        .no_proxy()
        .build()
        .expect("reqwest client");
    let sse_resp = client
        .get(format!("{base_url}/agents/{agent_id}/stream"))
        .bearer_auth(&admin_jwt)
        .send()
        .await
        .expect("subscribe SSE");
    assert!(
        sse_resp.status().is_success(),
        "SSE subscribe failed: {}",
        sse_resp.status()
    );
    let mut sse_stream = sse_resp.bytes_stream();

    // 6. Launch overacp-agent as a real subprocess, wrapped in a
    //    KillOnDrop guard so a panicking assertion still cleans up.
    let supervisor = Command::new(&agent_bin)
        .env("OVERACP_TOKEN", &agent_jwt)
        .env("OVERACP_SERVER_URL", &base_url)
        .env("OVERACP_WORKSPACE", workspace.path())
        .env("OVERACP_AGENT_BINARY", &overloop_bin)
        .env("LLM_API_KEY", "mock-key")
        .env("LLM_API_URL", &llm_url)
        .env("OVERFOLDER_MODEL", "mock-model")
        .env("RUST_LOG", "error")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn overacp-agent");
    let supervisor = KillOnDrop(supervisor);

    // 7. Poll GET /agents/{id} until the tunnel is connected.
    let connected = wait_for_connected(&client, &base_url, agent_id, &admin_jwt).await;
    assert!(connected, "tunnel never came up within 10s");

    // 8. Push a user message.
    let push_resp = client
        .post(format!("{base_url}/agents/{agent_id}/messages"))
        .bearer_auth(&admin_jwt)
        .json(&push_body("Reply with HELLO."))
        .send()
        .await
        .expect("POST /messages");
    assert!(
        push_resp.status().is_success(),
        "push failed: {}",
        push_resp.status()
    );

    // 9. Drain the SSE stream until a turn/end frame arrives.
    let frame = match timeout(Duration::from_secs(30), find_turn_end(&mut sse_stream)).await {
        Ok(Ok(Some(v))) => v,
        Ok(Ok(None)) => panic!("SSE stream closed without turn/end"),
        Ok(Err(e)) => panic!("error draining SSE stream: {e}"),
        Err(_) => panic!("timeout waiting for turn/end after 30s"),
    };

    // 10. Assert the turn/end frame carries nonzero usage + the
    //     assistant's HELLO message.
    assert_eq!(frame["method"], "turn/end");
    let input_tokens = frame["params"]["usage"]["input_tokens"]
        .as_u64()
        .unwrap_or(0);
    let output_tokens = frame["params"]["usage"]["output_tokens"]
        .as_u64()
        .unwrap_or(0);
    assert!(input_tokens > 0, "input_tokens not reported: {frame}");
    assert!(output_tokens > 0, "output_tokens not reported: {frame}");

    let messages = frame["params"]["messages"].as_array().unwrap();
    let any_assistant_hello = messages.iter().any(|m| {
        m["role"] == "assistant"
            && m["content"]
                .as_str()
                .map(|s| s.contains("HELLO"))
                .unwrap_or(false)
    });
    assert!(
        any_assistant_hello,
        "expected an assistant message containing HELLO, got: {messages:?}"
    );

    // Cleanup: kill the supervisor cleanly. Panics before this point
    // still trigger `KillOnDrop::drop` above.
    supervisor.kill_and_wait().await;
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
/// whose `method` field is `"turn/end"`. The SSE protocol prefixes
/// each frame with `data: `; we concatenate chunks and match on
/// line boundaries.
///
/// Transport errors (reqwest) are surfaced to the caller rather
/// than silently turned into `None` — otherwise a broken SSE
/// connection looks identical to a graceful close and the test
/// panic message becomes useless. UTF-8 decoding errors on a single
/// chunk are logged via `eprintln!` and skipped, since the SSE
/// body is always text/plain.
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
