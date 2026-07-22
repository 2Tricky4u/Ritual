//! The `/coverage` judge's report model + the completeness predicate + the
//! shared post-run `finalize` (App-free so the CLI and the TUI share one code
//! path). `parse_report`/`is_complete`/`feature_complete` are pure; `latest_report`
//! and `finalize` touch the findings dir + plan. The judge writes ONE
//! `-coverage.json`: a top-level `satisfied` array of deliverable ids it confirmed
//! done, plus one open finding per gap (each carrying `extra.deliverable` and a
//! `file` or `plan_step` route).

use std::collections::HashSet;
use std::path::Path;

use crate::findings::{Finding, FindingsFile};
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

/// Augment a parsed report with the deliverables the judge SILENTLY DROPPED: any
/// UNCHECKED `## Deliverables` item whose id is neither in `satisfied` nor already
/// a gap becomes an unverified gap. The judge must confirm (satisfied) or flag
/// (gap) every deliverable - silence is not "done", which is the whole point of
/// the gate. CHECKED `[x]` items are trusted (a prior round satisfied and ticked
/// them; the judge deliberately skips them). Synthetic gaps route via the
/// deliverable's declared `route:` so `drive_gaps` can build them, and carry the
/// STABLE deliverable id so the driver's attempt/stuck counters converge.
pub fn reconcile_missing(report: &mut CoverageReport, plan_text: &str) {
    use crate::findings::Severity;
    use crate::spec::{self, Route};

    let mut covered: HashSet<String> = report.satisfied.iter().map(|s| spec::norm_id(s)).collect();
    for g in &report.gaps {
        covered.insert(spec::norm_id(&g.deliverable));
    }
    for d in spec::deliverables(plan_text) {
        if d.checked || covered.contains(&spec::norm_id(&d.id)) {
            continue;
        }
        let (file, plan_step) = match &d.route {
            Some(Route::File(p)) => (Some(p.clone()), None),
            Some(Route::Section(s)) => (None, Some(s.clone())),
            None => (None, None),
        };
        let finding = Finding {
            severity: Severity::Major,
            title: format!("{}: not verified by the coverage judge", d.id),
            scenario:
                "the coverage judge neither confirmed nor flagged this deliverable; it must be \
                 verified against the tree"
                    .to_string(),
            verdict: "confirmed".to_string(),
            action: "pending".to_string(),
            file,
            plan_step,
            sources: vec!["coverage".to_string()],
            ..Default::default()
        };
        report.gaps.push(Gap {
            deliverable: d.id.clone(),
            finding,
        });
    }
}

