//! Thin JSON-RPC client that overloop speaks to its supervisor over
//! stdio. The supervisor (`overacp-agent`) bridges these frames to
//! the over/ACP broker's WebSocket tunnel.
//!
//! The wire vocabulary and payload shapes come from `overacp-protocol`
//! so that overloop cannot drift from the broker.

use anyhow::{Context, Result};
use overacp_protocol::messages::{
    Activity, Message as ProtoMessage, QuotaUpdateRequest, Role as ProtoRole, SessionMessageParams,
    TextDelta, ToolCallNotification, ToolResultNotification, TurnEndParams, Usage as ProtoUsage,
};
use overacp_protocol::methods;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Stdin, Stdout, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::llm::{Content, Message, Role, Usage};
use crate::traits::{AcpService, NextPush};

/// Shape of the `initialize` response, using the LLM-facing
/// [`Message`] type so the outer loop can drop the history directly
/// into its in-memory buffer without a second conversion.
///
/// The broker itself never inspects these fields — they are opaque
/// pass-through from the operator's `BootProvider` hook. Fields are
/// marked `#[serde(default)]` so a minimal boot response (empty
/// object) still parses cleanly.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitializeResult {
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools_config: Value,
}

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct JsonRpcNotification {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// ACP client communicating over JSON-RPC on any Read/Write stream.
pub struct AcpClient<R: Read, W: Write> {
    reader: BufReader<R>,
    writer: W,
}

impl AcpClient<Stdin, Stdout> {
    /// Create an AcpClient wired to process stdin/stdout.
    pub fn stdio() -> Self {
        Self {
            reader: BufReader::new(io::stdin()),
            writer: io::stdout(),
        }
    }
}

impl<R: Read, W: Write> AcpClient<R, W> {
    /// Create an AcpClient from arbitrary Read/Write streams.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    fn send_request(&mut self, method: &'static str, params: Option<Value>) -> Result<Value> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: next_id(),
            method,
            params,
        };

        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        self.writer
            .write_all(line.as_bytes())
            .context("write request")?;
        self.writer.flush()?;

        let mut buf = String::new();
        self.reader.read_line(&mut buf).context("read response")?;

        let resp: JsonRpcResponse = serde_json::from_str(&buf).context("parse ACP response")?;

        if let Some(err) = resp.error {
            anyhow::bail!("ACP error: {}", err.message);
        }

        resp.result
            .ok_or_else(|| anyhow::anyhow!("ACP response missing result"))
    }

    fn send_notification(&mut self, method: &'static str, params: Option<Value>) -> Result<()> {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&notif)?;
        line.push('\n');

        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// Call `initialize` once on cold start. The broker delegates to
    /// `BootProvider::initialize`; the returned value is opaque JSON
    /// and we parse it straight into the LLM-facing `Message` shape.
    pub fn initialize(&mut self) -> Result<InitializeResult> {
        let result = self.send_request(methods::INITIALIZE, None)?;
        serde_json::from_value(result).context("parse initialize result")
    }

    fn recv_notification(&mut self) -> Result<JsonRpcNotification> {
        let mut buf = String::new();
        let n = self
            .reader
            .read_line(&mut buf)
            .context("read notification")?;
        if n == 0 {
            anyhow::bail!("stdin closed while waiting for a notification");
        }
        serde_json::from_str(&buf).context("parse notification")
    }
}

// ── AcpService impl ─────────────────────────────────────────────────

impl<R: Read, W: Write> AcpService for AcpClient<R, W> {
    fn stream_text_delta(&mut self, text: &str) -> Result<()> {
        let params = TextDelta {
            text: text.to_string(),
        };
        self.send_notification(
            methods::STREAM_TEXT_DELTA,
            Some(serde_json::to_value(params)?),
        )
    }

    fn stream_activity(&mut self, activity: &str) -> Result<()> {
        // `stream/activity` carries a discriminator + opaque data per
        // docs/design/protocol.md § 3.4. Wrap the legacy single-string
        // form as a "log" kind.
        let params = Activity {
            kind: "log".to_string(),
            data: Value::String(activity.to_string()),
        };
        self.send_notification(
            methods::STREAM_ACTIVITY,
            Some(serde_json::to_value(params)?),
        )
    }

    fn stream_tool_call(&mut self, id: &str, name: &str, arguments: &Value) -> Result<()> {
        let params = ToolCallNotification {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.clone(),
        };
        self.send_notification(
            methods::STREAM_TOOL_CALL,
            Some(serde_json::to_value(params)?),
        )
    }

    fn stream_tool_result(&mut self, id: &str, content: &Value, is_error: bool) -> Result<()> {
        let params = ToolResultNotification {
            id: id.to_string(),
            content: content.clone(),
            is_error,
        };
        self.send_notification(
            methods::STREAM_TOOL_RESULT,
            Some(serde_json::to_value(params)?),
        )
    }

