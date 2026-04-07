//! Round-trip fixtures: parse a captured JSON payload into the
//! matching Rust type, re-serialize it, and assert that the JSON
//! values are equal (key order ignored).

use overacp_protocol::messages::{
    Activity, Heartbeat, InitializeResponse, Message, PollNewMessagesResponse, QuotaCheckResponse,
    QuotaUpdateRequest, TextDelta, ToolCallNotification, ToolResultNotification, TurnSaveRequest,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

fn assert_roundtrip<T: Serialize + DeserializeOwned>(fixture: &str) {
    let original: Value = serde_json::from_str(fixture).expect("fixture is not valid JSON");
    let typed: T = serde_json::from_value(original.clone()).expect("fixture failed to deserialize");
    let reserialized = serde_json::to_value(&typed).expect("re-serialize failed");
    assert_eq!(
        original, reserialized,
        "round-trip changed the JSON value\noriginal: {original}\nre-serialized: {reserialized}"
    );
}

#[test]
fn initialize_response_roundtrip() {
    assert_roundtrip::<InitializeResponse>(include_str!("fixtures/initialize_response.json"));
}

#[test]
fn quota_check_response_roundtrip() {
    assert_roundtrip::<QuotaCheckResponse>(include_str!("fixtures/quota_check_response.json"));
}

#[test]
fn quota_update_request_roundtrip() {
    assert_roundtrip::<QuotaUpdateRequest>(include_str!("fixtures/quota_update_request.json"));
}

#[test]
fn turn_save_request_roundtrip() {
    assert_roundtrip::<TurnSaveRequest>(include_str!("fixtures/turn_save_request.json"));
}

#[test]
fn poll_new_messages_response_roundtrip() {
    assert_roundtrip::<PollNewMessagesResponse>(include_str!(
        "fixtures/poll_new_messages_response.json"
    ));
}

#[test]
fn text_delta_roundtrip() {
    assert_roundtrip::<TextDelta>(include_str!("fixtures/stream_text_delta.json"));
}

#[test]
fn activity_roundtrip() {
    assert_roundtrip::<Activity>(include_str!("fixtures/stream_activity.json"));
}

#[test]
fn tool_call_notification_roundtrip() {
    assert_roundtrip::<ToolCallNotification>(include_str!("fixtures/stream_tool_call.json"));
}

#[test]
fn tool_result_notification_roundtrip() {
    assert_roundtrip::<ToolResultNotification>(include_str!("fixtures/stream_tool_result.json"));
}

#[test]
fn heartbeat_roundtrip() {
    assert_roundtrip::<Heartbeat>(include_str!("fixtures/heartbeat.json"));
}

#[test]
fn message_with_tool_calls_roundtrip() {
    assert_roundtrip::<Message>(include_str!("fixtures/message_with_tool_calls.json"));
}
