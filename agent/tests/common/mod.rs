//! Shared helpers for the agent integration tests.
//!
//! Spins up an in-process `overacp-server` broker on a random port,
//! a `wiremock` mock LLM on a random port, and locates the prebuilt
//! `overloop` / `overacp-agent` debug binaries.

#![allow(dead_code)] // helpers may be used by subsets of tests

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use overacp_server::auth::{Authenticator, Claims};
use overacp_server::{router, AppState, StaticJwtAuthenticator, ToolHost};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use uuid::Uuid;
use wiremock::matchers::{method as http_method, path as http_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const SIGNING_KEY: &str = "e2e-supervisor-loop-signing-key";
pub const ISSUER: &str = "overacp";

/// Bind the broker to `127.0.0.1:0` and return the bound address
/// plus the authenticator (so tests can mint tokens directly).
pub async fn spawn_broker() -> (SocketAddr, Arc<dyn Authenticator>, JoinHandle<()>) {
    let auth: Arc<dyn Authenticator> = Arc::new(StaticJwtAuthenticator::new(SIGNING_KEY, ISSUER));
    let state = AppState::new(auth.clone());
    let app = router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, auth, handle)
}

/// Mint an admin JWT scoped to a random operator UUID.
pub fn mint_admin(auth: &dyn Authenticator) -> String {
    auth.mint(&Claims::admin(Uuid::new_v4(), 3600, ISSUER))
        .expect("mint admin")
}

/// Mint an agent JWT for the given agent_id.
pub fn mint_agent(auth: &dyn Authenticator, agent_id: Uuid) -> String {
    auth.mint(&Claims::agent(agent_id, None, 3600, ISSUER))
        .expect("mint agent")
}

/// Start a `wiremock` mock LLM on a random port that answers every
/// `POST /chat/completions` with a single-word SSE stream whose
/// `usage` reports a nonzero input/output token count. Matches the
/// OpenAI chat-completions stream shape that `overloop::llm::LlmClient`
/// expects.
pub async fn spawn_mock_llm(reply_word: &str) -> MockServer {
    let server = MockServer::start().await;

    // Two delta frames (role + content) + a finish-reason frame with
    // a usage block, followed by `[DONE]`.
    //
    // `overloop` parses the SSE body line-by-line; the `usage` block
    // on the last delta lands in `StreamedResponse::usage` which the
    // agentic loop forwards to quota_update and turn/end.
    let sse_body = format!(
        "\
data: {{\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":\"{word}\"}}}}]}}\n\n\
data: {{\"choices\":[{{\"finish_reason\":\"stop\",\"delta\":{{}}}}],\"usage\":{{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10}}}}\n\n\
data: [DONE]\n\n",
        word = reply_word
    );

    Mock::given(http_method("POST"))
        .and(http_path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    server
}

/// Locate a debug binary built by cargo under `<repo>/target/debug/`.
/// Panics with a helpful message if the binary is missing — typically
/// the caller needs to run `cargo build --workspace` first.
pub fn cargo_debug_bin(name: &str) -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // agent crate sits at `<repo>/agent`; target/ is at `<repo>/target`.
    let candidate = manifest_dir.join("..").join("target/debug").join(name);
    if !candidate.exists() {
        panic!(
            "debug binary not found at {candidate:?}\n\
             run `cargo build --workspace` (or `cargo build -p {name}`) before running this test.",
            candidate = candidate,
            name = name,
        );
    }
    candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone())
}

/// Build a canned user-message body for `POST /agents/{id}/messages`.
pub fn push_body(content: &str) -> serde_json::Value {
    json!({"role": "user", "content": content})
}

/// Like `spawn_broker`, but wires a custom `ToolHost` into the
/// broker's `AppState` so operator-provided tools are available
/// via the `tools/list` / `tools/call` dispatch path.
pub async fn spawn_broker_with_tool_host(
    tool_host: Arc<dyn ToolHost>,
) -> (SocketAddr, Arc<dyn Authenticator>, JoinHandle<()>) {
    let auth: Arc<dyn Authenticator> = Arc::new(StaticJwtAuthenticator::new(SIGNING_KEY, ISSUER));
    let state = AppState::new(auth.clone()).with_tool_host(tool_host);
    let app = router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, auth, handle)
}

/// Start a mock LLM that returns a tool call on the first request,
/// then a text reply on the second. Uses wiremock priority to
/// sequence responses.
pub async fn spawn_mock_llm_with_tool_call(
    tool_name: &str,
    tool_args: &str,
    reply_word: &str,
) -> MockServer {
    let server = MockServer::start().await;

    // First request: return a tool_call (consumed once via up_to_n_times).
    let tool_sse = format!(
        "\
data: {{\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"tool_calls\":[{{\"index\":0,\"id\":\"call_e2e\",\"function\":{{\"name\":\"{tool_name}\",\"arguments\":\"\"}}}}]}}}}]}}\n\n\
data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"{tool_args}\"}}}}]}}}}]}}\n\n\
data: {{\"choices\":[{{\"finish_reason\":\"tool_calls\",\"delta\":{{}}}}],\"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}}}\n\n\
data: [DONE]\n\n",
    );

    Mock::given(http_method("POST"))
        .and(http_path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(tool_sse),
        )
        .up_to_n_times(1)
        .with_priority(1) // higher priority = matched first
        .mount(&server)
        .await;

    // Second request: return text (always-available fallback).
    let text_sse = format!(
        "\
data: {{\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":\"{reply_word}\"}}}}]}}\n\n\
data: {{\"choices\":[{{\"finish_reason\":\"stop\",\"delta\":{{}}}}],\"usage\":{{\"prompt_tokens\":20,\"completion_tokens\":3,\"total_tokens\":23}}}}\n\n\
data: [DONE]\n\n",
    );

    Mock::given(http_method("POST"))
        .and(http_path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(text_sse),
        )
        .mount(&server)
        .await;

    server
}
