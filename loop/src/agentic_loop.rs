use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::compaction::{compact_messages, estimate_tokens};
use crate::llm::{
    parse_tool_arguments, Content, ContentBlock, Message, ParsedArguments, Role, StopReason,
    ToolContent, TypedBlock, Usage,
};
use crate::observability::{
    build_context_snapshot, GenerationParams, SessionTrace, ToolSpanParams,
};
use crate::tools::{ToolOutput, ToolRegistry};
use crate::traits::{AcpService, LlmService};

/// Maximum characters captured for tool input/output in a Langfuse span.
const LF_TRUNCATE: usize = 4096;

const CONTEXT_WINDOW: usize = 128_000;
const COMPACTION_THRESHOLD: f64 = 0.80;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const WIND_DOWN_ITERS: usize = 5;

const SILENCE_NUDGE: &str = "You haven't produced any output. Please respond to the user \
     or explain what you're working on.";

pub struct LoopConfig {
    pub max_iterations: usize,
    pub timeout: Duration,
    /// Model name recorded on Langfuse generation spans. Free-form label;
    /// the trace is untouched when the `SessionTrace` is disabled.
    pub model: String,
    /// When true, attach a redacted chat-log snapshot to Langfuse
    /// generation `input`. Off by default.
    pub langfuse_capture_input: bool,
}

