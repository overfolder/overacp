//! Retry policy + LLM transport error classification.
//!
//! Two public concepts for `client.rs`:
//!
//! * [`StreamError`] — a retryable/fatal classification wrapping the underlying
//!   cause, with [`classify_reqwest_error`] and [`classify_http_response`]
//!   constructors.
//! * [`RetryBudget`] — attempt cap + backoff base. Starts in the `default`
//!   preset (2 retries @ 500ms) and escalates to the `escalated` preset
//!   (4 retries @ 2s) the first time a transient-capacity keyword is observed
//!   in the error body.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::time::Duration;

/// Classifies LLM transport / HTTP errors so callers can decide whether to
/// retry.
///
/// `body` carries the raw response body text for HTTP-level errors so that
/// the keyword-escalation pass inspects only the upstream's payload, not the
/// HTTP status phrase (which on 503 already contains "Service Unavailable"
/// and would otherwise force every 503 to escalate regardless of cause).
#[derive(Debug)]
pub(super) enum StreamError {
    /// Transient failure (timeout, connection reset, 429/5xx) — caller
    /// should retry with backoff.
    Retryable {
        message: String,
        body: Option<String>,
        source: Box<dyn StdError + Send + Sync>,
    },
    /// Permanent failure (auth error, bad request) — retrying will not help.
    Fatal {
        message: String,
        body: Option<String>,
        source: Box<dyn StdError + Send + Sync>,
    },
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retryable { message, .. } | Self::Fatal { message, .. } => {
                write!(f, "{message}")
            }
        }
    }
}

impl StdError for StreamError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Retryable { source, .. } | Self::Fatal { source, .. } => Some(source.as_ref()),
        }
    }
}

impl StreamError {
    pub(super) const fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }

    pub(super) fn message(&self) -> &str {
        match self {
            Self::Retryable { message, .. } | Self::Fatal { message, .. } => message,
        }
    }

    /// Text the keyword classifier should inspect when deciding whether to
    /// escalate the retry budget. Prefers the raw response body (for HTTP
    /// errors) and falls back to the formatted message (for transport errors
    /// that have no body).
    pub(super) fn keyword_haystack(&self) -> &str {
        match self {
            Self::Retryable { body: Some(b), .. } | Self::Fatal { body: Some(b), .. } => b,
            Self::Retryable { message, .. } | Self::Fatal { message, .. } => message,
        }
    }

    /// Convert a retryable error into a fatal one wrapping itself, prefixed
    /// with `prefix`. Used when partial output has already been streamed and
    /// retrying would duplicate tokens at the caller.
    pub(super) fn into_fatal_with_prefix(self, prefix: &str) -> Self {
        if matches!(self, Self::Fatal { .. }) {
            return self;
        }
        let message = format!("{prefix}: {}", self.message());
        Self::Fatal {
            message,
            body: None,
            source: Box::new(self),
        }
    }
}

/// Inspect a `reqwest::Error` and wrap it as retryable or fatal.
pub(super) fn classify_reqwest_error(e: reqwest::Error, elapsed: Duration) -> StreamError {
    let elapsed_s = elapsed.as_secs_f64();
    if e.is_timeout() {
        StreamError::Retryable {
            message: format!("Stream read timed out after {elapsed_s:.1}s: {e}"),
            body: None,
            source: Box::new(e),
        }
    } else if e.is_connect() {
        StreamError::Retryable {
            message: format!("Connection error during stream after {elapsed_s:.1}s: {e}"),
            body: None,
            source: Box::new(e),
        }
    } else if e.is_body() || e.is_decode() {
        StreamError::Retryable {
            message: format!(
                "Stream body error (possible connection reset) after {elapsed_s:.1}s: {e}"
            ),
            body: None,
            source: Box::new(e),
        }
    } else if let Some(status) = e.status() {
        if status.is_server_error() {
            StreamError::Retryable {
                message: format!("Server error {status} during stream after {elapsed_s:.1}s: {e}"),
                body: None,
                source: Box::new(e),
            }
        } else {
            StreamError::Fatal {
                message: format!("Client error {status} during stream after {elapsed_s:.1}s: {e}"),
                body: None,
                source: Box::new(e),
            }
        }
    } else {
        StreamError::Retryable {
            message: format!("Stream error after {elapsed_s:.1}s: {e}"),
            body: None,
            source: Box::new(e),
        }
    }
}

