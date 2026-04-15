//! Integration tests for the Langfuse observability module.
//!
//! Stands up a wiremock server that accepts Langfuse ingestion POSTs,
//! drives the tracer, and verifies that every `SessionTrace` method
//! (create / generation / tool span / finalize) posts the expected batch.

use std::time::Duration;

use chrono::Utc;
use overloop::config::Config;
use overloop::observability::{GenerationParams, LangfuseTracer, ToolSpanParams};
use serde_json::{json, Value};
use tokio::time::sleep;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config(host: &str, public: Option<&str>, secret: Option<&str>) -> Config {
    Config {
        llm_api_key: "k".into(),
        llm_api_url: "http://x".into(),
        model: "m".into(),
        workspace: ".".into(),
        mcp_servers: vec![],
        max_iterations: 1,
        timeout_minutes: 1,
        agent_name: None,
        sentry_dsn: None,
        sentry_environment: "local".into(),
        sentry_traces_sample_rate: 0.0,
        langfuse_public_key: public.map(str::to_string),
        langfuse_secret_key: secret.map(str::to_string),
        langfuse_host: host.to_string(),
        langfuse_environment: "test".into(),
    }
}

/// Wait up to `~1s` for the wiremock server to receive at least `n` requests.
async fn wait_for_requests(server: &MockServer, n: usize) -> Vec<wiremock::Request> {
    for _ in 0..40 {
        let reqs = server.received_requests().await.unwrap_or_default();
        if reqs.len() >= n {
            return reqs;
        }
        sleep(Duration::from_millis(25)).await;
    }
    server.received_requests().await.unwrap_or_default()
}

