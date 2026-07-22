use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::Config;
use crate::findings::{Finding, FindingsFile, Severity};
use crate::state::RitualDirs;

/// True when the configured coderabbit binary answers `--version`.
pub fn available(cfg: &Config) -> bool {
    crate::agents_status::run_capture(&cfg.coderabbit_cmd, &["--version"])
        .is_some_and(|o| o.status.success())
}

/// Run a CodeRabbit CLI review over uncommitted changes and record its
/// comments as SINGLE-SOURCE findings (verdict "unconfirmed", an ensemble's
/// third voice, never a blocker; the dual-review skill verifies/refutes and
/// only then adds `coderabbit` to a finding's sources). Cloud-backed and
/// rate-limited (3/hour free), so failures are the caller's notice, not a
/// pipeline error.
pub fn review(cfg: &Config, dirs: &RitualDirs) -> Result<Option<PathBuf>> {
    let argv = &cfg.coderabbit_cmd;
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .args(["review", "--agent", "--type", "uncommitted"])
        .args(["--base", &cfg.base_ref])
        .current_dir(&dirs.work_root)
        .output()
        .with_context(|| format!("running `{}`", argv.join(" ")))?;
    anyhow::ensure!(
        out.status.success(),
        "`{} review` failed: {}",
        argv.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );

    let findings = findings_from_agent_json(&String::from_utf8_lossy(&out.stdout), cfg.redaction);
    if findings.is_empty() {
        return Ok(None);
    }
    let file = FindingsFile {
        ritual_findings: 1,
        stage: "coderabbit".into(),
        branch: crate::state::current_branch(&dirs.work_root).unwrap_or_default(),
        generated_at: Utc::now().to_rfc3339(),
        findings,
        ..Default::default()
    };
    // Millisecond stamp (run-id style) so same-second runs never clobber;
    // atomic write so a concurrent reader never parses a torn file.
    let path = dirs.findings_dir().join(format!(
        "{}-coderabbit.json",
        Utc::now().format("%Y%m%dT%H%M%S%3fZ")
    ));
    crate::fsx::atomic_write(&path, serde_json::to_string_pretty(&file)?.as_bytes())?;
    Ok(Some(path))
}

/// Drift-tolerant mapper over `--agent` JSON output. The schema is not a
/// published contract, so this walks the whole document and treats any object
/// carrying a file path + comment text as a review comment; everything
/// unrecognized is skipped (same philosophy as AgentEvent::Raw). Junk that
/// isn't JSON at all yields zero findings, never an error.
fn findings_from_agent_json(text: &str, redact: bool) -> Vec<Finding> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut redactor = crate::redact::Redactor::new(redact);
    let mut raw = Vec::new();
    collect(&v, &mut raw);
    raw.iter()
        .enumerate()
        .map(|(i, (file, line, sev, comment, snippet))| {
            let mut title = comment.lines().next().unwrap_or_default().to_string();
            title.truncate(80);
            Finding {
                id: (i + 1) as u32,
                severity: *sev,
                title: redactor.text(&title),
                file: Some(file.clone()),
                line: *line,
                snippet: snippet.as_deref().map(|s| redactor.text(s)),
                scenario: redactor.text(comment),
                sources: vec!["coderabbit".into()],
                verdict: "unconfirmed".into(),
                action: "pending".into(),
                ..Default::default()
            }
        })
        .collect()
}

type RawComment = (String, Option<u32>, Severity, String, Option<String>);

fn collect(v: &serde_json::Value, out: &mut Vec<RawComment>) {
    match v {
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        serde_json::Value::Object(o) => {
            if let Some(c) = try_comment(o) {
                out.push(c);
            } else {
                o.values().for_each(|x| collect(x, out));
            }
        }
        _ => {}
    }
}

/// An object is a review comment iff it names a file AND carries prose.
fn try_comment(o: &serde_json::Map<String, serde_json::Value>) -> Option<RawComment> {
    let file = get_str(o, &["file", "path", "filename", "file_path"])?;
    let comment = get_str(o, &["comment", "body", "message", "description", "summary"])?;
    if comment.trim().is_empty() {
        return None;
    }
    let line = ["line", "start_line", "startLine", "line_start"]
        .iter()
        .find_map(|k| o.get(*k)?.as_u64())
        .map(|l| l as u32);
    let severity = match get_str(o, &["severity", "level", "priority"])
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "critical" | "high" | "major" | "error" => Severity::Major,
        _ => Severity::Minor, // single-source: never critical on its own say-so
    };
    let snippet = get_str(o, &["snippet", "code", "code_snippet", "codeSnippet"]);
    Some((file, line, severity, comment, snippet))
}

