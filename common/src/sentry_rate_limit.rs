//! Per-fingerprint rate limiter for Sentry events.
//!
//! Long-running Rust services that auto-capture `tracing::error!` via the
//! `sentry-tracing` layer are easy to flood: a single deterministic failure
//! on a recurring background task (scheduler tick, consumer loop, reaper,
//! sweep) can produce thousands of identical events in a day because every
//! tick re-raises the same error.
//!
//! This module is the choke point wired into the `before_send` hook of
//! `sentry::ClientOptions`: each distinct event fingerprint produces at
//! most one Sentry event per [`WINDOW`]. Local stdout/tracing output is
//! unaffected — throttling happens inside the Sentry client pipeline,
//! after `tracing` has already fanned the log line out.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sentry::protocol::Event;

/// Minimum interval between Sentry events with the same fingerprint.
const WINDOW: Duration = Duration::from_secs(60);

/// Drop fingerprint entries older than this on every check to bound the
/// map size across long uptimes. Cardinality is low in practice (dozens
/// of unique callsites), so this is mostly hygiene.
const PRUNE_AFTER: Duration = Duration::from_secs(600);

static LAST_SEEN: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns `true` if an event with this `key` should be forwarded to
/// Sentry, `false` if it should be dropped because an event with the
/// same key was already sent less than [`WINDOW`] ago.
pub fn should_send(key: &str) -> bool {
    should_send_inner(&LAST_SEEN, key, Instant::now(), WINDOW, PRUNE_AFTER)
}

/// Testable core: the map, clock, and window are injected so unit tests
/// can drive behavior deterministically without sleeping.
fn should_send_inner(
    map: &Mutex<HashMap<String, Instant>>,
    key: &str,
    now: Instant,
    window: Duration,
    prune_after: Duration,
) -> bool {
    let mut guard = map.lock().unwrap_or_else(PoisonError::into_inner);

    // Opportunistic prune so the map can't grow without bound.
    guard.retain(|_, last| now.saturating_duration_since(*last) < prune_after);

    match guard.get(key) {
        Some(last) if now.saturating_duration_since(*last) < window => false,
        _ => {
            guard.insert(key.to_string(), now);
            true
        }
    }
}

/// Maximum characters of a fallback message to include in a fingerprint.
///
/// Caps cardinality so that messages containing dynamic content (IDs,
/// timestamps, UUIDs) can't explode the rate-limiter map and bypass
/// throttling. 64 bytes is enough to distinguish common error prefixes
/// like `"Scheduler tick failed: SqlxError(...)"` vs
/// `"Scheduler tick failed: RedisError(...)"` while keeping dynamic tails
/// out of the key.
const MESSAGE_KEY_CAP: usize = 64;

/// Derive a stable fingerprint for a Sentry event.
///
/// Preference order:
/// 1. `event.culprit` — populated by `sentry-tracing` as `module::function`
///    (stable across repeated `error!` calls from the same call site). This
///    is the hot path for all background-task errors captured via the
///    sentry-tracing layer.
/// 2. `event.logger` + first exception **type** — covers explicit
///    `sentry::capture_error` calls. Deliberately omits `exception.value`
///    because it often contains dynamic content (row IDs, user IDs,
///    timestamps) that would defeat rate limiting.
/// 3. `event.logger` + bounded message prefix — final fallback, capped
///    at [`MESSAGE_KEY_CAP`] to bound cardinality from dynamic content.
pub fn event_key(event: &Event<'_>) -> String {
    if let Some(culprit) = event.culprit.as_deref() {
        return format!("culprit:{culprit}");
    }
    let logger = event.logger.as_deref().unwrap_or("");
    if let Some(exc) = event.exception.values.first() {
        return format!("exc:{logger}:{}", exc.ty);
    }
    let msg = event
        .message
        .as_deref()
        .or_else(|| event.logentry.as_ref().map(|l| l.message.as_str()))
        .unwrap_or("");
    format!("msg:{logger}:{}", truncate_chars(msg, MESSAGE_KEY_CAP))
}