/// The newest `-coverage.json` report BELONGING TO `slug` (files are UTC-timestamp
/// prefixed, so lexical sort = chronological), or None. Scoped so another branch's
/// newer coverage run can't be mistaken for this feature's; a branch-LESS file
/// still counts (lenient, like `has_open_confirmed`).
pub fn latest_report(findings_dir: &Path, slug: &str) -> Option<CoverageReport> {
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
    // Newest-first. A report actually STAMPED for this branch always wins, even
    // over a newer branch-less one - so an ambiguous legacy report can't shadow
    // this feature's real evidence. A branch-less report is only a fallback used
    // when no stamped report for this slug exists (backward-compat).
    let mut branchless_fallback: Option<CoverageReport> = None;
    for path in files.iter().rev() {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(ff) = serde_json::from_str::<FindingsFile>(&text) else {
            continue;
        };
        if !ff.branch.is_empty() && state::branch_slug(&ff.branch) == slug {
            return Some(parse_report(&ff));
        }
        if ff.branch.is_empty() && branchless_fallback.is_none() {
            branchless_fallback = Some(parse_report(&ff));
        }
    }
    branchless_fallback
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
    no_open: bool,
    check_green: bool,
) -> bool {
    coverage_clean && deliverables_ok && check_green && no_open
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
        msgs.push(format!(
            "coverage report {name} is unreadable or unparseable; needs attention"
        ));
        return (StageStatus::NeedsAttention, msgs);
    };
    let mut report = parse_report(&ff);

    // Supersede: delete THIS feature's older coverage files (bounds accumulation),
    // but never another branch's and never an ambiguous branch-less one (mirrors
    // reset's exact-branch caution). The file we just read is kept (excluded by
    // name); older same-branch files were stamped in prior rounds.
    if let Ok(rd) = std::fs::read_dir(dirs.findings_dir()) {
        for e in rd.flatten() {
            let fname = e.file_name();
            let fname = fname.to_string_lossy();
            if !fname.ends_with("-coverage.json") || fname == name.as_str() {
                continue;
            }
            let same_branch = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|t| serde_json::from_str::<FindingsFile>(&t).ok())
                .is_some_and(|ff| !ff.branch.is_empty() && state::branch_slug(&ff.branch) == slug);
            if same_branch {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    // Tick satisfied deliverables (never one also flagged a gap) into the plan.
    // Normalize ids on both sides (like `tick`), so a case/whitespace-drifted
    // `satisfied` entry that is ALSO a gap can't slip past this guard and tick a
    // deliverable the judge flagged (which a later round would then skip as done).
    let gap_ids: HashSet<String> = report
        .gaps
        .iter()
        .map(|g| crate::spec::norm_id(&g.deliverable))
        .collect();
    let satisfied: Vec<&str> = report
        .satisfied
        .iter()
        .map(String::as_str)
        .filter(|id| !gap_ids.contains(&crate::spec::norm_id(id)))
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

    // Reconcile against the POST-TICK plan: an unchecked deliverable the judge
    // neither confirmed nor flagged becomes a gap, so a partial/lazy report can't
    // read as clean. Read the plan once and reuse it for the backstop.
    let plan_text = std::fs::read_to_string(dirs.plan_file(&slug)).unwrap_or_default();
    reconcile_missing(&mut report, &plan_text);

    // Deterministic backstop: a plan with no real `## Deliverables` checklist is
    // never "complete", no matter how empty the gap list is.
    let deliverables_ok = crate::spec::deliverables_gate(&plan_text).is_ok();

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
        // All four must hold: coverage clean, deliverables declared, no open, green.
        assert!(feature_complete(true, true, true, true), "all signals true");
        assert!(
            !feature_complete(false, true, true, true),
            "coverage not clean blocks"
        );
        assert!(
            !feature_complete(true, false, true, true),
            "no deliverables checklist blocks"
        );
        assert!(
            !feature_complete(true, true, false, true),
            "an open confirmed finding blocks"
        );
        assert!(
            !feature_complete(true, true, true, false),
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
        // Branch-stamped (as ritual stamps post-run) so the same-slug supersede
        // can later reclaim it.
        std::fs::write(
            dirs.findings_dir().join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","branch":"main","satisfied":[],"findings":[{"title":"gap","verdict":"confirmed","action":"pending","deliverable":"D1"}]}"#,
        )
        .unwrap();
        let (st, _) = finalize(&dirs, "main", &["20260101T000000Z-coverage.json".into()]);
        assert_eq!(st, StageStatus::NeedsAttention, "open gap is not Done");

        // A clean report + a real checklist -> Done, and the old file is gone.
        std::fs::write(
            dirs.findings_dir().join("20260101T000001Z-coverage.json"),
            r#"{"stage":"coverage","branch":"main","satisfied":["D1"],"findings":[]}"#,
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

    #[test]
    fn reconcile_flags_an_unjudged_unchecked_deliverable() {
        // D1 satisfied, D2 unchecked+unjudged, D3 already ticked.
        let plan = "# Plan\n\n## Deliverables\n\
                    - [ ] D1: a - accept: x\n\
                    - [ ] D2: b - accept: y\n\
                    - [x] D3: c - accept: z\n";
        let mut rep = CoverageReport {
            satisfied: vec!["D1".into()],
            gaps: Vec::new(),
        };
        reconcile_missing(&mut rep, plan);
        assert_eq!(rep.gaps.len(), 1, "only the unjudged unchecked D2 is a gap");
        assert_eq!(rep.gaps[0].deliverable, "D2");
        assert_eq!(rep.gaps[0].finding.verdict, "confirmed");
        assert_eq!(rep.gaps[0].finding.action, "pending");
    }

    #[test]
    fn reconcile_trusts_checked_and_case_insensitive_satisfied() {
        // A `[x]` item is never flagged; `satisfied` matches ids case-insensitively.
        let plan = "# Plan\n\n## Deliverables\n\
                    - [x] D1: a - accept: x\n\
                    - [ ] D2: b - accept: y\n";
        let mut rep = CoverageReport {
            satisfied: vec!["d2".into()], // lowercase still matches D2
            gaps: Vec::new(),
        };
        reconcile_missing(&mut rep, plan);
        assert!(rep.gaps.is_empty(), "D1 checked, D2 satisfied -> no gaps");
    }

    #[test]
    fn reconcile_routes_synthetic_gaps_from_the_declared_route() {
        let plan = "# Plan\n\n## Deliverables\n\
                    - [ ] D1: file thing - accept: exists - route: src/a.rs\n\
                    - [ ] D2: plan thing - accept: ok - route: §Design\n\
                    - [ ] D3: no route - accept: ok\n";
        let mut rep = CoverageReport::default();
        reconcile_missing(&mut rep, plan);
        assert_eq!(rep.gaps.len(), 3);
        let d1 = rep.gaps.iter().find(|g| g.deliverable == "D1").unwrap();
        assert_eq!(
            d1.finding.file.as_deref(),
            Some("src/a.rs"),
            "code-fix route"
        );
        assert!(d1.finding.plan_step.is_none());
        let d2 = rep.gaps.iter().find(|g| g.deliverable == "D2").unwrap();
        assert_eq!(
            d2.finding.plan_step.as_deref(),
            Some("Design"),
            "plan-fix route"
        );
        assert!(d2.finding.file.is_none());
        let d3 = rep.gaps.iter().find(|g| g.deliverable == "D3").unwrap();
        assert!(
            d3.finding.file.is_none() && d3.finding.plan_step.is_none(),
            "no route -> unroutable (drive_gaps flags it)"
        );
    }

    #[test]
    fn latest_report_is_scoped_to_the_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // feat-a older (a gap), feat-b newer (clean).
        std::fs::write(
            dir.join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","branch":"feat-a","satisfied":[],"findings":[{"title":"gap","verdict":"confirmed","action":"pending","deliverable":"D1"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("20260101T000001Z-coverage.json"),
            r#"{"stage":"coverage","branch":"feat-b","satisfied":[],"findings":[]}"#,
        )
        .unwrap();
        // feat-a sees ITS gap, never feat-b's newer clean report.
        assert_eq!(latest_report(dir, "feat-a").unwrap().gaps.len(), 1);
        assert!(latest_report(dir, "feat-b").unwrap().gaps.is_empty());
    }

    #[test]
    fn latest_report_prefers_a_stamped_report_over_a_newer_branchless_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Older report stamped for feat-a (a gap); NEWER branch-less clean report.
        std::fs::write(
            dir.join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","branch":"feat-a","satisfied":[],"findings":[{"title":"gap","verdict":"confirmed","action":"pending","deliverable":"D1"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("20260101T000002Z-coverage.json"),
            r#"{"stage":"coverage","satisfied":[],"findings":[]}"#,
        )
        .unwrap();
        // feat-a's real (gap) report wins over the newer branch-less clean one.
        assert_eq!(latest_report(dir, "feat-a").unwrap().gaps.len(), 1);
        // A branch with no stamped report still falls back to the branch-less one.
        assert!(latest_report(dir, "feat-c").unwrap().gaps.is_empty());
    }

    #[test]
    fn finalize_supersede_leaves_other_branches_coverage() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(dirs.findings_dir()).unwrap();
        std::fs::create_dir_all(dirs.feature_dir("main")).unwrap();
        std::fs::write(
            dirs.plan_file("main"),
            "# Plan\n\n## Deliverables\n- [ ] D1: x - accept: y\n",
        )
        .unwrap();
        // Another feature's coverage file must survive main's finalize sweep.
        std::fs::write(
            dirs.findings_dir().join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","branch":"feat-b","satisfied":[],"findings":[]}"#,
        )
        .unwrap();
        std::fs::write(
            dirs.findings_dir().join("20260101T000001Z-coverage.json"),
            r#"{"stage":"coverage","branch":"main","satisfied":["D1"],"findings":[]}"#,
        )
        .unwrap();
        let _ = finalize(&dirs, "main", &["20260101T000001Z-coverage.json".into()]);
        assert!(
            dirs.findings_dir()
                .join("20260101T000000Z-coverage.json")
                .exists(),
            "feat-b's coverage is not superseded by main's run"
        );
    }
}
