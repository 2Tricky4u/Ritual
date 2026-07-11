//! `ritual export` — OTLP-shaped JSON lines from run metas, one span per
//! run. The cheapest honest OpenTelemetry integration: pipe the output into
//! any OTLP-JSON-aware collector; no live exporter dependency in the binary.

use anyhow::Result;

use crate::history;
use crate::provenance::sha256_hex;
use crate::state::RitualDirs;

pub fn otlp_json(dirs: &RitualDirs, out: Option<&std::path::Path>) -> Result<()> {
    let metas = history::load_all(&dirs.runs_dir())?;
    let mut lines = Vec::new();
    for m in &metas {
        let trace_id = &sha256_hex(m.run_id.as_bytes())[..32];
        let span_id = &sha256_hex(m.run_id.as_bytes())[32..48];
        let start_ns = m
            .started_at
            .map(|t| t.timestamp_nanos_opt().unwrap_or(0))
            .unwrap_or(0);
        let end_ns = m
            .finished_at
            .map(|t| t.timestamp_nanos_opt().unwrap_or(0))
            .unwrap_or(start_ns);
        let mut attributes = vec![
            attr_str("ritual.stage", &m.stage),
            attr_str("ritual.agent", &m.agent),
            attr_str("ritual.branch", &m.branch),
            attr_bool("ritual.ok", m.ok),
        ];
        if let Some(c) = m.total_cost_usd {
            attributes.push(attr_f64("ritual.cost_usd", c));
        }
        if let Some(u) = &m.usage {
            attributes.push(attr_i64("ritual.tokens.output", u.output_tokens as i64));
            attributes.push(attr_i64("ritual.tokens.input", u.input_tokens as i64));
        }
        let span = serde_json::json!({
            "resourceSpans": [{
                "resource": { "attributes": [attr_str("service.name", "ritual")] },
                "scopeSpans": [{
                    "scope": { "name": "ritual" },
                    "spans": [{
                        "traceId": trace_id,
                        "spanId": span_id,
                        "name": format!("ritual:{}", m.stage),
                        "kind": 1,
                        "startTimeUnixNano": start_ns.to_string(),
                        "endTimeUnixNano": end_ns.to_string(),
                        "status": { "code": if m.ok { 1 } else { 2 } },
                        "attributes": attributes,
                    }]
                }]
            }]
        });
        lines.push(serde_json::to_string(&span)?);
    }
    let text = lines.join("\n") + "\n";
    match out {
        Some(p) => std::fs::write(p, text)?,
        None => print!("{text}"),
    }
    eprintln!("{} span(s) exported", metas.len());
    Ok(())
}

fn attr_str(key: &str, v: &str) -> serde_json::Value {
    serde_json::json!({"key": key, "value": {"stringValue": v}})
}
fn attr_bool(key: &str, v: bool) -> serde_json::Value {
    serde_json::json!({"key": key, "value": {"boolValue": v}})
}
fn attr_f64(key: &str, v: f64) -> serde_json::Value {
    serde_json::json!({"key": key, "value": {"doubleValue": v}})
}
fn attr_i64(key: &str, v: i64) -> serde_json::Value {
    serde_json::json!({"key": key, "value": {"intValue": v.to_string()}})
}