pub async fn run(
    acp: &mut impl AcpService,
    llm: &(impl LlmService + ?Sized),
    registry: &mut ToolRegistry,
    messages: &mut Vec<Message>,
    config: &LoopConfig,
    trace: &SessionTrace,
) -> Result<()> {
    let start = Instant::now();
    let mut total_usage = Usage::default();
    let mut silent_turns = 0u32;
    let mut last_heartbeat = Instant::now();
    let mut response_text = String::new();
    let mut tool_count: usize = 0;

    for iteration in 0..config.max_iterations {
        // Timeout check
        if start.elapsed() >= config.timeout {
            warn!("Loop timeout reached after {:?}", config.timeout);
            acp.stream_text_delta("\n\n[Session timed out]")?;
            break;
        }

        // Heartbeat
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            let _ = acp.heartbeat();
            last_heartbeat = Instant::now();
        }

        // Quota check
        if !acp.quota_check().unwrap_or(true) {
            acp.stream_text_delta("\n\n[Quota exhausted]")?;
            break;
        }

        // Compaction check
        let est_tokens = estimate_tokens(messages);
        if est_tokens as f64 > CONTEXT_WINDOW as f64 * COMPACTION_THRESHOLD {
            info!(
                "Context at ~{}% — compacting",
                est_tokens * 100 / CONTEXT_WINDOW
            );
            *messages = compact_messages(llm, messages, 10).await?;
        }

        // Wind-down warning
        let remaining = config.max_iterations - iteration;
        if remaining == WIND_DOWN_ITERS {
            messages.push(system_msg(&format!(
                "[System: {} iterations remaining. Wrap up your work.]",
                remaining
            )));
        }

        // Silence nudge
        if silent_turns >= 3 {
            messages.push(system_msg(SILENCE_NUDGE));
            silent_turns = 0;
        }

        // Inject loop status
        let status = format!(
            "[iteration {}/{}, elapsed {:?}]",
            iteration + 1,
            config.max_iterations,
            start.elapsed()
        );
        info!("{}", status);

        // LLM call — collect text deltas, stream to ACP after
        let mut text_deltas = Vec::new();
        let tools = registry.definitions();
        let message_count = messages.len();
        // Pre-build the input preview once per LLM call so both the
        // error and success branches below share it without cloning
        // messages twice. Cheap when the flag is off (one `json!`
        // literal); only touches message content when opted in.
        let input_preview = if config.langfuse_capture_input {
            build_context_snapshot(messages)
        } else {
            json!({"message_count": message_count})
        };
        let lf_start = Utc::now();
        let streamed = llm
            .stream_completion(messages, &tools, &mut |text| {
                text_deltas.push(text.to_string());
            })
            .await;
        let lf_end = Utc::now();

        // Stream collected text to ACP (skip empty deltas the LLM
        // sends as SSE keepalives)
        for delta in &text_deltas {
            if !delta.is_empty() {
                let _ = acp.stream_text_delta(delta);
            }
        }

        let streamed = match streamed {
            Ok(s) => s,
            Err(e) => {
                error!("LLM call failed: {}", e);
                // `{e:#}` walks anyhow's source chain, so whatever the
                // LLM client wraps (plain string, classified enum, etc.)
                // we surface the full context in Langfuse rather than
                // just the outermost message.
                let status_message = format!("{e:#}");
                trace.record_generation(GenerationParams {
                    model: config.model.clone(),
                    message_count,
                    input_preview: input_preview.clone(),
                    output_text: None,
                    stop_reason: "error".into(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    cost: 0.0,
                    start_time: lf_start,
                    end_time: lf_end,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    level: Some("ERROR".into()),
                    status_message: Some(status_message),
                });
                acp.stream_text_delta(&format!("\n\n[LLM error: {}]", e))?;
                break;
            }
        };

        // Record successful generation on Langfuse.
        let streamed_text: String = text_deltas.join("");
        let stop_reason_str = match &streamed.finish_reason {
            Some(StopReason::Stop) => "stop",
            Some(StopReason::ToolCalls) => "tool_calls",
            Some(StopReason::Length) => "length",
            Some(StopReason::ContentFilter) => "content_filter",
            None => "unknown",
        };
        trace.record_generation(GenerationParams {
            model: config.model.clone(),
            message_count,
            input_preview,
            output_text: (!streamed_text.is_empty()).then(|| streamed_text.clone()),
            stop_reason: stop_reason_str.into(),
            prompt_tokens: streamed
                .usage
                .as_ref()
                .map(|u| u.prompt_tokens)
                .unwrap_or(0),
            completion_tokens: streamed
                .usage
                .as_ref()
                .map(|u| u.completion_tokens)
                .unwrap_or(0),
            cost: streamed.usage.as_ref().map(|u| u.cost).unwrap_or(0.0),
            start_time: lf_start,
            end_time: lf_end,
            cache_read_tokens: streamed
                .usage
                .as_ref()
                .map(|u| u.cache_read_tokens)
                .unwrap_or(0),
            cache_creation_tokens: streamed
                .usage
                .as_ref()
                .map(|u| u.cache_creation_tokens)
                .unwrap_or(0),
            level: None,
            status_message: None,
        });
        if !streamed_text.is_empty() {
            response_text.push_str(&streamed_text);
        }

        // Track usage
        if let Some(usage) = &streamed.usage {
            total_usage.prompt_tokens += usage.prompt_tokens;
            total_usage.completion_tokens += usage.completion_tokens;
            total_usage.total_tokens += usage.total_tokens;
            total_usage.cost += usage.cost;
            total_usage.cache_read_tokens += usage.cache_read_tokens;
            total_usage.cache_creation_tokens += usage.cache_creation_tokens;
            let _ = acp.quota_update(usage.prompt_tokens, usage.completion_tokens);
        }

        // Track silence
        let has_content = streamed.message.content.is_some();
        let has_tools = streamed.message.tool_calls.is_some();

        if has_content || has_tools {
            silent_turns = 0;
        } else {
            silent_turns += 1;
        }

        // Append assistant message
        messages.push(streamed.message.clone());

        // Handle tool calls
        if let Some(tool_calls) = &streamed.message.tool_calls {
            for tc in tool_calls {
                let name = &tc.function.name;

                let args: Value = match parse_tool_arguments(name, &tc.function.arguments) {
                    ParsedArguments::Ok(v) => v,
                    ParsedArguments::Failed { error_message, .. } => {
                        // Arguments were truncated by streaming — skip
                        // execution and feed an actionable error back
                        // to the LLM so it can retry with smaller input
                        // instead of silently getting `{}` and looping.
                        let _ = acp.stream_activity(&format!("Invalid tool arguments: {}", name));
                        let _ = acp.stream_tool_call(&tc.id, name, &Value::Null);
                        let content = Content::Text(error_message);
                        let content_value = serde_json::to_value(&content).unwrap_or(Value::Null);
                        let _ = acp.stream_tool_result(&tc.id, &content_value, true);
                        messages.push(Message {
                            role: Role::Tool,
                            content: Some(content),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                        });
                        continue;
                    }
                };

                // Human-readable activity log — kept for back-compat
                // with consumers that only watch stream/activity.
                let _ = acp.stream_activity(&format!("Running tool: {}", name));

                // Machine-readable observability: stream/toolCall
                // fires BEFORE the invocation, stream/toolResult
                // fires AFTER, both echoing the same tool-call id.
                let _ = acp.stream_tool_call(&tc.id, name, &args);

                let tool_start = Utc::now();
                let result = if registry.is_acp_tool(name) {
                    // Route through ACP tunnel to operator's ToolHost.
                    match acp.tools_call(name, args.clone()) {
                        Ok(value) => extract_acp_tool_result(&value),
                        Err(e) => Err(e.to_string()),
                    }
                } else {
                    registry.execute(name, args.clone()).await
                };
                let tool_end = Utc::now();

                let (content, is_error) = match result {
                    Ok(output) => (tool_output_to_content(output), false),
                    Err(err) => (Content::Text(format!("Error: {}", err)), true),
                };

                let content_value = serde_json::to_value(&content).unwrap_or(Value::Null);
                trace.record_tool_span(ToolSpanParams {
                    name: name.clone(),
                    input: truncate_for_trace(&args.to_string()),
                    output: truncate_for_trace(&content_value.to_string()),
                    is_error,
                    start_time: tool_start,
                    end_time: tool_end,
                });
                tool_count += 1;
                let _ = acp.stream_tool_result(&tc.id, &content_value, is_error);

                messages.push(Message {
                    role: Role::Tool,
                    content: Some(content),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }
            continue; // Loop back for the LLM to process tool results
        }

        // No tool calls — check stop reason
        match &streamed.finish_reason {
            Some(StopReason::Stop) | None => {
                info!("Loop completed: natural stop");
                break;
            }
            Some(StopReason::Length) => {
                warn!("Hit context length limit");
                acp.stream_text_delta("\n\n[Response truncated due to length limit]")?;
                break;
            }
            Some(StopReason::ContentFilter) => {
                warn!("Content filter triggered");
                acp.stream_text_delta("\n\n[Content filtered]")?;
                break;
            }
            Some(StopReason::ToolCalls) => {
                // Should have been caught above, but continue just in case
                continue;
            }
        }
    }

    // Emit the fire-and-forget turn/end notification. The broker
    // fans it out to SSE subscribers; the operator's backend is
    // responsible for persisting the data.
    if let Err(e) = acp.turn_end(messages, &total_usage) {
        error!("Failed to emit turn/end: {}", e);
    }

    trace.finalize(
        total_usage.total_tokens,
        total_usage.cost,
        tool_count,
        &response_text,
    );

    Ok(())
}

fn truncate_for_trace(s: &str) -> String {
    if s.len() <= LF_TRUNCATE {
        return s.to_string();
    }
    let mut end = LF_TRUNCATE;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("…[truncated]");
    out
}

fn system_msg(text: &str) -> Message {
    Message {
        role: Role::System,
        content: Some(Content::Text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Convert a [`ToolOutput`] into the LLM-facing [`Content`] shape.
/// Base64 images are re-encoded as OpenAI-compatible `image_url`
/// blocks with `data:` URIs so they work with OpenRouter.
fn tool_output_to_content(output: ToolOutput) -> Content {
    match output {
        ToolOutput::Text(s) => Content::Text(s),
        ToolOutput::Blocks(blocks) => {
            let content_blocks: Vec<TypedBlock> = blocks
                .into_iter()
                .map(|b| match b {
                    ToolContent::Text(t) => TypedBlock::Known(ContentBlock::Text { text: t }),
                    ToolContent::ImageBase64 { data, mime_type } => {
                        TypedBlock::Known(ContentBlock::ImageUrl {
                            image_url: serde_json::json!({
                                "url": format!("data:{};base64,{}", mime_type, data)
                            }),
                        })
                    }
                })
                .collect();
            Content::Blocks(content_blocks)
        }
    }
}

/// Extract a result from an ACP `tools/call` response, returning a
/// [`ToolOutput`] compatible with the multimodal tool pipeline.
///
/// Supports MCP-style `{"content": [{"type":"text","text":"..."}], "isError": bool}`
/// as well as opaque JSON (stringified as fallback).
fn extract_acp_tool_result(value: &Value) -> Result<ToolOutput, String> {
    let is_error = value
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Try MCP-style content array first.
    if let Some(content) = value.get("content").and_then(|c| c.as_array()) {
        let text: String = content
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return if is_error {
                Err(text)
            } else {
                Ok(ToolOutput::Text(text))
            };
        }
    }

    // Fallback: stringify the whole value.
    let text = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    if is_error {
        Err(text)
    } else {
        Ok(ToolOutput::Text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── tool_output_to_content tests (multimodal) ──

    #[test]
    fn tool_output_text_converts_to_content_text() {
        let output = ToolOutput::Text("hello".into());
        let content = tool_output_to_content(output);
        assert!(matches!(content, Content::Text(ref s) if s == "hello"));
    }

    #[test]
    fn tool_output_blocks_text_only() {
        let output = ToolOutput::Blocks(vec![
            ToolContent::Text("line 1".into()),
            ToolContent::Text("line 2".into()),
        ]);
        let content = tool_output_to_content(output);
        match content {
            Content::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].as_text(), Some("line 1"));
                assert_eq!(blocks[1].as_text(), Some("line 2"));
            }
            _ => panic!("expected Blocks"),
        }
    }

    #[test]
    fn tool_output_blocks_with_image_produces_data_uri() {
        let output = ToolOutput::Blocks(vec![
            ToolContent::Text("screenshot:".into()),
            ToolContent::ImageBase64 {
                data: "abc123".into(),
                mime_type: "image/png".into(),
            },
        ]);
        let content = tool_output_to_content(output);
        match content {
            Content::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].as_text(), Some("screenshot:"));
                match &blocks[1] {
                    TypedBlock::Known(ContentBlock::ImageUrl { image_url }) => {
                        let url = image_url["url"].as_str().unwrap();
                        assert_eq!(url, "data:image/png;base64,abc123");
                    }
                    other => panic!("expected Known(ImageUrl), got {:?}", other),
                }
            }
            _ => panic!("expected Blocks"),
        }
    }

    #[test]
    fn tool_output_empty_blocks() {
        let output = ToolOutput::Blocks(vec![]);
        let content = tool_output_to_content(output);
        match content {
            Content::Blocks(blocks) => assert!(blocks.is_empty()),
            _ => panic!("expected empty Blocks"),
        }
    }

    // ── extract_acp_tool_result tests ──

    #[test]
    fn extract_mcp_style_text_content() {
        let value = json!({
            "content": [{"type": "text", "text": "sunny, 22C"}],
            "isError": false
        });
        match extract_acp_tool_result(&value) {
            Ok(ToolOutput::Text(s)) => assert_eq!(s, "sunny, 22C"),
            other => panic!("expected Ok(Text), got {other:?}"),
        }
    }

    #[test]
    fn extract_mcp_style_error() {
        let value = json!({
            "content": [{"type": "text", "text": "city not found"}],
            "isError": true
        });
        match extract_acp_tool_result(&value) {
            Err(msg) => assert_eq!(msg, "city not found"),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn extract_multi_block_joins_text() {
        let value = json!({
            "content": [
                {"type": "text", "text": "line1"},
                {"type": "image", "data": "..."},
                {"type": "text", "text": "line2"}
            ]
        });
        match extract_acp_tool_result(&value) {
            Ok(ToolOutput::Text(s)) => assert_eq!(s, "line1\nline2"),
            other => panic!("expected Ok(Text), got {other:?}"),
        }
    }

    #[test]
    fn extract_fallback_stringifies_opaque_json() {
        let value = json!({"result": 42});
        match extract_acp_tool_result(&value) {
            Ok(ToolOutput::Text(s)) => assert!(s.contains("42")),
            other => panic!("expected Ok(Text), got {other:?}"),
        }
    }

    #[test]
    fn extract_fallback_with_is_error() {
        let value = json!({"detail": "bad", "isError": true});
        assert!(extract_acp_tool_result(&value).is_err());
    }

    #[test]
    fn extract_empty_content_array_falls_back() {
        let value = json!({"content": [], "isError": false});
        // Empty content array → no text blocks → fallback to stringify
        match extract_acp_tool_result(&value) {
            Ok(ToolOutput::Text(s)) => assert!(s.contains("content")),
            other => panic!("expected Ok(Text), got {other:?}"),
        }
    }
}
