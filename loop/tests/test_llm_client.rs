use overloop::llm::{Content, LlmClient, Message, Role, StopReason};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn user_msg(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Some(Content::Text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

#[tokio::test]
async fn test_complete_success() {
    let server = MockServer::start().await;

    let body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Hello there!"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .expect(1)
        .mount(&server)
        .await;

    let client = LlmClient::new(&server.uri(), "test-key", "test-model");
    let messages = vec![user_msg("Hi")];
    let response = client.complete(&messages).await.unwrap();

    assert_eq!(response.choices.len(), 1);
    let msg = response.choices[0].message.as_ref().unwrap();
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(
        msg.content.as_ref().unwrap().as_text().unwrap(),
        "Hello there!"
    );
    let usage = response.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 5);
}

#[tokio::test]
async fn test_stream_text() {
    let server = MockServer::start().await;

    let sse_body = "\
data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n\
data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n\
data: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = LlmClient::new(&server.uri(), "test-key", "test-model");
    let messages = vec![user_msg("Hi")];

    let mut collected = Vec::new();
    let result = client
        .stream_completion(&messages, &[], &mut |text| {
            collected.push(text.to_string());
        })
        .await
        .unwrap();

    assert_eq!(
        result.message.content.unwrap().as_text().unwrap(),
        "hello world"
    );
    assert_eq!(result.finish_reason, Some(StopReason::Stop));
    assert_eq!(collected, vec!["hello", " world"]);
}

#[tokio::test]
async fn test_stream_tool_calls() {
    let server = MockServer::start().await;

    let sse_body = "\
data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"exec\",\"arguments\":\"{\\\"co\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"mmand\\\":\\\"ls\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n\
data: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = LlmClient::new(&server.uri(), "test-key", "test-model");
    let messages = vec![user_msg("list files")];

    let result = client
        .stream_completion(&messages, &[], &mut |_| {})
        .await
        .unwrap();

    assert_eq!(result.finish_reason, Some(StopReason::ToolCalls));
    let tool_calls = result.message.tool_calls.unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].function.name, "exec");
    let args: serde_json::Value = serde_json::from_str(&tool_calls[0].function.arguments).unwrap();
    assert_eq!(args["command"], "ls");
}

/// A responder that cycles through a list of responses.
struct CyclingResponder {
    responses: Mutex<VecDeque<ResponseTemplate>>,
}

impl Respond for CyclingResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut queue = self.responses.lock().unwrap();
        queue
            .pop_front()
            .unwrap_or_else(|| ResponseTemplate::new(500))
    }
}

#[tokio::test]
async fn test_retry_on_429() {
    let server = MockServer::start().await;

    let success_body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "ok"
            },
            "finish_reason": "stop"
        }]
    });

    let responder = CyclingResponder {
        responses: Mutex::new(
            vec![
                ResponseTemplate::new(429).set_body_string("rate limited"),
                ResponseTemplate::new(200).set_body_json(&success_body),
            ]
            .into(),
        ),
    };

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let client = LlmClient::new(&server.uri(), "test-key", "test-model");
    let messages = vec![user_msg("Hi")];
    let response = client.complete(&messages).await.unwrap();

    let msg = response.choices[0].message.as_ref().unwrap();
    assert_eq!(msg.content.as_ref().unwrap().as_text().unwrap(), "ok");
}

#[tokio::test]
async fn test_retry_on_5xx() {
    let server = MockServer::start().await;

    let success_body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "ok"
            },
            "finish_reason": "stop"
        }]
    });

    let responder = CyclingResponder {
        responses: Mutex::new(
            vec![
                ResponseTemplate::new(502).set_body_string("bad gateway"),
                ResponseTemplate::new(200).set_body_json(&success_body),
            ]
            .into(),
        ),
    };

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let client = LlmClient::new(&server.uri(), "test-key", "test-model");
    let messages = vec![user_msg("Hi")];
    let response = client.complete(&messages).await.unwrap();

    let msg = response.choices[0].message.as_ref().unwrap();
    assert_eq!(msg.content.as_ref().unwrap().as_text().unwrap(), "ok");
}

#[tokio::test]
async fn test_stream_read_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Accept one connection, announce a chunked body, then close the
    // socket mid-chunk so the client's stream yields an Err on next poll.
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let resp = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Transfer-Encoding: chunked\r\n\
                        \r\n\
                        20\r\n\
                        data: {\"choices\":[{\"delta\":";
            let _ = sock.write_all(resp.as_bytes()).await;
            // Drop socket without finishing the chunk or sending a terminator.
        }
    });

    let url = format!("http://{addr}");
    let client = LlmClient::new(&url, "test-key", "test-model");
    let messages = vec![user_msg("Hi")];
    let result = client.stream_completion(&messages, &[], &mut |_| {}).await;
    assert!(result.is_err(), "expected stream read error");
}

