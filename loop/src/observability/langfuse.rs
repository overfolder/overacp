//! Optional Langfuse LLM observability.
//!
//! [`LangfuseTracer`] is the app-level handle; [`SessionTrace`] is the
//! per-session handle threaded through the agentic loop. When credentials
//! (`LANGFUSE_PUBLIC_KEY` + `LANGFUSE_SECRET_KEY`) are missing, every
//! method is a no-op — zero overhead beyond an `Option::is_none` check.
//!
//! All HTTP POSTs to the Langfuse ingestion API are fire-and-forget via
//! `tokio::spawn` so tracing never blocks the loop.

use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::Config;

#[derive(Clone)]
pub struct LangfuseTracer {
    inner: Option<Arc<LangfuseInner>>,
    environment: String,
}

struct LangfuseInner {
    base_url: String,
    public_key: String,
    secret_key: String,
    http: Client,
}

impl LangfuseTracer {
    pub fn new(config: &Config) -> Self {
        let inner = match (&config.langfuse_public_key, &config.langfuse_secret_key) {
            (Some(pk), Some(sk)) => {
                info!(host = %config.langfuse_host, "Langfuse tracing enabled");
                Some(Arc::new(LangfuseInner {
                    base_url: config.langfuse_host.trim_end_matches('/').to_string(),
                    public_key: pk.clone(),
                    secret_key: sk.clone(),
                    http: Client::new(),
                }))
            }
            _ => {
                info!("Langfuse tracing disabled (no credentials)");
                None
            }
        };

        Self {
            inner,
            environment: config.langfuse_environment.clone(),
        }
    }

    pub const fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Start a trace for a session. The `session_id` groups related traces
    /// in the Langfuse UI; callers pass a stable ID (e.g. the ACP session
    /// id) to correlate multi-turn work.
    pub fn start_session(&self, session_id: String) -> SessionTrace {
        let inner = self.inner.as_ref().map(|i| SessionTraceInner {
            client: Arc::clone(i),
            trace_id: Uuid::new_v4().to_string(),
            session_id,
            environment: self.environment.clone(),
        });
        SessionTrace { inner }
    }
}

pub struct SessionTrace {
    inner: Option<SessionTraceInner>,
}

struct SessionTraceInner {
    client: Arc<LangfuseInner>,
    trace_id: String,
    session_id: String,
    environment: String,
}

pub struct GenerationParams {
    pub model: String,
    pub message_count: usize,
    pub input_preview: Value,
    pub output_text: Option<String>,
    pub stop_reason: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub level: Option<String>,
    pub status_message: Option<String>,
}

pub struct ToolSpanParams {
    pub name: String,
    pub input: String,
    pub output: String,
    pub is_error: bool,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
}

impl SessionTrace {
    /// Create a disabled (no-op) trace — useful for tests and for
    /// code paths that bypass Langfuse.
    pub const fn noop() -> Self {
        Self { inner: None }
    }

    pub const fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    pub fn trace_id(&self) -> Option<&str> {
        self.inner.as_ref().map(|i| i.trace_id.as_str())
    }

    /// Create the trace on Langfuse. Call once at session start.
    pub fn create_trace(&self, user_message: &str, tags: Vec<String>) {
        let Some(inner) = &self.inner else { return };
        let client = Arc::clone(&inner.client);
        let trace_id = inner.trace_id.clone();
        let session_id = inner.session_id.clone();
        let environment = inner.environment.clone();
        let input = user_message.to_string();
        let mut all_tags = vec![environment.clone()];
        all_tags.extend(tags);

        tokio::spawn(async move {
            let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let body = json!({
                "batch": [{
                    "id": Uuid::new_v4().to_string(),
                    "type": "trace-create",
                    "timestamp": &ts,
                    "body": {
                        "id": &trace_id,
                        "timestamp": &ts,
                        "name": "agent-session",
                        "sessionId": &session_id,
                        "environment": &environment,
                        "input": &input,
                        "tags": &all_tags,
                    }
                }]
            });
            post_ingestion(&client, &body, &trace_id, "create trace").await;
        });
    }

