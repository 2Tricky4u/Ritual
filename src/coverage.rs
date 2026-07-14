//! The `/coverage` judge's report model + the completeness predicate + the
//! shared post-run `finalize` (App-free so the CLI and the TUI share one code
//! path). `parse_report`/`is_complete`/`feature_complete` are pure; `latest_report`
//! and `finalize` touch the findings dir + plan. The judge writes ONE
//! `-coverage.json`: a top-level `satisfied` array of deliverable ids it confirmed
//! done, plus one open finding per gap (each carrying `extra.deliverable` and a
//! `file` or `plan_step` route).

use std::collections::HashSet;
use std::path::Path;

use crate::findings::{Finding, FindingsFile, LoadedFindings};
use crate::state::{self, RitualDirs, StageStatus};

/// One unmet deliverable the coverage judge flagged.
#[derive(Debug, Clone)]
pub struct Gap {
    /// The deliverable id (from the finding's `extra.deliverable`, else its title).
    pub deliverable: String,
    pub finding: Finding,
}

/// The parsed coverage verdict: which deliverables the judge confirmed done, and
/// which are still gaps (open findings).
#[derive(Debug, Clone, Default)]
pub struct CoverageReport {
    pub satisfied: Vec<String>,
    pub gaps: Vec<Gap>,
}

/// Extract the report from a `-coverage.json` findings file. Satisfied ids come
/// from the top-level `extra.satisfied` array; gaps are the OPEN findings (a
/// gap that has since been fixed/dismissed no longer counts).
pub fn parse_report(ff: &FindingsFile) -> CoverageReport {
    let satisfied = ff
        .extra
        .get("satisfied")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let gaps = ff
        .findings
        .iter()
        .filter(|f| !f.resolved())
        .map(|f| Gap {
            deliverable: f
                .extra
                .get("deliverable")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&f.title)
                .to_string(),
            finding: f.clone(),
        })
        .collect();
    CoverageReport { satisfied, gaps }
}

/// This coverage run reports the project complete: zero open gaps.
pub fn is_complete(ff: &FindingsFile) -> bool {
    parse_report(ff).gaps.is_empty()
}

/// The newest `-coverage.json` report in the findings dir (files are
/// UTC-timestamp prefixed, so lexical sort = chronological), or None.
pub fn latest_report(findings_dir: &Path) -> Option<CoverageReport> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(findings_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("-coverage.json"))
        })
        .collect();
    files.sort();
    let text = std::fs::read_to_string(files.last()?).ok()?;
    serde_json::from_str::<FindingsFile>(&text)
        .ok()
        .map(|ff| parse_report(&ff))
}

/// The real "project is done" signal (the missing gate). Derived DETERMINISTICALLY
/// from evidence, NOT from the Coverage stage status (which the TUI and CLI set
/// differently and can be stale): the latest coverage report is genuinely
/// zero-gap (`coverage_clean`), the plan declares a real `## Deliverables`
/// checklist (`deliverables_ok`), the tree passes `check.sh`, AND no confirmed
/// non-coverage finding is still open. Green tests alone are never enough.
pub fn feature_complete(
    coverage_clean: bool,
    deliverables_ok: bool,
    findings: &[LoadedFindings],
    check_green: bool,
) -> bool {
    coverage_clean
        && deliverables_ok
        && check_green
        && !crate::findings::has_open_confirmed(findings)
}