fn get_str(o: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| o.get(*k)?.as_str())
        .map(str::to_string)
}

/// Best-effort pre-review hook for the dual-review spawn path.
pub fn preflight(cfg: &Config, dirs: &RitualDirs) -> Option<String> {
    if !cfg.coderabbit_enabled {
        return None;
    }
    if !available(cfg) {
        return Some("coderabbit skipped: CLI not installed (see the guide)".into());
    }
    match review(cfg, dirs) {
        Ok(Some(path)) => Some(format!(
            "coderabbit review → {} (unconfirmed until dual-review verifies)",
            path.display()
        )),
        Ok(None) => Some("coderabbit review: no comments".into()),
        // Rate limits (3/hour free) and network blips must never fail a review run.
        Err(e) => Some(format!("coderabbit skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_nested_agent_json_to_unconfirmed_findings() {
        let json = r#"{
            "review": {"id": "r-1"},
            "comments": [
                {"file": "src/a.rs", "line": 12, "severity": "high",
                 "comment": "Possible off-by-one in loop bound\nThe range excludes the last element.",
                 "code_snippet": "for i in 0..len - 1 {"},
                {"path": "src/b.rs", "startLine": 4,
                 "body": "Consider renaming for clarity"},
                {"note": "no file here, not a comment"}
            ]
        }"#;
        let f = findings_from_agent_json(json, true);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].title, "Possible off-by-one in loop bound");
        assert_eq!(f[0].severity, Severity::Major);
        assert_eq!(f[0].file.as_deref(), Some("src/a.rs"));
        assert_eq!(f[0].line, Some(12));
        assert_eq!(f[0].snippet.as_deref(), Some("for i in 0..len - 1 {"));
        assert_eq!(f[0].verdict, "unconfirmed");
        assert_eq!(f[0].sources, vec!["coderabbit"]);
        assert_eq!(f[1].severity, Severity::Minor, "unknown severity -> minor");
        assert_eq!(f[1].line, Some(4));
    }

    #[test]
    fn try_comment_key_variant_matrix() {
        let json = r#"[
            {"filename":"a.rs","message":"m1","start_line":1,"level":"critical"},
            {"file_path":"b.rs","description":"m2","line_start":2,"priority":"error","code":"let x;"},
            {"path":"c.rs","summary":"m3","line":3,"severity":"major","codeSnippet":"y()"},
            {"file":"d.rs","comment":"   "},
            {"file":"e.rs","comment":"m5","line":"not-a-number"}
        ]"#;
        let f = findings_from_agent_json(json, false);
        assert_eq!(f.len(), 4, "whitespace-only comment dropped: {f:#?}");

        assert_eq!(f[0].file.as_deref(), Some("a.rs"));
        assert_eq!(f[0].title, "m1");
        assert_eq!(f[0].line, Some(1));
        assert_eq!(f[0].severity, Severity::Major, "critical caps at major");

        assert_eq!(f[1].file.as_deref(), Some("b.rs"));
        assert_eq!(f[1].line, Some(2));
        assert_eq!(f[1].severity, Severity::Major, "error maps to major");
        assert_eq!(f[1].snippet.as_deref(), Some("let x;"));

        assert_eq!(f[2].severity, Severity::Major);
        assert_eq!(f[2].snippet.as_deref(), Some("y()"));

        // A non-numeric line is tolerated as no anchor, not a parse failure.
        assert_eq!(f[3].file.as_deref(), Some("e.rs"));
        assert_eq!(f[3].line, None);
    }

    #[test]
    fn junk_and_schema_drift_yield_zero_findings_not_errors() {
        assert!(findings_from_agent_json("not json at all", true).is_empty());
        assert!(findings_from_agent_json("{}", true).is_empty());
        assert!(findings_from_agent_json(r#"{"totally":{"different":"shape"}}"#, true).is_empty());
        // A comment without a file is ignored rather than mis-anchored.
        assert!(findings_from_agent_json(r#"[{"body":"floating remark"}]"#, true).is_empty());
    }
}
