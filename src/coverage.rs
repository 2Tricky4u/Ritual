//! Pure parsing of the `/coverage` judge's findings file into a completeness
//! report, plus the "is the project actually done" predicate. Kept free of I/O
//! and App so it is unit-testable (the repo pattern: `code_fix`, `review`,
//! `answers`). The judge writes ONE `-coverage.json`: a top-level
//! `satisfied` array of deliverable ids it confirmed done, plus one open finding
//! per gap (each carrying `extra.deliverable` and a `file` or `plan_step` route).

use std::path::Path;

use crate::findings::{Finding, FindingsFile, LoadedFindings};
use crate::state::{Feature, StageId, StageStatus};

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

/// The real "project is done" signal (the missing gate): the coverage stage
/// judged it complete (== Done, i.e. zero gaps), the tree passes `check.sh`, AND
/// no confirmed finding is still open. Green tests alone are never enough.
pub fn feature_complete(feature: &Feature, findings: &[LoadedFindings], check_green: bool) -> bool {
    feature.stage(StageId::Coverage).status == StageStatus::Done
        && check_green
        && !crate::findings::has_open_confirmed(findings)
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
    fn feature_complete_requires_coverage_done_and_green() {
        let mut feat = Feature::new("main", "x");
        assert!(
            !feature_complete(&feat, &[], true),
            "coverage pending -> not complete even when green"
        );
        feat.stages.get_mut(&StageId::Coverage).unwrap().status = StageStatus::Done;
        assert!(feature_complete(&feat, &[], true), "done + green + clean");
        assert!(
            !feature_complete(&feat, &[], false),
            "a red check.sh blocks completion"
        );
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