/// Finalize a coverage run: parse the judge's report, supersede older coverage
/// files, tick the satisfied deliverables into the plan (confined + undo-pushed),
/// and return the stage status - `Done` ONLY at zero gaps AND a real
/// `## Deliverables` checklist. PRINT-FREE (returns messages) so BOTH the CLI
/// (`run_headless`) and the TUI (which must not write to the alt-screen) can
/// call it. `new_findings` is the set of files this run wrote.
pub fn finalize(
    dirs: &RitualDirs,
    branch: &str,
    new_findings: &[String],
) -> (StageStatus, Vec<String>) {
    let mut msgs = Vec::new();
    let slug = state::branch_slug(branch);
    let Some(name) = new_findings.iter().find(|f| f.ends_with("-coverage.json")) else {
        msgs.push("coverage run wrote no coverage findings; needs attention".into());
        return (StageStatus::NeedsAttention, msgs);
    };
    let Some(ff) = std::fs::read_to_string(dirs.findings_dir().join(name))
        .ok()
        .and_then(|t| serde_json::from_str::<FindingsFile>(&t).ok())
    else {
        return (StageStatus::NeedsAttention, msgs);
    };
    let report = parse_report(&ff);

    // Supersede: keep only the file we just read; delete older coverage files.
    if let Ok(rd) = std::fs::read_dir(dirs.findings_dir()) {
        for e in rd.flatten() {
            let fname = e.file_name();
            let fname = fname.to_string_lossy();
            if fname.ends_with("-coverage.json") && fname != name.as_str() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    // Tick satisfied deliverables (never one also flagged a gap) into the plan.
    let gap_ids: HashSet<&str> = report.gaps.iter().map(|g| g.deliverable.as_str()).collect();
    let satisfied: Vec<&str> = report
        .satisfied
        .iter()
        .map(String::as_str)
        .filter(|id| !gap_ids.contains(id))
        .collect();
    if !satisfied.is_empty()
        && let Ok(before) = std::fs::read_to_string(dirs.plan_file(&slug))
    {
        let after = crate::spec::tick(&before, &satisfied);
        let deliverables = ["Deliverables".to_string()];
        if after != before
            && crate::spec::confine_by_heading(&before, &after, &deliverables).is_some()
        {
            let _ = crate::undo::push(dirs, &slug, "plan", &before);
            let _ = std::fs::write(dirs.plan_file(&slug), &after);
        }
    }

    // Deterministic backstop: a plan with no real `## Deliverables` checklist is
    // never "complete", no matter how empty the gap list is.
    let deliverables_ok = std::fs::read_to_string(dirs.plan_file(&slug))
        .map(|t| crate::spec::deliverables_gate(&t).is_ok())
        .unwrap_or(false);

    if report.gaps.is_empty() && deliverables_ok {
        msgs.push("coverage: all deliverables satisfied - feature complete".into());
        (StageStatus::Done, msgs)
    } else if report.gaps.is_empty() {
        msgs.push(
            "coverage: no `## Deliverables` checklist to verify against; needs attention".into(),
        );
        (StageStatus::NeedsAttention, msgs)
    } else {
        msgs.push(format!(
            "coverage: {} deliverable gap(s) remain; needs attention",
            report.gaps.len()
        ));
        (StageStatus::NeedsAttention, msgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_report_splits_satisfied_from_open_gaps() {
        let json = r#"{
            "stage": "coverage",
            "satisfied": ["D1", "D2"],
            "findings": [
                {"id":1,"title":"media stack missing","verdict":"confirmed","action":"pending","file":"stacks/media/compose.yml","deliverable":"D3"},
                {"id":2,"title":"cloud stub","verdict":"confirmed","action":"fixed","deliverable":"D4"}
            ]
        }"#;
        let ff: FindingsFile = serde_json::from_str(json).unwrap();
        let rep = parse_report(&ff);
        assert_eq!(rep.satisfied, vec!["D1", "D2"]);
        assert_eq!(rep.gaps.len(), 1, "the fixed gap D4 no longer counts");
        assert_eq!(rep.gaps[0].deliverable, "D3");
        assert_eq!(
            rep.gaps[0].finding.file.as_deref(),
            Some("stacks/media/compose.yml")
        );
        assert!(!is_complete(&ff));
    }

    #[test]
    fn is_complete_with_no_open_gaps() {
        let ff: FindingsFile =
            serde_json::from_str(r#"{"stage":"coverage","satisfied":["D1"],"findings":[]}"#)
                .unwrap();
        assert!(is_complete(&ff));
    }

    #[test]
    fn feature_complete_requires_every_deterministic_signal() {
        // All four must hold: coverage clean, deliverables declared, green, no open.
        assert!(feature_complete(true, true, &[], true), "all signals true");
        assert!(
            !feature_complete(false, true, &[], true),
            "coverage not clean blocks"
        );
        assert!(
            !feature_complete(true, false, &[], true),
            "no deliverables checklist blocks"
        );
        assert!(
            !feature_complete(true, true, &[], false),
            "a red check.sh blocks"
        );
    }

    #[test]
    fn finalize_is_done_only_at_zero_gaps_with_deliverables_and_supersedes() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(dirs.findings_dir()).unwrap();
        std::fs::create_dir_all(dirs.feature_dir("main")).unwrap();
        std::fs::write(
            dirs.plan_file("main"),
            "# Plan\n\n## Deliverables\n- [ ] D1: x - accept: y is true\n",
        )
        .unwrap();
        // A coverage report WITH a gap -> NeedsAttention (the TUI-false-Done fix).
        std::fs::write(
            dirs.findings_dir().join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","satisfied":[],"findings":[{"title":"gap","verdict":"confirmed","action":"pending","deliverable":"D1"}]}"#,
        )
        .unwrap();
        let (st, _) = finalize(&dirs, "main", &["20260101T000000Z-coverage.json".into()]);
        assert_eq!(st, StageStatus::NeedsAttention, "open gap is not Done");

        // A clean report + a real checklist -> Done, and the old file is gone.
        std::fs::write(
            dirs.findings_dir().join("20260101T000001Z-coverage.json"),
            r#"{"stage":"coverage","satisfied":["D1"],"findings":[]}"#,
        )
        .unwrap();
        let (st, _) = finalize(&dirs, "main", &["20260101T000001Z-coverage.json".into()]);
        assert_eq!(st, StageStatus::Done);
        let n = std::fs::read_dir(dirs.findings_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with("-coverage.json"))
            .count();
        assert_eq!(n, 1, "older coverage file superseded");
    }

    #[test]
    fn finalize_not_done_without_a_deliverables_checklist() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(dirs.findings_dir()).unwrap();
        std::fs::create_dir_all(dirs.feature_dir("main")).unwrap();
        std::fs::write(dirs.plan_file("main"), "# Plan\n\n## Steps\n1. x\n").unwrap();
        std::fs::write(
            dirs.findings_dir().join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","satisfied":[],"findings":[]}"#,
        )
        .unwrap();
        // Zero gaps but no `## Deliverables` -> NeedsAttention, never Done.
        let (st, _) = finalize(&dirs, "main", &["20260101T000000Z-coverage.json".into()]);
        assert_eq!(st, StageStatus::NeedsAttention);
    }

    #[test]
    fn gap_deliverable_falls_back_to_title() {
        let ff: FindingsFile = serde_json::from_str(
            r#"{"stage":"coverage","findings":[{"title":"no id here","action":"pending"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_report(&ff).gaps[0].deliverable, "no id here");
    }
}