    fn turn_end(&mut self, messages: &[Message], usage: &Usage) -> Result<()> {
        // Convert LLM-facing messages into the protocol's `Message`
        // shape via serde round-trip. The two types serialize
        // identically (same field names, `tool_calls` as Value on the
        // protocol side absorbs the richer ToolCall typing); using
        // the typed `TurnEndParams` here makes the broker's wire
        // contract enforce the payload shape at compile time rather
        // than at runtime when the broker rejects it.
        let proto_messages: Vec<ProtoMessage> =
            serde_json::from_value(serde_json::to_value(messages)?)
                .context("convert llm::Message to protocol::Message for turn/end")?;
        let params = TurnEndParams {
            messages: proto_messages,
            usage: ProtoUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            },
        };
        self.send_notification(methods::TURN_END, Some(serde_json::to_value(params)?))
    }

    fn quota_check(&mut self) -> Result<bool> {
        let result = self.send_request(methods::QUOTA_CHECK, None)?;
        Ok(result
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(true))
    }

    fn quota_update(&mut self, input_tokens: u64, output_tokens: u64) -> Result<()> {
        let params = QuotaUpdateRequest {
            input_tokens,
            output_tokens,
        };
        self.send_request(methods::QUOTA_UPDATE, Some(serde_json::to_value(params)?))?;
        Ok(())
    }

    fn next_push(&mut self) -> Result<NextPush> {
        loop {
            let notif = self.recv_notification()?;
            match notif.method.as_str() {
                m if m == methods::SESSION_MESSAGE => {
                    let params: SessionMessageParams = serde_json::from_value(notif.params)
                        .context("parse session/message params")?;
                    let msg = session_message_to_llm_message(params)?;
                    return Ok(NextPush::Message(msg));
                }
                m if m == methods::SESSION_CANCEL => {
                    return Ok(NextPush::Cancel);
                }
                other => {
                    tracing::warn!(method = other, "ignoring unexpected tunnel notification");
                }
            }
        }
    }

    fn heartbeat(&mut self) -> Result<()> {
        self.send_notification(methods::HEARTBEAT, None)
    }
}

