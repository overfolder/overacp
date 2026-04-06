use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Read, Stdin, Stdout, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::llm::Message;
use crate::traits::AcpService;

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
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
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct InitializeResult {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub conversation_id: String,
    #[serde(default)]
    pub tools_config: Value,
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

    fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: next_id(),
            method: method.to_string(),
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

    fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notif = serde_json::json!({
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

    pub fn initialize(&mut self) -> Result<InitializeResult> {
        let result = self.send_request("initialize", None)?;
        serde_json::from_value(result).context("parse initialize result")
    }

    pub fn stream_text_delta(&mut self, text: &str) -> Result<()> {
        self.send_notification(
            "stream/textDelta",
            Some(serde_json::json!({ "delta": text })),
        )
    }

    pub fn stream_activity(&mut self, activity: &str) -> Result<()> {
        self.send_notification(
            "stream/activity",
            Some(serde_json::json!({ "activity": activity })),
        )
    }

    pub fn turn_save(&mut self, messages: &[Message], usage: &Value) -> Result<()> {
        self.send_request(
            "turn/save",
            Some(serde_json::json!({
                "messages": messages,
                "usage": usage,
            })),
        )?;
        Ok(())
    }

    pub fn quota_check(&mut self) -> Result<bool> {
        let result = self.send_request("quota/check", None)?;
        Ok(result
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(true))
    }

    pub fn quota_update(&mut self, input_tokens: u64, output_tokens: u64) -> Result<()> {
        self.send_request(
            "quota/update",
            Some(serde_json::json!({
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            })),
        )?;
        Ok(())
    }

    pub fn poll_new_messages(&mut self) -> Result<Vec<Message>> {
        let result = self.send_request("poll/newMessages", None)?;
        let messages: Vec<Message> = serde_json::from_value(result).unwrap_or_default();
        Ok(messages)
    }

    pub fn heartbeat(&mut self) -> Result<()> {
        self.send_notification("heartbeat", None)
    }

    pub fn recv_notification(&mut self) -> Result<JsonRpcNotification> {
        let mut buf = String::new();
        self.reader
            .read_line(&mut buf)
            .context("read notification")?;

        serde_json::from_str(&buf).context("parse notification")
    }
}

impl<R: Read, W: Write> AcpService for AcpClient<R, W> {
    fn stream_text_delta(&mut self, text: &str) -> Result<()> {
        self.stream_text_delta(text)
    }

    fn stream_activity(&mut self, activity: &str) -> Result<()> {
        self.stream_activity(activity)
    }

    fn turn_save(&mut self, messages: &[Message], usage: &Value) -> Result<()> {
        self.turn_save(messages, usage)
    }

    fn quota_check(&mut self) -> Result<bool> {
        self.quota_check()
    }

    fn quota_update(&mut self, input_tokens: u64, output_tokens: u64) -> Result<()> {
        self.quota_update(input_tokens, output_tokens)
    }

    fn poll_new_messages(&mut self) -> Result<Vec<Message>> {
        self.poll_new_messages()
    }

    fn heartbeat(&mut self) -> Result<()> {
        self.heartbeat()
    }
}

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
        format!(
            "{}\n",
            serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
        )
    }

    #[test]
    fn test_stream_text_delta() {
        let mut acp = mock_acp("");
        acp.stream_text_delta("hello").unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "stream/textDelta");
        assert_eq!(parsed["params"]["delta"], "hello");
    }

    #[test]
    fn test_stream_activity() {
        let mut acp = mock_acp("");
        acp.stream_activity("working").unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "stream/activity");
        assert_eq!(parsed["params"]["activity"], "working");
    }

    #[test]
    fn test_heartbeat() {
        let mut acp = mock_acp("");
        acp.heartbeat().unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "heartbeat");
    }

    #[test]
    fn test_quota_check_allowed() {
        let input = jsonrpc_result(1, serde_json::json!({"allowed": true}));
        let mut acp = mock_acp(&input);
        assert!(acp.quota_check().unwrap());
    }

    #[test]
    fn test_quota_check_denied() {
        let input = jsonrpc_result(1, serde_json::json!({"allowed": false}));
        let mut acp = mock_acp(&input);
        assert!(!acp.quota_check().unwrap());
    }

    #[test]
    fn test_poll_new_messages_empty() {
        let input = jsonrpc_result(1, serde_json::json!([]));
        let mut acp = mock_acp(&input);
        let msgs = acp.poll_new_messages().unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_initialize() {
        let init_result = serde_json::json!({
            "system_prompt": "You are helpful.",
            "messages": [],
            "conversation_id": "conv-123"
        });
        let input = jsonrpc_result(1, init_result);
        let mut acp = mock_acp(&input);
        let result = acp.initialize().unwrap();
        assert_eq!(result.system_prompt, "You are helpful.");
        assert_eq!(result.conversation_id, "conv-123");
    }

    #[test]
    fn test_recv_notification() {
        let notif = format!(
            "{}\n",
            serde_json::json!({"jsonrpc":"2.0","method":"session/message","params":{}})
        );
        let mut acp = mock_acp(&notif);
        let n = acp.recv_notification().unwrap();
        assert_eq!(n.method, "session/message");
    }

    #[test]
    fn test_send_request_error() {
        let input = format!(
            "{}\n",
            serde_json::json!({"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"bad"}})
        );
        let mut acp = mock_acp(&input);
        let err = acp.quota_check().unwrap_err();
        assert!(err.to_string().contains("bad"));
    }

    #[test]
    fn test_turn_save() {
        let input = jsonrpc_result(1, serde_json::json!({}));
        let mut acp = mock_acp(&input);
        acp.turn_save(&[], &serde_json::json!({})).unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["method"], "turn/save");
    }

    #[test]
    fn test_quota_update() {
        let input = jsonrpc_result(1, serde_json::json!({}));
        let mut acp = mock_acp(&input);
        acp.quota_update(100, 50).unwrap();

        let out = String::from_utf8(acp.writer.clone()).unwrap();
        let parsed: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["params"]["input_tokens"], 100);
        assert_eq!(parsed["params"]["output_tokens"], 50);
    }
}