/// Truncate to at most `max_chars` characters without splitting a UTF-8
/// code point. Returns the original string if it's already short enough.
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Drop-in `before_send` for `sentry::ClientOptions`. Forwards the event
/// iff [`event_key`] has not fired within [`WINDOW`].
pub fn before_send(event: Event<'static>) -> Option<Event<'static>> {
    should_send(&event_key(&event)).then_some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentry::protocol::{Exception, LogEntry, Values};

    fn fresh_map() -> Mutex<HashMap<String, Instant>> {
        Mutex::new(HashMap::new())
    }

    #[test]
    fn first_call_for_a_key_is_allowed() {
        let map = fresh_map();
        let now = Instant::now();
        assert!(should_send_inner(
            &map,
            "scheduler::tick",
            now,
            Duration::from_secs(60),
            Duration::from_secs(600),
        ));
    }

    #[test]
    fn repeated_call_within_window_is_dropped() {
        let map = fresh_map();
        let t0 = Instant::now();
        let window = Duration::from_secs(60);
        let prune = Duration::from_secs(600);

        assert!(should_send_inner(&map, "k", t0, window, prune));
        assert!(!should_send_inner(
            &map,
            "k",
            t0 + Duration::from_secs(1),
            window,
            prune
        ));
        assert!(!should_send_inner(
            &map,
            "k",
            t0 + Duration::from_secs(59),
            window,
            prune
        ));
    }

    #[test]
    fn call_after_window_is_allowed_again() {
        let map = fresh_map();
        let t0 = Instant::now();
        let window = Duration::from_secs(60);
        let prune = Duration::from_secs(600);

        assert!(should_send_inner(&map, "k", t0, window, prune));
        assert!(!should_send_inner(
            &map,
            "k",
            t0 + Duration::from_secs(30),
            window,
            prune
        ));
        // Exactly at the window boundary counts as "outside".
        assert!(should_send_inner(
            &map,
            "k",
            t0 + Duration::from_secs(60),
            window,
            prune
        ));
    }

    #[test]
    fn distinct_keys_do_not_interfere() {
        let map = fresh_map();
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let prune = Duration::from_secs(600);

        assert!(should_send_inner(
            &map,
            "scheduler::tick",
            now,
            window,
            prune
        ));
        assert!(should_send_inner(
            &map,
            "redis::consumer",
            now,
            window,
            prune
        ));
        assert!(!should_send_inner(
            &map,
            "scheduler::tick",
            now,
            window,
            prune
        ));
        assert!(!should_send_inner(
            &map,
            "redis::consumer",
            now,
            window,
            prune
        ));
    }

    #[test]
    fn stale_entries_are_pruned() {
        let map = fresh_map();
        let t0 = Instant::now();
        let window = Duration::from_secs(60);
        let prune = Duration::from_secs(600);

        assert!(should_send_inner(&map, "old", t0, window, prune));
        // Well past the prune horizon — the entry should be gone, so
        // sending any key again is treated as a fresh first call.
        let t1 = t0 + Duration::from_secs(700);
        assert!(should_send_inner(&map, "new", t1, window, prune));

        let contains_old = map.lock().unwrap().contains_key("old");
        assert!(!contains_old);
    }

    #[test]
    fn event_key_prefers_culprit() {
        let event = Event {
            culprit: Some("overloop::agentic_loop::run".into()),
            logger: Some("overloop".into()),
            message: Some("LLM request failed".into()),
            ..Default::default()
        };
        assert_eq!(event_key(&event), "culprit:overloop::agentic_loop::run");
    }

    #[test]
    fn event_key_exception_ignores_value_so_dynamic_content_does_not_bypass() {
        // Two events with the same exception type but different dynamic
        // values (e.g. different row IDs) should produce the same key so
        // the rate limiter collapses them. If `value` leaked into the
        // key, each unique ID would generate a unique key and bypass
        // throttling.
        let base = Event {
            logger: Some("overloop".into()),
            exception: Values {
                values: vec![Exception {
                    ty: "reqwest::Error".into(),
                    value: Some("request id=abc123 failed".into()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        let variant = Event {
            logger: Some("overloop".into()),
            exception: Values {
                values: vec![Exception {
                    ty: "reqwest::Error".into(),
                    value: Some("request id=xyz789 failed".into()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        assert_eq!(event_key(&base), "exc:overloop:reqwest::Error");
        assert_eq!(event_key(&base), event_key(&variant));
    }

    #[test]
    fn event_key_exception_type_still_discriminates_across_error_types() {
        // Different exception types in the same logger should still get
        // distinct keys so genuinely new failure modes surface.
        let http = Event {
            logger: Some("overloop".into()),
            exception: Values {
                values: vec![Exception {
                    ty: "reqwest::Error".into(),
                    value: Some("boom".into()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        let json = Event {
            logger: Some("overloop".into()),
            exception: Values {
                values: vec![Exception {
                    ty: "serde_json::Error".into(),
                    value: Some("boom".into()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        assert_ne!(event_key(&http), event_key(&json));
    }

    #[test]
    fn event_key_message_is_length_capped() {
        // Dynamic tail content past MESSAGE_KEY_CAP must not enter the
        // key, so bursts of similar messages with unique suffixes
        // collapse. Build a static prefix strictly longer than
        // MESSAGE_KEY_CAP, then differ only in the tail.
        let prefix: String = "x".repeat(MESSAGE_KEY_CAP + 10);
        let a = Event {
            logger: Some("overloop".into()),
            message: Some(format!("{prefix}_tail_abc123")),
            ..Default::default()
        };
        let b = Event {
            logger: Some("overloop".into()),
            message: Some(format!("{prefix}_tail_xyz789")),
            ..Default::default()
        };
        // The first MESSAGE_KEY_CAP chars are identical, so the events
        // collapse to the same key despite distinct dynamic tails.
        assert_eq!(event_key(&a), event_key(&b));
        assert_eq!(event_key(&a).len(), "msg:overloop:".len() + MESSAGE_KEY_CAP);
    }

    #[test]
    fn event_key_message_discriminates_on_static_prefix() {
        let a = Event {
            logger: Some("overloop".into()),
            message: Some("LLM request failed".into()),
            ..Default::default()
        };
        let b = Event {
            logger: Some("overloop".into()),
            message: Some("Tool invocation failed".into()),
            ..Default::default()
        };
        assert_ne!(event_key(&a), event_key(&b));
    }

    #[test]
    fn event_key_falls_back_to_logentry_message() {
        let event = Event {
            logger: Some("overloop".into()),
            logentry: Some(LogEntry {
                message: "Stream read failed".into(),
                params: vec![],
            }),
            ..Default::default()
        };
        assert_eq!(event_key(&event), "msg:overloop:Stream read failed");
    }

    #[test]
    fn truncate_chars_respects_utf8_boundaries() {
        // Truncating at a char count that would split a multibyte
        // sequence must not panic or produce invalid UTF-8.
        let s = "αβγδε"; // 5 code points, 10 bytes (2 bytes each).
        assert_eq!(truncate_chars(s, 3), "αβγ");
        assert_eq!(truncate_chars(s, 10), "αβγδε");
    }
}