/// Wrap a non-2xx HTTP response as a classified error. 429 and 5xx are
/// retryable; other 4xx are fatal. The raw response body is preserved
/// separately from the formatted display message so that keyword-based
/// budget escalation only sees what the upstream actually said, not the
/// HTTP status phrase.
pub(super) fn classify_http_response(status: reqwest::StatusCode, body: String) -> StreamError {
    let message = format!("LLM HTTP {status}: {body}");
    let source = Box::new(io::Error::other(message.clone()));
    if status.as_u16() == 429 || status.is_server_error() {
        StreamError::Retryable {
            message,
            body: Some(body),
            source,
        }
    } else {
        StreamError::Fatal {
            message,
            body: Some(body),
            source,
        }
    }
}

/// Transient-capacity keywords. A match in a failure body escalates the
/// retry budget once (never de-escalates). Matched case-insensitively as
/// substrings so e.g. "overloaded" and "rate_limit_exceeded" both hit.
const TRANSIENT_KEYWORDS: &[&str] = &[
    "overload",
    "unavailable",
    "limit",
    "quota",
    "capacity",
    "throttle",
    "congestion",
];

pub(super) fn contains_transient_keyword(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    TRANSIENT_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Retry budget: total attempt cap + exponential-backoff base.
#[derive(Debug, Clone, Copy)]
pub(super) struct RetryBudget {
    pub(super) max_attempts: u32,
    pub(super) base_delay: Duration,
    pub(super) escalated: bool,
}

impl RetryBudget {
    /// 2 retries (3 total attempts), 500ms base delay. The default policy for
    /// ordinary 5xx / connection hiccups.
    pub(super) const fn default_budget() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            escalated: false,
        }
    }

    /// 4 retries (5 total attempts), 2s base delay. Used once a transient-
    /// capacity keyword is detected in a failure body.
    pub(super) const fn escalated_budget() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_millis(2000),
            escalated: true,
        }
    }

    /// Backoff for the *next* wait, given how many retries have already
    /// happened (0 = first retry about to sleep).
    pub(super) fn delay_for(&self, retry_index: u32) -> Duration {
        self.base_delay * 2u32.pow(retry_index)
    }
}

/// If `err`'s body (or formatted message, when there is no body) contains a
/// transient-capacity keyword and `budget` is still the default, upgrade to
/// escalated. Otherwise return `budget` unchanged.
pub(super) fn escalate_if_transient(budget: RetryBudget, err: &StreamError) -> RetryBudget {
    if !budget.escalated && contains_transient_keyword(err.keyword_haystack()) {
        RetryBudget::escalated_budget()
    } else {
        budget
    }
}

/// Mutable state driven by both `complete` and `stream_completion` retry
/// loops. Tracks the active budget, the total attempt count (for the
/// `max_attempts` cap), and a separate exponent index for backoff
/// computation that resets when the budget escalates — so the first sleep
/// under the escalated budget is its documented base delay (2s) rather than
/// `base * 2^accumulated`.
pub(super) struct RetryState {
    pub(super) budget: RetryBudget,
    pub(super) attempt_num: u32,
    exponent_idx: u32,
}

impl RetryState {
    pub(super) const fn new() -> Self {
        Self {
            budget: RetryBudget::default_budget(),
            attempt_num: 1,
            exponent_idx: 0,
        }
    }

    /// Inspect the failure and decide whether to retry. On `Some(delay)` the
    /// caller should sleep that long, then call [`Self::advance`] before the
    /// next attempt. On `None` the error is fatal or the budget is exhausted.
    pub(super) fn next_delay(&mut self, err: &StreamError) -> Option<Duration> {
        let upgraded = escalate_if_transient(self.budget, err);
        let escalated_now = upgraded.escalated && !self.budget.escalated;
        self.budget = upgraded;

        if !err.is_retryable() || self.attempt_num >= self.budget.max_attempts {
            return None;
        }

        if escalated_now {
            self.exponent_idx = 0;
        }
        Some(self.budget.delay_for(self.exponent_idx))
    }

    /// Bump the attempt and exponent counters after a sleep completes.
    pub(super) fn advance(&mut self) {
        self.attempt_num += 1;
        self.exponent_idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_http_response, classify_reqwest_error, contains_transient_keyword,
        escalate_if_transient, RetryBudget, RetryState, StreamError,
    };
    use std::error::Error as _;
    use std::time::Duration;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn connect_error() -> reqwest::Error {
        reqwest::Client::new()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("connect to :1 should fail")
    }

