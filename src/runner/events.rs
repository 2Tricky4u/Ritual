use serde_json::Value;

use crate::history::{RateLimitInfo, Usage};

/// Unified event stream from any agent (claude stream-json, codex exec
/// --json, or a check.sh run). Anything unrecognized becomes `Raw` — the
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
    /// Unrecognized JSON event — rendered dimmed, never dropped.
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