/// Convert the typed `SessionMessageParams` from the protocol crate
/// into the LLM-facing `Message` shape the agentic loop operates on.
///
/// `content` is opaque on the wire (the broker never inspects it),
/// so we accept both a bare string (`Content::Text`) and a structured
/// list of content blocks (`Content::Blocks`).
fn session_message_to_llm_message(params: SessionMessageParams) -> Result<Message> {
    let role = match params.role {
        ProtoRole::System => Role::System,
        ProtoRole::User => Role::User,
        ProtoRole::Assistant => Role::Assistant,
        ProtoRole::Tool => Role::Tool,
    };

    let content = match params.content {
        Value::Null => None,
        Value::String(s) => Some(Content::Text(s)),
        other @ (Value::Array(_) | Value::Object(_)) => {
            Some(serde_json::from_value(other).context("parse session/message content blocks")?)
        }
        other => Some(Content::Text(other.to_string())),
    };

    Ok(Message {
        role,
        content,
        tool_calls: None,
        tool_call_id: None,
    })
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Helper: create an AcpClient backed by in-memory buffers.
    fn mock_acp(input: &str) -> AcpClient<Cursor<Vec<u8>>, Vec<u8>> {
        let reader = Cursor::new(input.as_bytes().to_vec());
        let writer = Vec::new();
        AcpClient::new(reader, writer)
    }

    fn jsonrpc_result(id: u64, result: Value) -> String {
        format!("{}\n", json!({"jsonrpc":"2.0","id":id,"result":result}))
    }

    fn notification(method: &str, params: Value) -> String {
        format!(
            "{}\n",
            json!({"jsonrpc":"2.0","method":method,"params":params})
        )
    }

    #[test]
    fn stream_text_delta_wraps_text() {
        let mut acp = mock_acp("");
        acp.stream_text_delta("hello").unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "stream/textDelta");
        assert_eq!(parsed["params"]["text"], "hello");
    }

    #[test]
    fn stream_activity_wraps_as_log_kind() {
        let mut acp = mock_acp("");
        acp.stream_activity("working").unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "stream/activity");
        assert_eq!(parsed["params"]["kind"], "log");
        assert_eq!(parsed["params"]["data"], "working");
    }

    #[test]
    fn heartbeat_is_notification() {
        let mut acp = mock_acp("");
        acp.heartbeat().unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "heartbeat");
        assert!(parsed.get("id").is_none() || parsed["id"].is_null());
    }

    #[test]
    fn quota_check_returns_allowed_flag() {
        let input = jsonrpc_result(1, json!({"allowed": true}));
        let mut acp = mock_acp(&input);
        assert!(acp.quota_check().unwrap());
    }

    #[test]
    fn quota_check_defaults_to_allowed_on_missing_field() {
        let input = jsonrpc_result(1, json!({}));
        let mut acp = mock_acp(&input);
        assert!(acp.quota_check().unwrap());
    }

    #[test]
    fn quota_check_denied_surfaces_false() {
        let input = jsonrpc_result(1, json!({"allowed": false}));
        let mut acp = mock_acp(&input);
        assert!(!acp.quota_check().unwrap());
    }

    #[test]
    fn initialize_parses_broker_response_shape() {
        let init_result = json!({
            "system_prompt": "You are helpful.",
            "messages": [],
            "tools_config": {}
        });
        let input = jsonrpc_result(1, init_result);
        let mut acp = mock_acp(&input);
        let result = acp.initialize().unwrap();
        assert_eq!(result.system_prompt, "You are helpful.");
        assert!(result.messages.is_empty());
    }

    #[test]
    fn initialize_tolerates_missing_optional_fields() {
        let input = jsonrpc_result(1, json!({}));
        let mut acp = mock_acp(&input);
        let result = acp.initialize().unwrap();
        assert!(result.system_prompt.is_empty());
        assert!(result.messages.is_empty());
    }

    #[test]
    fn next_push_surfaces_session_message_as_user_message() {
        let input = notification("session/message", json!({"role": "user", "content": "hi"}));
        let mut acp = mock_acp(&input);
        match acp.next_push().unwrap() {
            NextPush::Message(m) => {
                assert_eq!(m.role, Role::User);
                assert_eq!(m.content.as_ref().and_then(|c| c.as_text()), Some("hi"));
            }
            NextPush::Cancel => panic!("expected Message, got Cancel"),
        }
    }

    #[test]
    fn next_push_surfaces_session_cancel_as_cancel_sentinel() {
        let input = notification("session/cancel", json!({}));
        let mut acp = mock_acp(&input);
        assert!(matches!(acp.next_push().unwrap(), NextPush::Cancel));
    }

    #[test]
    fn next_push_parses_content_blocks() {
        let input = notification(
            "session/message",
            json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "what's this?" },
                    { "type": "image_url", "image_url": { "url": "https://x/y.png" } }
                ]
            }),
        );
        let mut acp = mock_acp(&input);
        let msg = match acp.next_push().unwrap() {
            NextPush::Message(m) => m,
            NextPush::Cancel => panic!("expected Message"),
        };
        assert_eq!(msg.role, Role::User);
        match msg.content.as_ref().expect("content") {
            Content::Blocks(blocks) => assert_eq!(blocks.len(), 2),
            Content::Text(_) => panic!("expected Blocks"),
        }
    }

    #[test]
    fn next_push_skips_unknown_notifications_and_continues() {
        let input = format!(
            "{}{}",
            notification("stream/unknown", json!({})),
            notification("session/message", json!({"role": "user", "content": "ok"})),
        );
        let mut acp = mock_acp(&input);
        match acp.next_push().unwrap() {
            NextPush::Message(m) => {
                assert_eq!(m.content.as_ref().and_then(|c| c.as_text()), Some("ok"));
            }
            NextPush::Cancel => panic!("expected Message"),
        }
    }

    #[test]
    fn next_push_rejects_unknown_role() {
        // An unknown role is caught at the `SessionMessageParams`
        // deserialization boundary by the protocol crate's typed
        // `Role` enum, so we just assert the call fails.
        let input = notification("session/message", json!({"role": "alien", "content": "hi"}));
        let mut acp = mock_acp(&input);
        let err = acp.next_push().unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("session/message") || msg.contains("variant"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn next_push_errors_on_eof() {
        let mut acp = mock_acp("");
        let err = acp.next_push().unwrap_err();
        assert!(err.to_string().contains("stdin closed"));
    }

    #[test]
    fn turn_end_wraps_messages_and_maps_usage_field_names() {
        let mut acp = mock_acp("");
        let messages = vec![Message {
            role: Role::Assistant,
            content: Some(Content::Text("done".into())),
            tool_calls: None,
            tool_call_id: None,
        }];
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        acp.turn_end(&messages, &usage).unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "turn/end");
        assert!(parsed.get("id").is_none() || parsed["id"].is_null());
        assert_eq!(parsed["params"]["usage"]["input_tokens"], 100);
        assert_eq!(parsed["params"]["usage"]["output_tokens"], 50);
        assert_eq!(parsed["params"]["messages"][0]["role"], "assistant");
    }

    #[test]
    fn quota_update_sends_request_with_tokens() {
        let input = jsonrpc_result(1, json!({}));
        let mut acp = mock_acp(&input);
        acp.quota_update(100, 50).unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "quota/update");
        assert_eq!(parsed["params"]["input_tokens"], 100);
        assert_eq!(parsed["params"]["output_tokens"], 50);
    }

    #[test]
    fn send_request_surfaces_error_from_peer() {
        let input = format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"bad"}})
        );
        let mut acp = mock_acp(&input);
        let err = acp.quota_check().unwrap_err();
        assert!(err.to_string().contains("bad"));
    }
}
