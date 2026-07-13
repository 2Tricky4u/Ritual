//! Parser for `claude -p ... --output-format stream-json --verbose` lines.
//! Field names verified against live captures of Claude Code 2.1.205
//! (tests/fixtures/*.jsonl). Drift-tolerant: unknown shapes become Raw.

use serde_json::Value;

use crate::history::{RateLimitInfo, Usage};
use crate::runner::events::{AgentEvent, summarize};

/// One stdout line -> zero or more events. Non-JSON lines become Text
/// (stdout bleed happens in practice and must not kill the stream).
pub fn parse_line(line: &str) -> Vec<AgentEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return vec![];
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return vec![AgentEvent::Text {
            text: trimmed.to_string(),
        }];
    };
    parse_value(value)
}

fn parse_value(value: Value) -> Vec<AgentEvent> {
    match value.get("type").and_then(Value::as_str) {
        Some("system") => parse_system(value),
        Some("assistant") => parse_message_blocks(&value),
        Some("user") => parse_tool_results(&value),
        Some("rate_limit_event") => parse_rate_limit(&value),
        Some("result") => parse_result(&value),
        _ => vec![AgentEvent::Raw { value }],
    }
}

fn parse_system(value: Value) -> Vec<AgentEvent> {
    if value.get("subtype").and_then(Value::as_str) != Some("init") {
        // hook_started / hook_response / model_refusal_fallback / future ones
        return vec![AgentEvent::Raw { value }];
    }
    let mcp_servers = value
        .get("mcp_servers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    (
                        str_field(s, "name").unwrap_or_default(),
                        str_field(s, "status").unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    vec![AgentEvent::SessionStart {
        session_id: str_field(&value, "session_id").unwrap_or_default(),
        model: str_field(&value, "model").unwrap_or_default(),
        mcp_servers,
    }]
}

fn parse_message_blocks(value: &Value) -> Vec<AgentEvent> {
    let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else {
        return vec![AgentEvent::Raw {
            value: value.clone(),
        }];
    };
    blocks
        .iter()
        .map(|b| match b.get("type").and_then(Value::as_str) {
            Some("text") => AgentEvent::Text {
                text: str_field(b, "text").unwrap_or_default(),
            },
            Some("thinking") => AgentEvent::Thinking {
                text: str_field(b, "thinking").unwrap_or_default(),
            },
            Some("tool_use") => AgentEvent::ToolUse {
                name: str_field(b, "name").unwrap_or_default(),
                summary: b
                    .get("input")
                    .map(|i| summarize(i, 100))
                    .unwrap_or_default(),
            },
            _ => AgentEvent::Raw { value: b.clone() },
        })
        .collect()
}

fn parse_tool_results(value: &Value) -> Vec<AgentEvent> {
    let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else {
        // e.g. a plain-string user message; not interesting enough to fail on
        return vec![AgentEvent::Raw {
            value: value.clone(),
        }];
    };
    blocks
        .iter()
        .filter_map(|b| {
            if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                Some(AgentEvent::ToolResult {
                    is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    summary: b
                        .get("content")
                        .map(|c| summarize(c, 120))
                        .unwrap_or_default(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn parse_rate_limit(value: &Value) -> Vec<AgentEvent> {
    let info = value.get("rate_limit_info");
    vec![AgentEvent::RateLimit(RateLimitInfo {
        resets_at: info.and_then(|i| i.get("resetsAt")).and_then(Value::as_i64),
        kind: info.and_then(|i| str_field(i, "rateLimitType")),
        status: info.and_then(|i| str_field(i, "status")),
    })]
}

fn parse_result(value: &Value) -> Vec<AgentEvent> {
    let usage = value.get("usage").map(|u| Usage {
        input_tokens: u64_field(u, "input_tokens"),
        output_tokens: u64_field(u, "output_tokens"),
        cache_read_input_tokens: u64_field(u, "cache_read_input_tokens"),
        cache_creation_input_tokens: u64_field(u, "cache_creation_input_tokens"),
    });
    vec![AgentEvent::Completed {
        ok: !value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        result_text: str_field(value, "result"),
        total_cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
        usage,
        num_turns: value
            .get("num_turns")
            .and_then(Value::as_u64)
            .map(|n| n as u32),
        duration_ms: value.get("duration_ms").and_then(Value::as_u64),
        permission_denials: value
            .get("permission_denials")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }]
}

pub fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

pub fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Session id extraction for `claude --resume` takeover.
#[allow(dead_code)] // used by the TUI takeover key (M3)
pub fn session_id(ev: &AgentEvent) -> Option<&str> {
    match ev {
        AgentEvent::SessionStart { session_id, .. } if !session_id.is_empty() => Some(session_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<AgentEvent> {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        text.lines().flat_map(parse_line).collect()
    }

    #[test]
    fn parses_toolrich_fixture() {
        let events = fixture("claude_toolrich.jsonl");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SessionStart { model, .. } if !model.is_empty()))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Thinking { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolUse { name, .. } if name == "Bash"))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult {
                is_error: false,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::RateLimit(_))));
        let completed = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Completed {
                    ok,
                    total_cost_usd,
                    usage,
                    ..
                } => Some((*ok, *total_cost_usd, usage.clone())),
                _ => None,
            })
            .expect("result event");
        assert!(completed.0);
        assert!(completed.1.unwrap() > 0.0);
        assert!(completed.2.unwrap().output_tokens > 0);
    }

    #[test]
    fn minimal_fixture_has_session_and_result() {
        let events = fixture("claude_minimal.jsonl");
        assert!(events.iter().any(|e| session_id(e).is_some()));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Completed { ok: true, .. }))
        );
    }

    #[test]
    fn garbage_never_panics_and_falls_back() {
        let events = fixture("garbage.jsonl");
        // Non-JSON lines -> Text; unknown types / unknown blocks -> Raw;
        // final result still parses.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Text { text } if text.contains("not json")))
        );
        assert!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Raw { .. }))
                .count()
                >= 2
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Completed { ok: true, .. }))
        );
    }

    #[test]
    fn empty_and_whitespace_lines_are_skipped() {
        assert!(parse_line("").is_empty());
        assert!(parse_line("   ").is_empty());
    }

    #[test]
    fn failure_fixture_covers_every_error_arm() {
        let events = fixture("claude_failure.jsonl");

        // init carries the MCP server list, statuses included.
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStart { mcp_servers, .. }
                if mcp_servers.contains(&("codex".into(), "connected".into()))
                    && mcp_servers.contains(&("pal".into(), "failed".into()))
        )));
        // A failing tool_result keeps its error flag and content.
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult { is_error: true, summary } if summary.contains("exit 101")
        )));
        // A rejected rate limit parses fully; one with no info yields Nones.
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::RateLimit(i)
                if i.status.as_deref() == Some("rejected")
                    && i.kind.as_deref() == Some("five_hour")
                    && i.resets_at == Some(1760000000)
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::RateLimit(i)
                if i.status.is_none() && i.kind.is_none() && i.resets_at.is_none()
        )));
        // Non-init system, assistant without /message/content, and a
        // plain-string user message all degrade to Raw, never a panic.
        assert!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Raw { .. }))
                .count()
                >= 3
        );
        // The failed result: ok=false, text kept, denials kept, no usage.
        let (ok, text, denials, usage) = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Completed {
                    ok,
                    result_text,
                    permission_denials,
                    usage,
                    ..
                } => Some((
                    *ok,
                    result_text.clone(),
                    permission_denials.len(),
                    usage.clone(),
                )),
                _ => None,
            })
            .expect("result event");
        assert!(!ok);
        assert!(text.unwrap().contains("budget exceeded"));
        assert_eq!(denials, 1);
        assert!(usage.is_none(), "failure result carried no usage block");
    }

    #[test]
    fn empty_session_id_yields_none_and_huge_lines_survive() {
        let evs = parse_line(r#"{"type":"system","subtype":"init","session_id":"","model":"m"}"#);
        assert!(session_id(&evs[0]).is_none());

        // A pathologically long single line must parse, not truncate or die.
        let big = "x".repeat(70_000);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{big}"}}]}}}}"#
        );
        let evs = parse_line(&line);
        assert!(matches!(&evs[0], AgentEvent::Text { text } if text.len() == 70_000));
    }
}