#[tokio::test]
async fn test_no_retry_on_400() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
        .expect(1)
        .mount(&server)
        .await;

    let client = LlmClient::new(&server.uri(), "test-key", "test-model");
    let messages = vec![user_msg("Hi")];
    let result = client.complete(&messages).await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("400"),
        "Expected 400 in error: {}",
        err_msg
    );
}

/// Default budget is 2 retries (3 attempts total) when no transient keyword
/// is observed in the error body. Status phrase "Internal Server Error" and
/// body "upstream error" together contain none of the escalation keywords.
#[tokio::test]
async fn test_default_budget_stops_at_3_attempts() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream error"))
        .expect(3)
        .mount(&server)
        .await;

    let client = LlmClient::new(&server.uri(), "test-key", "test-model");
    let messages = vec![user_msg("Hi")];
    let result = client.complete(&messages).await;

    assert!(result.is_err(), "expected exhaustion after 3 attempts");
    // wiremock asserts `expect(3)` on drop.
}

/// Mid-stream connection drop before any SSE frame is emitted: the retry is
/// safe (no partial output delivered) so the client transparently retries
/// the whole request.
#[tokio::test]
async fn test_mid_stream_retries_when_no_text_emitted() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // First connection: reply 200 + chunked headers, then close with no data
    // frames. Second connection: reply with a complete valid SSE body.
    let server = tokio::spawn(async move {
        // First attempt — drop socket before any `data:` frame.
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let resp = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Transfer-Encoding: chunked\r\n\
                        \r\n";
            let _ = sock.write_all(resp.as_bytes()).await;
            // Drop mid-stream before any `data:` line.
            drop(sock);
        }

        // Second attempt — valid full SSE body with Content-Length.
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body =
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}\n\n\
                        data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n\
                        data: [DONE]\n\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: text/event-stream\r\n\
                 Content-Length: {}\r\n\
                 \r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let url = format!("http://{addr}");
    let client = LlmClient::new(&url, "test-key", "test-model");
    let messages = vec![user_msg("Hi")];

    let mut collected = Vec::new();
    let result = client
        .stream_completion(&messages, &[], &mut |t| collected.push(t.to_string()))
        .await
        .expect("second attempt should succeed");

    assert_eq!(result.message.content.unwrap().as_text().unwrap(), "ok");
    // Only the successful second attempt should have emitted any text.
    assert_eq!(collected, vec!["ok"]);
    server.await.unwrap();
}

/// Mid-stream failure AFTER a text delta has been delivered: retrying would
/// cause duplicate tokens at the caller, so the client downgrades the error
/// to fatal and surfaces it without another attempt.
#[tokio::test]
async fn test_mid_stream_fatal_after_partial_text() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Count incoming connections — must be exactly 1. If the client retries
    // the request (which it should NOT after partial output), the counter
    // will reach 2 and the assertion below will fail.
    let connect_count = Arc::new(AtomicUsize::new(0));
    let connect_count_task = connect_count.clone();
    let server = tokio::spawn(async move {
        loop {
            if let Ok((mut sock, _)) = listener.accept().await {
                connect_count_task.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let body = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"partial\"}}]}\n\n";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/event-stream\r\n\
                     Transfer-Encoding: chunked\r\n\
                     \r\n\
                     {len:x}\r\n\
                     {body}",
                    len = body.len(),
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                // Drop socket mid-stream after the one delta chunk.
                drop(sock);
            }
        }
    });

    let url = format!("http://{addr}");
    let client = LlmClient::new(&url, "test-key", "test-model");
    let messages = vec![user_msg("Hi")];

    let mut collected = Vec::new();
    let result = client
        .stream_completion(&messages, &[], &mut |t| collected.push(t.to_string()))
        .await;

    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("mid-stream error after partial output should be fatal"),
    };
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("mid-stream error after partial output emitted"),
        "unexpected error: {err_msg}"
    );
    // The partial delta must have been delivered to the caller before the
    // failure, and no retry should have happened.
    assert_eq!(collected, vec!["partial"]);
    assert_eq!(
        connect_count.load(Ordering::SeqCst),
        1,
        "expected exactly one HTTP connection; got a retry"
    );

    server.abort();
}
