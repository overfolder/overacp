//! Parse streamed tool-call arguments with actionable error messages.
//!
//! When SSE streaming truncates JSON arguments, instead of silently falling
//! back to `{}` (which causes "missing field" errors and retry loops), this
//! module surfaces a terse, actionable error that the LLM can act on.

use serde_json::Value;
use tracing::warn;

/// Result of attempting to parse raw tool-call arguments.
pub enum ParsedArguments {
    /// JSON parsed successfully.
    Ok(Value),
    /// JSON was malformed (typically truncated by streaming).
    Failed {
        char_count: usize,
        error_message: String,
    },
}

/// Parse raw JSON tool arguments, returning an actionable error on failure.
pub fn parse_tool_arguments(tool_name: &str, raw: &str) -> ParsedArguments {
    match serde_json::from_str(raw) {
        Ok(value) => ParsedArguments::Ok(value),
        Err(parse_err) => {
            let char_count = raw.len();
            let preview: String = raw.chars().take(200).collect();
            warn!(
                tool = %tool_name,
                arg_len = char_count,
                preview = %preview,
                error = %parse_err,
                "Failed to parse streamed tool arguments"
            );
            // `is_eof` is serde_json's signal for "input ended mid-token",
            // which is what streaming truncation looks like. Anything else
            // is a syntax error in otherwise-complete JSON — we keep a
            // size-reduction tip since oversized args correlate with both
            // failure modes, but the framing has to match reality.
            let error_message = if parse_err.is_eof() {
                let tip = truncation_tip(tool_name);
                format!(
                    "tool arguments were truncated ({char_count} chars of incomplete JSON).\n\
                     Your {tool_name} content was too large for a single tool call.\n\
                     Action: {tip}"
                )
            } else {
                let tip = truncation_tip(tool_name);
                format!(
                    "tool arguments failed to parse as JSON ({char_count} chars): {parse_err}.\n\
                     Fix the JSON syntax and retry. If the arguments are large, also: {tip}"
                )
            };
            ParsedArguments::Failed {
                char_count,
                error_message,
            }
        }
    }
}

fn truncation_tip(tool_name: &str) -> &'static str {
    match tool_name {
        "write" => "use edit to append in chunks of ~3000 chars instead of one large write",
        "edit" => "break the replacement into multiple smaller edit calls",
        _ => "reduce the size of your arguments and retry",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_json() {
        let raw = r#"{"path": "/workspace/test.txt", "content": "hello"}"#;
        let result = parse_tool_arguments("write", raw);
        assert!(matches!(result, ParsedArguments::Ok(_)));
        if let ParsedArguments::Ok(val) = result {
            assert_eq!(val["path"], "/workspace/test.txt");
        }
    }

    #[test]
    fn parse_truncated_json() {
        let raw = r#"{"path": "/workspace/test.txt", "content": "hello worl"#;
        let result = parse_tool_arguments("write", raw);
        assert!(matches!(result, ParsedArguments::Failed { .. }));
        if let ParsedArguments::Failed {
            char_count,
            error_message,
        } = result
        {
            assert_eq!(char_count, raw.len());
            assert!(error_message.contains("truncated"));
            assert!(error_message.contains("write"));
            assert!(error_message.contains("edit to append"));
        }
    }

    #[test]
    fn parse_empty_string() {
        let result = parse_tool_arguments("exec", "");
        assert!(matches!(result, ParsedArguments::Failed { .. }));
    }

    #[test]
    fn parse_syntax_error_not_truncation() {
        // Complete JSON-ish input with a syntax error — not EOF. The
        // error message must not mislead the LLM into thinking the
        // input was truncated.
        let raw = r#"{"path": "a", "content": not_quoted}"#;
        let result = parse_tool_arguments("write", raw);
        let ParsedArguments::Failed { error_message, .. } = result else {
            panic!("expected Failed");
        };
        assert!(!error_message.contains("truncated"));
        assert!(error_message.contains("failed to parse as JSON"));
    }

    #[test]
    fn truncation_tip_write() {
        assert!(truncation_tip("write").contains("edit to append"));
    }

    #[test]
    fn truncation_tip_edit() {
        assert!(truncation_tip("edit").contains("smaller edit calls"));
    }

    #[test]
    fn truncation_tip_other() {
        assert!(truncation_tip("exec").contains("reduce the size"));
    }
}
