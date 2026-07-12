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
            // OTel GenAI semconv (Development, 2026): gen_ai.provider.name is
            // the current attribute — gen_ai.system is deprecated.
            attr_str("gen_ai.operation.name", "invoke_agent"),
            attr_str(
                "gen_ai.provider.name",
                match m.agent.as_str() {
                    "claude" => "anthropic",
                    "codex" => "openai",
                    other => other,
                },
            ),
            attr_str("gen_ai.agent.name", &format!("ritual:{}", m.stage)),
        ];
        if let Some(model) = &m.model {
            attributes.push(attr_str("gen_ai.response.model", model));
        }
        // The requested model rides in the recorded argv (--model X).
        if let Some(i) = m.argv.iter().position(|a| a == "--model")
            && let Some(req_model) = m.argv.get(i + 1)
        {
            attributes.push(attr_str("gen_ai.request.model", req_model));
        }
        if let Some(sid) = &m.session_id {
            attributes.push(attr_str("gen_ai.conversation.id", sid));
        }
        if let Some(c) = m.total_cost_usd {
            attributes.push(attr_f64("ritual.cost_usd", c));
        }
        if let Some(n) = m.num_turns {
            attributes.push(attr_i64("ritual.num_turns", n as i64));
        }
        if let Some(u) = &m.usage {
            attributes.push(attr_i64("ritual.tokens.output", u.output_tokens as i64));
            attributes.push(attr_i64("ritual.tokens.input", u.input_tokens as i64));
            attributes.push(attr_i64(
                "gen_ai.usage.output_tokens",
                u.output_tokens as i64,
            ));
            attributes.push(attr_i64("gen_ai.usage.input_tokens", u.input_tokens as i64));
            attributes.push(attr_i64(
                "ritual.tokens.cache_read",
                u.cache_read_input_tokens as i64,
            ));
            attributes.push(attr_i64(
                "ritual.tokens.cache_creation",
                u.cache_creation_input_tokens as i64,
            ));
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

/// `ritual export --audit-trail` — one draft-sharif-agent-audit-trail-00
/// record per finished run, oldest first, hash-chained: `prev_hash(N)` =
/// SHA-256 over the RFC 8785 (JCS) canonicalization of record N-1. With
/// default features serde_json already IS canonical for this value domain
/// (BTreeMap-backed objects = sorted keys, minimal escapes, plain integers),
/// so `canonical()` below is a real JCS for what we emit.
pub fn audit_trail(dirs: &RitualDirs, out: Option<&std::path::Path>) -> Result<()> {
    let mut metas = history::load_all(&dirs.runs_dir())?;
    metas.reverse(); // chain runs oldest -> newest

    let mut lines = Vec::new();
    let mut prev: Option<serde_json::Value> = None;
    for m in &metas {
        let record_id = synth_uuid(&m.run_id);
        let session_id = m
            .session_id
            .as_deref()
            .filter(|s| is_uuid(s))
            .map(str::to_string)
            .unwrap_or_else(|| synth_uuid(m.session_id.as_deref().unwrap_or(&m.run_id)));
        let agent_version = m.repro.as_ref().and_then(|r| match m.agent.as_str() {
            "claude" => r.claude_version.clone(),
            "codex" => r.codex_version.clone(),
            _ => None,
        });
        let mut record = serde_json::json!({
            "record_id": record_id,
            "timestamp": m.started_at.or(m.finished_at)
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".into()),
            "agent_id": format!("urn:ritual:agent:{}", if m.agent.is_empty() { "unknown" } else { &m.agent }),
            "agent_version": agent_version.unwrap_or_else(|| "unknown".into()),
            "session_id": session_id,
            "action_type": format!("stage.{}", m.stage),
            "action_detail": {
                "feature": m.feature,
                "branch": m.branch,
                "run_id": m.run_id,
                "exit_code": m.exit_code,
            },
            "outcome": if m.ok { "success" } else { "failure" },
            "trust_level": "L2",
            "parent_record_id": prev.as_ref().map(|p| p["record_id"].clone()).unwrap_or(serde_json::Value::Null),
            "prev_hash": prev.as_ref().map(|p| serde_json::Value::String(sha256_hex(canonical(p).as_bytes()))).unwrap_or(serde_json::Value::Null),
        });
        // Optional members the draft defines and we actually have.
        if let Some(model) = &m.model {
            record["model_id"] = serde_json::json!(model);
        }
        if let Some(c) = m.total_cost_usd {
            record["cost_estimate"] = serde_json::json!(c);
        }
        if let Some(d) = m.duration_ms {
            record["latency_ms"] = serde_json::json!(d);
        }
        lines.push(canonical(&record));
        prev = Some(record);
    }

    let text = lines.join("\n") + "\n";
    match out {
        Some(p) => std::fs::write(p, text)?,
        None => print!("{text}"),
    }
    eprintln!("{} audit record(s) exported", metas.len());
    Ok(())
}

/// RFC 8785 canonical form for OUR value domain (see audit_trail docs).
fn canonical(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Deterministic UUIDv4-formatted id from a seed (version/variant bits set).
fn synth_uuid(seed: &str) -> String {
    let hex = sha256_hex(seed.as_bytes());
    let mut c: Vec<char> = hex[..32].chars().collect();
    c[12] = '4';
    c[16] = ['8', '9', 'a', 'b'][(c[16].to_digit(16).unwrap_or(0) & 0b11) as usize];
    let s: String = c.into_iter().collect();
    format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, ch)| match i {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        })
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
    fn gen_ai_semconv_attributes_ride_along() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        write_meta(
            &runs,
            "20260711T000000Z-g.meta.json",
            r#"{"run_id":"g1","stage":"dual-review","agent":"claude","ok":true,
                "model":"claude-fable-5","session_id":"s-123","num_turns":7,
                "argv":["claude","-p","x","--model","opus"],
                "usage":{"input_tokens":10,"output_tokens":5,
                         "cache_read_input_tokens":90,"cache_creation_input_tokens":2}}"#,
        );
        let lines = export_to_string(&tmp);
        let span = first_span(&lines[0]);
        assert_eq!(
            attr(span, "gen_ai.operation.name").unwrap()["stringValue"],
            "invoke_agent"
        );
        assert_eq!(
            attr(span, "gen_ai.provider.name").unwrap()["stringValue"],
            "anthropic"
        );
        assert_eq!(
            attr(span, "gen_ai.response.model").unwrap()["stringValue"],
            "claude-fable-5"
        );
        assert_eq!(
            attr(span, "gen_ai.request.model").unwrap()["stringValue"],
            "opus",
            "requested model recovered from recorded argv"
        );
        assert_eq!(
            attr(span, "gen_ai.conversation.id").unwrap()["stringValue"],
            "s-123"
        );
        assert_eq!(
            attr(span, "gen_ai.usage.input_tokens").unwrap()["intValue"],
            "10"
        );
        assert_eq!(
            attr(span, "ritual.tokens.cache_read").unwrap()["intValue"],
            "90"
        );
        assert_eq!(attr(span, "ritual.num_turns").unwrap()["intValue"], "7");
    }

    #[test]
    fn audit_trail_chains_records_with_jcs_sha256() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        for (i, ok) in [(1, true), (2, true), (3, false)] {
            write_meta(
                &runs,
                &format!("20260711T00000{i}Z-r.meta.json"),
                &format!(
                    r#"{{"run_id":"run-{i}","stage":"plan-review","agent":"claude","ok":{ok},
                        "feature":"F","branch":"main","exit_code":0,"duration_ms":1200,
                        "total_cost_usd":0.5,"model":"claude-fable-5",
                        "started_at":"2026-07-11T00:00:0{i}Z"}}"#
                ),
            );
        }
        let dirs = RitualDirs::new(tmp.path());
        let out = tmp.path().join("audit.jsonl");
        audit_trail(&dirs, Some(&out)).unwrap();
        let records: Vec<Value> = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(records.len(), 3);

        // Oldest first; the genesis record has null links.
        assert_eq!(records[0]["action_detail"]["run_id"], "run-1");
        assert!(records[0]["prev_hash"].is_null());
        assert!(records[0]["parent_record_id"].is_null());

        // Mandatory draft members present with the exact names.
        for r in &records {
            for key in [
                "record_id",
                "timestamp",
                "agent_id",
                "agent_version",
                "session_id",
                "action_type",
                "action_detail",
                "outcome",
                "trust_level",
                "parent_record_id",
                "prev_hash",
            ] {
                assert!(!r[key].is_null() || key == "prev_hash" || key == "parent_record_id");
            }
            let id = r["record_id"].as_str().unwrap();
            assert!(is_uuid(id), "not uuid-shaped: {id}");
            assert_eq!(id.as_bytes()[14], b'4', "version nibble");
        }
        assert_eq!(records[2]["outcome"], "failure");
        assert_eq!(records[1]["parent_record_id"], records[0]["record_id"]);

        // The chain verifies: prev_hash(N) == sha256(JCS(record N-1)).
        for i in 1..records.len() {
            let expect = sha256_hex(canonical(&records[i - 1]).as_bytes());
            assert_eq!(
                records[i]["prev_hash"].as_str().unwrap(),
                expect,
                "broken link at {i}"
            );
        }

        // Records are emitted in canonical (sorted-key) form on disk.
        let first_line = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert_eq!(first_line, canonical(&records[0]));
    }

    #[test]
    fn audit_trail_zero_runs_is_a_sane_noop() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual/runs")).unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let out = tmp.path().join("audit.jsonl");
        audit_trail(&dirs, Some(&out)).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text.lines().filter(|l| !l.trim().is_empty()).count(), 0);
    }

    #[test]
    fn synth_uuid_shape_is_deterministic_v4() {
        for i in 0..64 {
            let u = synth_uuid(&format!("seed-{i}"));
            assert!(is_uuid(&u), "{u}");
            assert_eq!(u.as_bytes()[14], b'4', "version nibble: {u}");
            assert!(
                matches!(u.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
                "variant nibble: {u}"
            );
            assert_eq!(u, synth_uuid(&format!("seed-{i}")), "deterministic");
        }
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("11111111x2222-4333-8444-555555555555"));
    }

    #[test]
    fn audit_trail_passes_real_session_uuids_and_maps_codex_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        let real = "11111111-2222-4333-8444-555555555555";
        write_meta(
            &runs,
            "20260711T000000Z-x.meta.json",
            &format!(
                r#"{{"run_id":"rx","stage":"plan-review","agent":"codex","ok":true,
                    "session_id":"{real}","started_at":"2026-07-11T00:00:00Z"}}"#
            ),
        );
        let dirs = RitualDirs::new(tmp.path());
        let out = tmp.path().join("audit.jsonl");
        audit_trail(&dirs, Some(&out)).unwrap();
        let rec: Value = serde_json::from_str(
            std::fs::read_to_string(&out)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            rec["session_id"], real,
            "real UUIDs pass through unsynthesized"
        );
        assert_eq!(rec["agent_id"], "urn:ritual:agent:codex");

        // And the OTLP side maps codex to the openai provider.
        let lines = export_to_string(&tmp);
        let span = first_span(&lines[0]);
        assert_eq!(
            attr(span, "gen_ai.provider.name").unwrap()["stringValue"],
            "openai"
        );
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
