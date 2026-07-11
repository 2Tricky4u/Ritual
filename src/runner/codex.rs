//! Parser for `codex exec --json` lines. Event shapes verified against a
//! live capture (tests/fixtures/codex_exec.jsonl, Codex CLI 0.144.1).
//! Everything unrecognized falls through to Raw, so schema drift degrades to
//! dimmed raw lines instead of breaking the stream.

use serde_json::Value;

use crate::history::Usage;
use crate::runner::claude::{str_field, u64_field};
use crate::runner::events::{AgentEvent, summarize};

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
        Some("thread.started") => vec![AgentEvent::SessionStart {
            session_id: str_field(&value, "thread_id").unwrap_or_default(),
            model: str_field(&value, "model").unwrap_or_default(),
            mcp_servers: vec![],
        }],
        Some("item.started") | Some("item.completed") | Some("item.updated") => parse_item(&value),
        Some("turn.completed") => {
            let usage = value.get("usage").map(|u| Usage {
                input_tokens: u64_field(u, "input_tokens"),
                output_tokens: u64_field(u, "output_tokens"),
                cache_read_input_tokens: u64_field(u, "cached_input_tokens"),
                cache_creation_input_tokens: 0,
            });
            vec![AgentEvent::Completed {
                ok: true,
                result_text: None,
                total_cost_usd: None,
                usage,
                num_turns: None,
                duration_ms: None,
                permission_denials: vec![],
            }]
        }
        Some("turn.failed") | Some("error") => vec![AgentEvent::Completed {
            ok: false,
            result_text: value
                .get("error")
                .map(|e| summarize(e, 200))
                .or_else(|| str_field(&value, "message")),
            total_cost_usd: None,
            usage: None,
            num_turns: None,
            duration_ms: None,
            permission_denials: vec![],
        }],
        _ => vec![AgentEvent::Raw { value }],
    }
}

fn parse_item(value: &Value) -> Vec<AgentEvent> {
    // Only surface completed items to avoid duplicate lines per item.
    if value.get("type").and_then(Value::as_str) != Some("item.completed") {
        return vec![];
    }
    let Some(item) = value.get("item") else {
        return vec![AgentEvent::Raw {
            value: value.clone(),
        }];
    };
    match item
        .get("item_type")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
    {
        Some("agent_message") => vec![AgentEvent::Text {
            text: str_field(item, "text").unwrap_or_default(),
        }],
        Some("reasoning") => vec![AgentEvent::Thinking {
            text: str_field(item, "text").unwrap_or_default(),
        }],
        Some("command_execution") => vec![AgentEvent::ToolUse {
            name: "command".into(),
            summary: str_field(item, "command").unwrap_or_default(),
        }],
        Some("file_change") => vec![AgentEvent::ToolUse {
            name: "file_change".into(),
            summary: item
                .get("changes")
                .map(|c| summarize(c, 100))
                .unwrap_or_default(),
        }],
        Some("mcp_tool_call") => vec![AgentEvent::ToolUse {
            name: str_field(item, "tool").unwrap_or_else(|| "mcp".into()),
            summary: item
                .get("arguments")
                .map(|a| summarize(a, 100))
                .unwrap_or_default(),
        }],
        _ => vec![AgentEvent::Raw {
            value: value.clone(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_shapes_parse() {
        let events: Vec<_> = [
            r#"{"type":"thread.started","thread_id":"t1","model":"gpt-codex"}"#,
            r#"{"type":"item.completed","item":{"item_type":"agent_message","text":"hi"}}"#,
            r#"{"type":"item.completed","item":{"item_type":"command_execution","command":"ls"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
        ]
        .iter()
        .flat_map(|l| parse_line(l))
        .collect();
        assert!(
            matches!(&events[0], AgentEvent::SessionStart { session_id, .. } if session_id == "t1")
        );
        assert!(matches!(&events[1], AgentEvent::Text { text } if text == "hi"));
        assert!(matches!(&events[2], AgentEvent::ToolUse { summary, .. } if summary == "ls"));
        assert!(matches!(&events[3], AgentEvent::Completed { ok: true, .. }));
    }

    #[test]
    fn parses_real_captured_fixture() {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/codex_exec.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let events: Vec<_> = text.lines().flat_map(parse_line).collect();
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::SessionStart { session_id, .. } if !session_id.is_empty())
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Text { text } if text == "ok"))
        );
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::Completed { ok: true, usage: Some(u), .. } if u.output_tokens > 0)
        ));
    }

    #[test]
    fn unknown_becomes_raw_or_text() {
        assert!(matches!(
            parse_line(r#"{"type":"totally.new"}"#).as_slice(),
            [AgentEvent::Raw { .. }]
        ));
        assert!(matches!(
            parse_line("plain stderr-ish noise").as_slice(),
            [AgentEvent::Text { .. }]
        ));
    }
}