    /// Record one LLM call. Fire after the call completes (success or
    /// error — failed calls set `level = Some("ERROR")` and `status_message`).
    pub fn record_generation(&self, params: GenerationParams) {
        let Some(inner) = &self.inner else { return };
        let client = Arc::clone(&inner.client);
        let trace_id = inner.trace_id.clone();
        let environment = inner.environment.clone();

        tokio::spawn(async move {
            let total_tokens = params.prompt_tokens + params.completion_tokens;
            let start = params
                .start_time
                .to_rfc3339_opts(SecondsFormat::Millis, true);
            let end = params.end_time.to_rfc3339_opts(SecondsFormat::Millis, true);
            let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);

            let mut body_inner = json!({
                "id": Uuid::new_v4().to_string(),
                "traceId": &trace_id,
                "name": "llm-call",
                "environment": &environment,
                "model": &params.model,
                "input": &params.input_preview,
                "output": &params.output_text,
                "metadata": {
                    "stop_reason": &params.stop_reason,
                    "message_count": params.message_count,
                    "cost_usd": params.cost,
                    "cache_read_tokens": params.cache_read_tokens,
                    "cache_creation_tokens": params.cache_creation_tokens,
                },
                "usage": {
                    "promptTokens": params.prompt_tokens,
                    "completionTokens": params.completion_tokens,
                    "totalTokens": total_tokens,
                },
                "startTime": &start,
                "endTime": &end,
            });

            if let Some(level) = &params.level {
                body_inner["level"] = json!(level);
            }
            if let Some(msg) = &params.status_message {
                body_inner["statusMessage"] = json!(msg);
            }

            let body = json!({
                "batch": [{
                    "id": Uuid::new_v4().to_string(),
                    "type": "generation-create",
                    "timestamp": &ts,
                    "body": body_inner,
                }]
            });
            post_ingestion(&client, &body, &trace_id, "record generation").await;
        });
    }

    /// Record a tool invocation as a span.
    pub fn record_tool_span(&self, params: ToolSpanParams) {
        let Some(inner) = &self.inner else { return };
        let client = Arc::clone(&inner.client);
        let trace_id = inner.trace_id.clone();
        let environment = inner.environment.clone();

        tokio::spawn(async move {
            let level = if params.is_error { "ERROR" } else { "DEFAULT" };
            let start = params
                .start_time
                .to_rfc3339_opts(SecondsFormat::Millis, true);
            let end = params.end_time.to_rfc3339_opts(SecondsFormat::Millis, true);
            let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);

            let body = json!({
                "batch": [{
                    "id": Uuid::new_v4().to_string(),
                    "type": "span-create",
                    "timestamp": &ts,
                    "body": {
                        "id": Uuid::new_v4().to_string(),
                        "traceId": &trace_id,
                        "name": &params.name,
                        "environment": &environment,
                        "startTime": &start,
                        "endTime": &end,
                        "level": level,
                        "input": &params.input,
                        "output": &params.output,
                        "metadata": { "is_error": params.is_error },
                    }
                }]
            });
            post_ingestion(&client, &body, &trace_id, "record tool span").await;
        });
    }

    /// Finalize the trace with session totals and the agent's response.
    pub fn finalize(&self, total_tokens: u64, total_cost: f64, tool_count: usize, response: &str) {
        let Some(inner) = &self.inner else { return };
        let client = Arc::clone(&inner.client);
        let trace_id = inner.trace_id.clone();
        let session_id = inner.session_id.clone();
        let environment = inner.environment.clone();
        let output = response.to_string();

        tokio::spawn(async move {
            let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let body = json!({
                "batch": [{
                    "id": Uuid::new_v4().to_string(),
                    "type": "trace-create",
                    "timestamp": &ts,
                    "body": {
                        "id": &trace_id,
                        "timestamp": &ts,
                        "name": "agent-session",
                        "sessionId": &session_id,
                        "environment": &environment,
                        "output": &output,
                        "metadata": {
                            "total_tokens": total_tokens,
                            "total_cost_usd": total_cost,
                            "tool_calls": tool_count,
                        },
                    }
                }]
            });
            post_ingestion(&client, &body, &trace_id, "finalize trace").await;
        });
    }
}

async fn post_ingestion(client: &LangfuseInner, body: &Value, trace_id: &str, action: &str) {
    let result = client
        .http
        .post(format!("{}/api/public/ingestion", client.base_url))
        .basic_auth(&client.public_key, Some(&client.secret_key))
        .json(body)
        .send()
        .await;

    match result {
        Ok(resp) if !resp.status().is_success() => {
            warn!(
                status = %resp.status(),
                trace_id = %trace_id,
                action = %action,
                "Langfuse ingestion failed"
            );
        }
        Err(e) => {
            warn!(error = %e, trace_id = %trace_id, action = %action, "Langfuse HTTP error");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn test_config(public: Option<&str>, secret: Option<&str>) -> Config {
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
            langfuse_host: "https://cloud.langfuse.com".into(),
            langfuse_environment: "test".into(),
        }
    }

    #[test]
    fn disabled_tracer_is_noop() {
        let cfg = test_config(None, None);
        let tracer = LangfuseTracer::new(&cfg);
        assert!(!tracer.is_enabled());

        let trace = tracer.start_session("sess-1".into());
        assert!(!trace.is_active());
        assert!(trace.trace_id().is_none());

        // Every method should be a no-op (no panic, no runtime needed).
        trace.create_trace("hi", vec!["tag".into()]);
        let now = Utc::now();
        trace.record_generation(GenerationParams {
            model: "m".into(),
            message_count: 2,
            input_preview: json!({"message_count": 2}),
            output_text: Some("ok".into()),
            stop_reason: "stop".into(),
            prompt_tokens: 10,
            completion_tokens: 5,
            cost: 0.0,
            start_time: now - ChronoDuration::milliseconds(100),
            end_time: now,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            level: None,
            status_message: None,
        });
        trace.record_tool_span(ToolSpanParams {
            name: "read".into(),
            input: "{}".into(),
            output: "ok".into(),
            is_error: false,
            start_time: now - ChronoDuration::milliseconds(10),
            end_time: now,
        });
        trace.finalize(15, 0.0, 1, "done");
    }

    #[test]
    fn enabled_tracer_yields_active_session() {
        let cfg = test_config(Some("pk"), Some("sk"));
        let tracer = LangfuseTracer::new(&cfg);
        assert!(tracer.is_enabled());
        let trace = tracer.start_session("sess".into());
        assert!(trace.is_active());
        assert!(trace.trace_id().is_some());
    }
}