    async fn timeout_error() -> reqwest::Error {
        reqwest::Client::builder()
            .timeout(Duration::from_millis(1))
            .build()
            .unwrap()
            .get("http://10.255.255.1/")
            .send()
            .await
            .expect_err("unroutable + 1ms timeout should fail")
    }

    #[tokio::test]
    async fn classifies_connect_error_as_retryable() {
        let e = connect_error().await;
        let classified = classify_reqwest_error(e, Duration::from_millis(500));
        assert!(classified.is_retryable());
        let msg = classified.to_string();
        assert!(
            msg.contains("Connection error") && msg.contains("0.5s"),
            "unexpected message: {msg}"
        );
        assert!(classified.source().is_some());
    }

    #[tokio::test]
    async fn classifies_timeout_as_retryable() {
        let e = timeout_error().await;
        let classified = classify_reqwest_error(e, Duration::from_millis(1200));
        assert!(classified.is_retryable());
        assert!(classified.to_string().contains("1.2s"));
    }

    #[tokio::test]
    async fn classifies_4xx_status_as_fatal() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let e = reqwest::get(server.uri())
            .await
            .unwrap()
            .error_for_status()
            .expect_err("401 should be error_for_status err");
        let classified = classify_reqwest_error(e, Duration::from_millis(100));
        assert!(matches!(classified, StreamError::Fatal { .. }));
        assert!(!classified.is_retryable());
        assert!(classified.to_string().contains("401"));
    }

    #[tokio::test]
    async fn classifies_5xx_status_as_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let e = reqwest::get(server.uri())
            .await
            .unwrap()
            .error_for_status()
            .expect_err("503 should be error_for_status err");
        let classified = classify_reqwest_error(e, Duration::from_millis(100));
        assert!(classified.is_retryable());
        assert!(classified.to_string().contains("503"));
    }

    #[test]
    fn http_response_429_is_retryable() {
        let e = classify_http_response(reqwest::StatusCode::TOO_MANY_REQUESTS, "slow down".into());
        assert!(e.is_retryable());
        assert!(e.message().contains("429"));
    }

    #[test]
    fn http_response_400_is_fatal() {
        let e = classify_http_response(reqwest::StatusCode::BAD_REQUEST, "bad input".into());
        assert!(!e.is_retryable());
        assert!(matches!(e, StreamError::Fatal { .. }));
    }

    #[test]
    fn keyword_match_is_case_insensitive() {
        assert!(contains_transient_keyword("Model Is OVERLOADED"));
        assert!(contains_transient_keyword("you hit your quota"));
        assert!(contains_transient_keyword("upstream capacity exceeded"));
        assert!(contains_transient_keyword("service unavailable"));
        assert!(contains_transient_keyword("please throttle"));
        assert!(contains_transient_keyword("network congestion detected"));
        assert!(contains_transient_keyword("rate limit exceeded"));
    }

    #[test]
    fn keyword_miss_returns_false() {
        assert!(!contains_transient_keyword("internal server error"));
        assert!(!contains_transient_keyword("bad gateway"));
        assert!(!contains_transient_keyword(""));
    }

    #[test]
    fn budget_backoff_doubles() {
        let b = RetryBudget::default_budget();
        assert_eq!(b.delay_for(0), Duration::from_millis(500));
        assert_eq!(b.delay_for(1), Duration::from_millis(1000));
        assert_eq!(b.delay_for(2), Duration::from_millis(2000));

        let e = RetryBudget::escalated_budget();
        assert_eq!(e.delay_for(0), Duration::from_millis(2000));
        assert_eq!(e.delay_for(3), Duration::from_millis(16000));
    }

    #[test]
    fn escalation_happens_once_on_keyword() {
        let budget = RetryBudget::default_budget();
        let err = classify_http_response(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "model overloaded".into(),
        );
        let upgraded = escalate_if_transient(budget, &err);
        assert!(upgraded.escalated);
        assert_eq!(upgraded.max_attempts, 5);

        let err2 = classify_http_response(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "model overloaded".into(),
        );
        let still = escalate_if_transient(upgraded, &err2);
        assert!(still.escalated);
        assert_eq!(still.max_attempts, 5);
    }

    #[test]
    fn escalation_skipped_without_keyword() {
        let budget = RetryBudget::default_budget();
        let err = classify_http_response(reqwest::StatusCode::BAD_GATEWAY, "upstream error".into());
        let same = escalate_if_transient(budget, &err);
        assert!(!same.escalated);
        assert_eq!(same.max_attempts, 3);
    }

    /// Regression: the HTTP status phrase "Service Unavailable" contains the
    /// keyword "unavailable" but escalation must look only at the upstream
    /// body, not the canned status text. A 503 with a non-capacity body
    /// stays on the default budget.
    #[test]
    fn escalation_ignores_status_phrase_keywords() {
        let budget = RetryBudget::default_budget();
        let err = classify_http_response(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "internal error".into(),
        );
        let same = escalate_if_transient(budget, &err);
        assert!(
            !same.escalated,
            "503 with keyword-free body must not escalate via status phrase"
        );
        assert_eq!(same.max_attempts, 3);
    }

    /// Transport errors carry no body — fall back to the formatted message.
    /// Connection errors (no keyword) must not escalate.
    #[tokio::test]
    async fn escalation_skipped_for_keywordless_transport_error() {
        let e = connect_error().await;
        let classified = classify_reqwest_error(e, Duration::from_millis(100));
        let budget = RetryBudget::default_budget();
        let same = escalate_if_transient(budget, &classified);
        assert!(!same.escalated);
    }

    #[test]
    fn into_fatal_wraps_retryable() {
        let err = classify_http_response(reqwest::StatusCode::SERVICE_UNAVAILABLE, "boom".into());
        assert!(err.is_retryable());
        let fatal = err.into_fatal_with_prefix("mid-stream error");
        assert!(!fatal.is_retryable());
        assert!(fatal.message().starts_with("mid-stream error: "));
        assert!(fatal.source().is_some());
    }

    #[test]
    fn into_fatal_is_identity_on_fatal() {
        let err = classify_http_response(reqwest::StatusCode::BAD_REQUEST, "nope".into());
        let msg_before = err.message().to_string();
        let still_fatal = err.into_fatal_with_prefix("ignored");
        assert!(!still_fatal.is_retryable());
        assert_eq!(still_fatal.message(), msg_before);
    }

    /// Retry-state walkthrough for the default budget alone — three attempts,
    /// 500ms then 1s backoff, then exhaustion.
    #[test]
    fn retry_state_default_budget_walkthrough() {
        let mut state = RetryState::new();
        let err = || classify_http_response(reqwest::StatusCode::BAD_GATEWAY, "boom".into());

        let d1 = state.next_delay(&err()).expect("attempt 1 → retry 1");
        assert_eq!(d1, Duration::from_millis(500));
        state.advance();

        let d2 = state.next_delay(&err()).expect("attempt 2 → retry 2");
        assert_eq!(d2, Duration::from_millis(1000));
        state.advance();

        // Attempt 3 fails — budget (3 max) is exhausted, no further retry.
        assert!(state.next_delay(&err()).is_none());
    }

    /// Regression for Sentry PR feedback on #43: when escalation happens
    /// mid-loop, the next sleep must be the escalated budget's *base* (2s),
    /// not `base * 2^accumulated`.
    #[test]
    fn retry_state_resets_exponent_on_escalation() {
        let mut state = RetryState::new();
        let benign = classify_http_response(reqwest::StatusCode::BAD_GATEWAY, "boom".into());
        let transient =
            classify_http_response(reqwest::StatusCode::BAD_GATEWAY, "model overloaded".into());

        // Attempt 1 (default budget): fails non-transiently → 500ms backoff.
        let d1 = state.next_delay(&benign).unwrap();
        assert_eq!(d1, Duration::from_millis(500));
        assert!(!state.budget.escalated);
        state.advance();

        // Attempt 2: fails with a transient keyword. Budget escalates and the
        // next backoff is the escalated base (2s), NOT 2s * 2 = 4s.
        let d2 = state.next_delay(&transient).unwrap();
        assert_eq!(d2, Duration::from_millis(2000));
        assert!(state.budget.escalated);
        state.advance();

        // Attempt 3 onward: backoff doubles within the escalated budget.
        let d3 = state.next_delay(&transient).unwrap();
        assert_eq!(d3, Duration::from_millis(4000));
        state.advance();

        let d4 = state.next_delay(&transient).unwrap();
        assert_eq!(d4, Duration::from_millis(8000));
        state.advance();

        // 5 attempts total under escalated → next call exhausts.
        assert!(state.next_delay(&transient).is_none());
    }

    #[test]
    fn retry_state_returns_none_on_fatal() {
        let mut state = RetryState::new();
        let fatal = classify_http_response(reqwest::StatusCode::BAD_REQUEST, "nope".into());
        assert!(state.next_delay(&fatal).is_none());
    }
}
