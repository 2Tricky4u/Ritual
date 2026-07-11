//! CI mode: JUnit XML emission from findings so `ritual run <stage> --ci`
//! plugs into any CI system's test-report tooling. Confirmed critical/major
//! findings are failures; the report lands in `.ritual/ci/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::findings::{FindingsFile, Severity};

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A finding fails the suite when it's confirmed and critical/major.
fn is_failure(sev: Severity, verdict: &str) -> bool {
    matches!(sev, Severity::Critical | Severity::Major) && verdict == "confirmed"
}

pub struct JunitOutcome {
    pub path: PathBuf,
    pub failures: usize,
    pub tests: usize,
}

/// Write one JUnit file for a run's findings (possibly several findings
/// files when a stage emitted more than one).
pub fn write_junit(
    ci_dir: &Path,
    run_id: &str,
    stage: &str,
    findings: &[&FindingsFile],
    stage_failed: bool,
) -> Result<JunitOutcome> {
    std::fs::create_dir_all(ci_dir)?;

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
            let name = format!("{}: {} [{}]", f.severity.label(), f.title, f.location());
            if is_failure(f.severity, &f.verdict) {
                failures += 1;
                cases.push_str(&format!(
                    r#"    <testcase name="{}" classname="ritual.{}"><failure message="{}">{}</failure></testcase>{}"#,
                    xml_escape(&name),
                    xml_escape(&file.stage),
                    xml_escape(&f.verdict),
                    xml_escape(&f.scenario),
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
        let out = write_junit(tmp.path(), "run1", "dual-review", &[&file], false).unwrap();
        assert_eq!(out.tests, 3);
        assert_eq!(out.failures, 1);
        let xml = std::fs::read_to_string(&out.path).unwrap();
        assert!(xml.contains("bad &lt;thing&gt;"));
        assert!(xml.contains("boom &amp; crash"));
        assert!(xml.contains(r#"failures="1""#));
    }

    #[test]
    fn junit_empty_findings_is_one_passing_case() {
        let tmp = tempfile::tempdir().unwrap();
        let out = write_junit(tmp.path(), "run2", "plan-review", &[], false).unwrap();
        assert_eq!(out.tests, 1);
        assert_eq!(out.failures, 0);
    }

    #[test]
    fn junit_stage_failure_is_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = write_junit(tmp.path(), "run3", "dual-review", &[], true).unwrap();
        assert_eq!(out.failures, 1);
    }
}
