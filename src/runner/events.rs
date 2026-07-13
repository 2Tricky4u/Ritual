use serde_json::Value;

use crate::history::{RateLimitInfo, Usage};

/// Unified event stream from any agent (claude stream-json, codex exec
/// --json, or a check.sh run). Anything unrecognized becomes `Raw`; the
/// parsers must never error on schema drift.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionStart {
        session_id: String,
        model: String,
        mcp_servers: Vec<(String, String)>,
    },
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolUse {
        name: String,
        summary: String,
    },
    ToolResult {
        is_error: bool,
        summary: String,
    },
    RateLimit(RateLimitInfo),
    Completed {
        ok: bool,
        result_text: Option<String>,
        /// Machine failure class on error (claude result `subtype`, e.g.
        /// "error_max_budget_usd"; codex `error.kind`). None on success.
        error_subtype: Option<String>,
        total_cost_usd: Option<f64>,
        usage: Option<Usage>,
        num_turns: Option<u32>,
        duration_ms: Option<u64>,
        permission_denials: Vec<Value>,
    },
    /// A line on the child's stderr (hook noise, warnings, real errors).
    Stderr {
        line: String,
    },
    /// Unrecognized JSON event, rendered dimmed, never dropped.
    Raw {
        value: Value,
    },
}

/// Compact single-line summary of a JSON value (tool inputs, results).
pub fn summarize(value: &Value, max: usize) -> String {
    let s = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let s = s.replace('\n', " ⏎ ");
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_values_lose_their_quotes() {
        // A JSON string is summarized as its content, not its quoted form.
        assert_eq!(summarize(&json!("hello"), 80), "hello");
    }

    #[test]
    fn non_string_values_stringify() {
        assert_eq!(summarize(&json!({"a": 1}), 80), r#"{"a":1}"#);
        assert_eq!(summarize(&json!([1, 2, 3]), 80), "[1,2,3]");
    }

    #[test]
    fn newlines_become_return_glyphs_not_literal_breaks() {
        let out = summarize(&json!("line1\nline2"), 80);
        assert_eq!(out, "line1 ⏎ line2");
        assert!(!out.contains('\n'));
    }

    #[test]
    fn overlong_input_is_truncated_with_ellipsis() {
        let long = "x".repeat(200);
        let out = summarize(&json!(long), 10);
        // 10 kept chars + the ellipsis marker.
        assert_eq!(out.chars().count(), 11);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn exactly_max_is_not_truncated() {
        let s = "1234567890";
        let out = summarize(&json!(s), 10);
        assert_eq!(out, s);
        assert!(!out.ends_with('…'));
    }

    #[test]
    fn truncation_counts_chars_not_bytes() {
        // Multi-byte chars must not be split mid-codepoint or miscounted.
        let out = summarize(&json!("ααααα"), 3);
        assert_eq!(out, "ααα…");
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

        #[test]
        fn summarize_is_bounded_and_single_line(s in "\\PC{0,300}", max in 1usize..64) {
            let out = summarize(&json!(s), max);
            proptest::prop_assert!(out.chars().count() <= max + 1, "{}", out);
            proptest::prop_assert!(!out.contains('\n'));
        }
    }

    #[test]
    fn summarize_flattens_deeply_nested_structures() {
        let v = json!({"a": {"b": [1, {"c": "line\nbreak"}]}, "e": null});
        let out = summarize(&v, 200);
        // Nested values arrive JSON-escaped, never a literal newline.
        assert!(!out.contains('\n'), "always a single line: {out}");
        assert!(out.contains("\"c\""));
        // A TOP-LEVEL string keeps its newlines -> visible ⏎ markers.
        assert!(summarize(&json!("a\nb"), 50).contains("⏎"));
        let capped = summarize(&v, 10);
        assert_eq!(capped.chars().count(), 11, "10 chars + ellipsis");
        assert!(capped.ends_with('…'));
    }
}
