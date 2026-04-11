use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::compaction::{compact_messages, estimate_tokens};
use crate::llm::{Content, Message, Role, StopReason, Usage};
use crate::tools::ToolRegistry;
use crate::traits::{AcpService, LlmService};

const CONTEXT_WINDOW: usize = 128_000;
const COMPACTION_THRESHOLD: f64 = 0.80;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const WIND_DOWN_ITERS: usize = 5;

const SILENCE_NUDGE: &str = "You haven't produced any output. Please respond to the user \
     or explain what you're working on.";

pub struct LoopConfig {
    pub max_iterations: usize,
    pub timeout: Duration,
}

pub async fn run(
    acp: &mut impl AcpService,
    llm: &(impl LlmService + ?Sized),
    registry: &mut ToolRegistry,
    messages: &mut Vec<Message>,
    config: &LoopConfig,
) -> Result<()> {
    let start = Instant::now();
    let mut total_usage = Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    };
    let mut silent_turns = 0u32;
    let mut last_heartbeat = Instant::now();

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
        let streamed = llm
            .stream_completion(messages, &tools, &mut |text| {
                text_deltas.push(text.to_string());
            })
            .await;

        // Stream collected text to ACP
        for delta in &text_deltas {
            let _ = acp.stream_text_delta(delta);
        }

        let streamed = match streamed {
            Ok(s) => s,
            Err(e) => {
                error!("LLM call failed: {}", e);
                acp.stream_text_delta(&format!("\n\n[LLM error: {}]", e))?;
                break;
            }
        };

        // Track usage
        if let Some(usage) = &streamed.usage {
            total_usage.prompt_tokens += usage.prompt_tokens;
            total_usage.completion_tokens += usage.completion_tokens;
            total_usage.total_tokens += usage.total_tokens;
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
                let args: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);

                // Human-readable activity log — kept for back-compat
                // with consumers that only watch stream/activity.
                let _ = acp.stream_activity(&format!("Running tool: {}", name));

                // Machine-readable observability: stream/toolCall
                // fires BEFORE the invocation, stream/toolResult
                // fires AFTER, both echoing the same tool-call id.
                let _ = acp.stream_tool_call(&tc.id, name, &args);

                let result = registry.execute(name, args).await;

                let (content, is_error) = match result {
                    Ok(output) => (output, false),
                    Err(err) => (format!("Error: {}", err), true),
                };

                let _ = acp.stream_tool_result(
                    &tc.id,
                    &Value::String(content.clone()),
                    is_error,
                );

                messages.push(Message {
                    role: Role::Tool,
                    content: Some(Content::Text(content)),
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

    Ok(())
}

fn system_msg(text: &str) -> Message {
    Message {
        role: Role::System,
        content: Some(Content::Text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}
