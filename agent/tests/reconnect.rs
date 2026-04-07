//! Smoke tests for the reconnect/backoff helper.

use overacp_agent::tunnel::backoff_secs;

#[test]
fn backoff_grows_then_caps() {
    assert_eq!(backoff_secs(0), 1);
    assert_eq!(backoff_secs(1), 2);
    assert_eq!(backoff_secs(2), 4);
    assert_eq!(backoff_secs(3), 8);
    assert_eq!(backoff_secs(4), 16);
    // Capped at 30s.
    assert_eq!(backoff_secs(5), 30);
    assert_eq!(backoff_secs(10), 30);
    assert_eq!(backoff_secs(u32::MAX), 30);
}