fn batch_type(req: &wiremock::Request) -> String {
    let v: Value = serde_json::from_slice(&req.body).expect("valid json");
    v["batch"][0]["type"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn batch_body(req: &wiremock::Request) -> Value {
    let v: Value = serde_json::from_slice(&req.body).expect("valid json");
    v["batch"][0]["body"].clone()
}

#[tokio::test]
async fn create_trace_posts_trace_create() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/ingestion"))
        .respond_with(ResponseTemplate::new(207))
        .mount(&server)
        .await;

    let cfg = test_config(&server.uri(), Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    assert!(tracer.is_enabled());
    let trace = tracer.start_session("sess-1".into());
    assert!(trace.is_active());
    assert!(trace.trace_id().is_some());

    trace.create_trace("hello world", vec!["model-x".into()]);

    let reqs = wait_for_requests(&server, 1).await;
    assert_eq!(reqs.len(), 1);
    assert_eq!(batch_type(&reqs[0]), "trace-create");
    let body = batch_body(&reqs[0]);
    assert_eq!(body["name"], "agent-session");
    assert_eq!(body["sessionId"], "sess-1");
    assert_eq!(body["environment"], "test");
    assert_eq!(body["input"], "hello world");
    let tags = body["tags"].as_array().unwrap();
    // Environment is prepended to user-supplied tags.
    assert!(tags.iter().any(|t| t == "test"));
    assert!(tags.iter().any(|t| t == "model-x"));
    // Basic auth header must be set.
    let auth = reqs[0]
        .headers
        .get("authorization")
        .expect("authorization header");
    assert!(auth.to_str().unwrap().starts_with("Basic "));
}

#[tokio::test]
async fn record_generation_success_posts_generation_create() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/ingestion"))
        .respond_with(ResponseTemplate::new(207))
        .mount(&server)
        .await;

    let cfg = test_config(&server.uri(), Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    let trace = tracer.start_session("s".into());

    let now = Utc::now();
    trace.record_generation(GenerationParams {
        model: "gpt-4".into(),
        message_count: 3,
        input_preview: json!({"message_count": 3}),
        output_text: Some("answer".into()),
        stop_reason: "stop".into(),
        prompt_tokens: 100,
        completion_tokens: 25,
        cost: 0.0,
        start_time: now - chrono::Duration::milliseconds(200),
        end_time: now,
        cache_read_tokens: 80,
        cache_creation_tokens: 5,
        level: None,
        status_message: None,
    });

    let reqs = wait_for_requests(&server, 1).await;
    assert_eq!(reqs.len(), 1);
    assert_eq!(batch_type(&reqs[0]), "generation-create");
    let body = batch_body(&reqs[0]);
    assert_eq!(body["model"], "gpt-4");
    assert_eq!(body["name"], "llm-call");
    assert_eq!(body["usage"]["promptTokens"], 100);
    assert_eq!(body["usage"]["completionTokens"], 25);
    assert_eq!(body["usage"]["totalTokens"], 125);
    assert_eq!(body["metadata"]["stop_reason"], "stop");
    assert_eq!(body["metadata"]["cache_read_tokens"], 80);
    assert_eq!(body["metadata"]["cache_creation_tokens"], 5);
    // Success path — no level / statusMessage fields.
    assert!(body.get("level").is_none());
    assert!(body.get("statusMessage").is_none());
}

#[tokio::test]
async fn record_generation_error_sets_level_and_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/ingestion"))
        .respond_with(ResponseTemplate::new(207))
        .mount(&server)
        .await;

    let cfg = test_config(&server.uri(), Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    let trace = tracer.start_session("s".into());

    let now = Utc::now();
    trace.record_generation(GenerationParams {
        model: "gpt-4".into(),
        message_count: 2,
        input_preview: json!({"message_count": 2}),
        output_text: None,
        stop_reason: "error".into(),
        prompt_tokens: 0,
        completion_tokens: 0,
        cost: 0.0,
        start_time: now - chrono::Duration::milliseconds(50),
        end_time: now,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        level: Some("ERROR".into()),
        status_message: Some("upstream 500: boom".into()),
    });

    let reqs = wait_for_requests(&server, 1).await;
    let body = batch_body(&reqs[0]);
    assert_eq!(body["level"], "ERROR");
    assert_eq!(body["statusMessage"], "upstream 500: boom");
}

#[tokio::test]
async fn record_tool_span_sets_error_level_on_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/ingestion"))
        .respond_with(ResponseTemplate::new(207))
        .mount(&server)
        .await;

    let cfg = test_config(&server.uri(), Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    let trace = tracer.start_session("s".into());

    let now = Utc::now();
    trace.record_tool_span(ToolSpanParams {
        name: "read".into(),
        input: r#"{"path":"/x"}"#.into(),
        output: "ok".into(),
        is_error: false,
        start_time: now - chrono::Duration::milliseconds(10),
        end_time: now,
    });
    trace.record_tool_span(ToolSpanParams {
        name: "exec".into(),
        input: r#"{"cmd":"false"}"#.into(),
        output: "Error: exit 1".into(),
        is_error: true,
        start_time: now - chrono::Duration::milliseconds(10),
        end_time: now,
    });

    let reqs = wait_for_requests(&server, 2).await;
    assert_eq!(reqs.len(), 2);
    assert!(reqs.iter().all(|r| batch_type(r) == "span-create"));

    let bodies: Vec<Value> = reqs.iter().map(batch_body).collect();
    let read_body = bodies.iter().find(|b| b["name"] == "read").unwrap();
    let exec_body = bodies.iter().find(|b| b["name"] == "exec").unwrap();
    assert_eq!(read_body["level"], "DEFAULT");
    assert_eq!(read_body["metadata"]["is_error"], false);
    assert_eq!(exec_body["level"], "ERROR");
    assert_eq!(exec_body["metadata"]["is_error"], true);
}

#[tokio::test]
async fn finalize_posts_trace_with_totals() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/ingestion"))
        .respond_with(ResponseTemplate::new(207))
        .mount(&server)
        .await;

    let cfg = test_config(&server.uri(), Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    let trace = tracer.start_session("final".into());

    trace.finalize(1234, 0.0, 7, "done");

    let reqs = wait_for_requests(&server, 1).await;
    assert_eq!(batch_type(&reqs[0]), "trace-create");
    let body = batch_body(&reqs[0]);
    assert_eq!(body["sessionId"], "final");
    assert_eq!(body["output"], "done");
    assert_eq!(body["metadata"]["total_tokens"], 1234);
    assert_eq!(body["metadata"]["tool_calls"], 7);
}

#[tokio::test]
async fn non_success_response_does_not_panic() {
    // Drive the warn-on-non-success branch of post_ingestion.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/ingestion"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let cfg = test_config(&server.uri(), Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    let trace = tracer.start_session("err".into());

    trace.create_trace("hi", vec![]);

    // One request should still have hit the server; the tracer logs a
    // warning but does not propagate.
    let reqs = wait_for_requests(&server, 1).await;
    assert_eq!(reqs.len(), 1);
}

#[tokio::test]
async fn transport_error_does_not_panic() {
    // Drive the warn-on-HTTP-error branch: point at an unreachable host.
    let cfg = test_config("http://127.0.0.1:1", Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    let trace = tracer.start_session("err".into());

    trace.create_trace("hi", vec![]);
    // Give the spawned task time to attempt + fail + log.
    sleep(Duration::from_millis(200)).await;
    // Survived without panic — assertion is implicit.
}

#[tokio::test]
async fn trailing_slash_host_is_normalised() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/ingestion"))
        .respond_with(ResponseTemplate::new(207))
        .mount(&server)
        .await;

    // Host with trailing slash should still hit /api/public/ingestion
    // (no double-slash).
    let host = format!("{}/", server.uri());
    let cfg = test_config(&host, Some("pk"), Some("sk"));
    let tracer = LangfuseTracer::new(&cfg);
    let trace = tracer.start_session("s".into());
    trace.create_trace("hi", vec![]);
    let reqs = wait_for_requests(&server, 1).await;
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url.path(), "/api/public/ingestion");
}
