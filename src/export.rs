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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn write_meta(runs: &std::path::Path, name: &str, json: &str) {
        std::fs::write(runs.join(name), json).unwrap();
    }

    /// Find an OTLP attribute by key inside a span's attribute array.
    fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
        span["attributes"]
            .as_array()?
            .iter()
            .find(|a| a["key"] == key)
            .map(|a| &a["value"])
    }

    fn export_to_string(dir: &tempfile::TempDir) -> Vec<Value> {
        let dirs = RitualDirs::new(dir.path());
        let out = dir.path().join("spans.jsonl");
        otlp_json(&dirs, Some(&out)).unwrap();
        std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn first_span(line: &Value) -> &Value {
        &line["resourceSpans"][0]["scopeSpans"][0]["spans"][0]
    }

    #[test]
    fn empty_project_exports_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual/runs")).unwrap();
        assert!(export_to_string(&tmp).is_empty());
    }

    #[test]
    fn one_run_becomes_one_valid_otlp_span() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        write_meta(
            &runs,
            "20260711T000000Z-a.meta.json",
            r#"{"run_id":"r1","stage":"plan-review","agent":"claude","branch":"main",
                "ok":true,"total_cost_usd":1.25,
                "usage":{"input_tokens":100,"output_tokens":40},
                "started_at":"2026-07-11T00:00:00Z","finished_at":"2026-07-11T00:01:00Z"}"#,
        );
        let lines = export_to_string(&tmp);
        assert_eq!(lines.len(), 1);
        let span = first_span(&lines[0]);

        // OTLP id widths: trace = 32 hex, span = 16 hex.
        assert_eq!(span["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(span["spanId"].as_str().unwrap().len(), 16);
        assert_eq!(span["name"], "ritual:plan-review");
        assert_eq!(span["status"]["code"], 1); // ok
        assert_eq!(
            attr(span, "ritual.stage").unwrap()["stringValue"],
            "plan-review"
        );
        assert_eq!(attr(span, "ritual.agent").unwrap()["stringValue"], "claude");
        assert_eq!(attr(span, "ritual.ok").unwrap()["boolValue"], true);
        assert_eq!(attr(span, "ritual.cost_usd").unwrap()["doubleValue"], 1.25);
        // intValue is stringified per the OTLP JSON encoding.
        assert_eq!(
            attr(span, "ritual.tokens.output").unwrap()["intValue"],
            "40"
        );
        // End time must reflect finished_at, not equal start.
        assert_ne!(span["startTimeUnixNano"], span["endTimeUnixNano"]);
    }

    #[test]
    fn failed_run_maps_to_error_status_code() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        write_meta(
            &runs,
            "20260711T000000Z-b.meta.json",
            r#"{"run_id":"r2","stage":"dual-review","ok":false}"#,
        );
        let lines = export_to_string(&tmp);
        let span = first_span(&lines[0]);
        assert_eq!(span["status"]["code"], 2); // error
        assert_eq!(attr(span, "ritual.ok").unwrap()["boolValue"], false);
    }

    #[test]
    fn distinct_runs_get_distinct_trace_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        write_meta(
            &runs,
            "20260711T000000Z-a.meta.json",
            r#"{"run_id":"alpha","stage":"plan-review","ok":true}"#,
        );
        write_meta(
            &runs,
            "20260711T000001Z-b.meta.json",
            r#"{"run_id":"beta","stage":"plan-review","ok":true}"#,
        );
        let lines = export_to_string(&tmp);
        assert_eq!(lines.len(), 2);
        let t0 = first_span(&lines[0])["traceId"].as_str().unwrap();
        let t1 = first_span(&lines[1])["traceId"].as_str().unwrap();
        assert_ne!(t0, t1);
    }
}
