//! Pure parsing of the `/coverage` judge's findings file into a completeness
//! report, plus the "is the project actually done" predicate. Kept free of I/O
//! and App so it is unit-testable (the repo pattern: `code_fix`, `review`,
//! `answers`). The judge writes ONE `-coverage.json`: a top-level
//! `satisfied` array of deliverable ids it confirmed done, plus one open finding
//! per gap (each carrying `extra.deliverable` and a `file` or `plan_step` route).

use crate::findings::{Finding, FindingsFile};

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
    fn gap_deliverable_falls_back_to_title() {
        let ff: FindingsFile = serde_json::from_str(
            r#"{"stage":"coverage","findings":[{"title":"no id here","action":"pending"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_report(&ff).gaps[0].deliverable, "no id here");
    }
}
