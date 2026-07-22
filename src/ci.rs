//! CI mode: JUnit XML emission from findings so `ritual run <stage> --ci`
//! plugs into any CI system's test-report tooling. Confirmed critical/major
//! findings are failures; the report lands in `.ritual/ci/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::findings::{Finding, FindingsFile, Severity};

fn xml_escape(s: &str) -> String {
    // Besides the markup chars, drop control characters XML 1.0 forbids even
    // escaped (everything below 0x20 except tab/newline/CR): one stray ESC
    // byte in an agent-authored scenario would invalidate the whole report
    // for strict CI parsers - exactly when there IS a finding to show.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// A finding fails the suite when it's confirmed, critical/major, and not
/// already resolved (fixed/dismissed) by a human.
fn is_failure(f: &Finding) -> bool {
    matches!(f.severity, Severity::Critical | Severity::Major)
        && crate::findings::verdict_confirmed(&f.verdict)
        && !f.resolved()
}

pub struct JunitOutcome {
    pub path: PathBuf,
    pub failures: usize,
    pub tests: usize,
}

/// Write one JUnit file for a run's findings (possibly several findings
/// files when a stage emitted more than one). `redact` runs every finding
/// text through the redactor: the XML is an outward artifact (uploaded to
/// CI), same posture as pr_comment.
pub fn write_junit(
    ci_dir: &Path,
    run_id: &str,
    stage: &str,
    findings: &[&FindingsFile],
    stage_failed: bool,
    redact: bool,
) -> Result<JunitOutcome> {
    std::fs::create_dir_all(ci_dir)?;
    let mut redactor = crate::redact::Redactor::new(redact);

    let mut cases = String::new();
    let mut tests = 0;
    let mut failures = 0;

    if stage_failed {
        tests += 1;
        failures += 1;
        cases.push_str(&format!(
            r#"    <testcase name="stage:{}" classname="ritual"><failure message="stage failed to complete"/></testcase>{}"#,
            xml_escape(stage),
            '\n'
        ));
    }

    for file in findings {
        for f in &file.findings {
            tests += 1;
            let name = redactor.text(&format!(
                "{}: {} [{}]",
                f.severity.label(),
                f.title,
                f.location()
            ));
            if is_failure(f) {
                failures += 1;
                cases.push_str(&format!(
                    r#"    <testcase name="{}" classname="ritual.{}"><failure message="{}">{}</failure></testcase>{}"#,
                    xml_escape(&name),
                    xml_escape(&file.stage),
                    xml_escape(&redactor.text(&f.verdict)),
                    xml_escape(&redactor.text(&f.scenario)),
                    '\n'
                ));
            } else {
                cases.push_str(&format!(
                    r#"    <testcase name="{}" classname="ritual.{}"/>{}"#,
                    xml_escape(&name),
                    xml_escape(&file.stage),
                    '\n'
                ));
            }
        }
    }
    if tests == 0 {
        // A clean review is still a passing test, not an empty suite.
        tests = 1;
        cases.push_str(&format!(
            r#"    <testcase name="no findings" classname="ritual.{}"/>{}"#,
            xml_escape(stage),
            '\n'
        ));
    }

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="ritual:{}" tests="{}" failures="{}" errors="0">
{}  </testsuite>
</testsuites>
"#,
        xml_escape(stage),
        tests,
        failures,
        cases
    );
    let path = ci_dir.join(format!("{run_id}.xml"));
    std::fs::write(&path, xml).with_context(|| format!("writing {}", path.display()))?;
    Ok(JunitOutcome {
        path,
        failures,
        tests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_strips_forbidden_control_chars() {
        // ESC and friends invalidate the XML even escaped; tab/newline stay.
        assert_eq!(xml_escape("a\u{1b}b\u{0}c"), "a\u{fffd}b\u{fffd}c");
        assert_eq!(xml_escape("a\tb\nc"), "a\tb\nc");
        assert_eq!(xml_escape("<&\">"), "&lt;&amp;&quot;&gt;");
    }

    #[test]
    fn junit_marks_confirmed_critical_as_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let file: FindingsFile = serde_json::from_str(
            r#"{"stage":"dual-review","findings":[
                {"id":1,"severity":"critical","title":"bad <thing>","file":"a.rs","line":1,
                 "scenario":"boom & crash","verdict":"confirmed"},
                {"id":2,"severity":"minor","title":"style","verdict":"confirmed"},
                {"id":3,"severity":"major","title":"maybe","verdict":"unconfirmed"}
            ]}"#,
        )
        .unwrap();
        let out = write_junit(tmp.path(), "run1", "dual-review", &[&file], false, false).unwrap();
        assert_eq!(out.tests, 3);
        assert_eq!(out.failures, 1);
        let xml = std::fs::read_to_string(&out.path).unwrap();
        assert!(xml.contains("bad &lt;thing&gt;"));
        assert!(xml.contains("boom &amp; crash"));
        assert!(xml.contains(r#"failures="1""#));
    }

    #[test]
    fn junit_resolved_critical_is_not_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let file: FindingsFile = serde_json::from_str(
            r#"{"stage":"dual-review","findings":[
                {"id":1,"severity":"critical","title":"was bad","verdict":"confirmed","action":"fixed"},
                {"id":2,"severity":"major","title":"noise","verdict":"confirmed","action":"dismissed"}
            ]}"#,
        )
        .unwrap();
        let out = write_junit(tmp.path(), "run4", "dual-review", &[&file], false, false).unwrap();
        assert_eq!(out.tests, 2);
        assert_eq!(out.failures, 0, "resolved findings must not fail CI");
    }

    #[test]
    fn junit_empty_findings_is_one_passing_case() {
        let tmp = tempfile::tempdir().unwrap();
        let out = write_junit(tmp.path(), "run2", "plan-review", &[], false, false).unwrap();
        assert_eq!(out.tests, 1);
        assert_eq!(out.failures, 0);
    }

    #[test]
    fn junit_stage_failure_is_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = write_junit(tmp.path(), "run3", "dual-review", &[], true, false).unwrap();
        assert_eq!(out.failures, 1);
    }
}
