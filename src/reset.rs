//! Reset a feature's plan back to just the spec: delete `plan.md`, reset the
//! plan-derived pipeline stages to Pending, remove this feature's plan-review
//! and coverage findings, and clear the plan undo/redo stacks. The spec and ALL
//! git-tracked code are left untouched - this is a "re-plan from scratch", never
//! a code wipe.

use crate::state::{self, RitualDirs, StageId, StageStatus, State};

/// Stages downstream of `spec` that a plan reset clears back to Pending.
pub const PLAN_DERIVED: &[StageId] = &[
    StageId::Plan,
    StageId::PlanReview,
    StageId::TestsRed,
    StageId::Implement,
    StageId::DualReview,
    StageId::Coverage,
];

/// Findings stages tied to the plan (not to code): removed on reset.
const PLAN_FINDING_STAGES: &[&str] = &["plan-review", "coverage"];

/// What a reset touched (for the caller's report).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResetSummary {
    pub plan_deleted: bool,
    pub stages_reset: usize,
    pub findings_removed: usize,
}

/// Reset the plan for `branch`. Mutates `st` (the caller saves it). Idempotent:
/// re-running on an already-reset feature is a harmless no-op.
pub fn reset_plan(dirs: &RitualDirs, st: &mut State, branch: &str) -> ResetSummary {
    let slug = state::branch_slug(branch);

    // 1. Delete plan.md.
    let plan_deleted = std::fs::remove_file(dirs.plan_file(&slug)).is_ok();

    // 2. Reset the plan-derived stages to Pending.
    let feature = st.feature_for_branch_mut(branch);
    let mut stages_reset = 0;
    for id in PLAN_DERIVED {
        let entry = feature.stages.entry(*id).or_default();
        if entry.status != StageStatus::Pending {
            stages_reset += 1;
        }
        *entry = Default::default(); // Pending, no timestamps/runs/session
    }
    feature.updated_at = chrono::Utc::now();

    // 3. Remove this feature's plan-review + coverage findings.
    let findings_removed = remove_plan_findings(dirs, &slug);

    // 4. Clear the plan undo/redo stacks (they snapshot the now-deleted plan).
    crate::undo::clear(dirs, &slug, "plan");

    ResetSummary {
        plan_deleted,
        stages_reset,
        findings_removed,
    }
}

/// Delete the plan-review/coverage findings files that belong to this feature,
/// matched EXACTLY by the file's `branch`. A destructive op must never touch a
/// branch-LESS file (its owner is ambiguous - it may belong to another feature;
/// the findings dir is shared). Such leftovers are harmless: coverage supersedes
/// its file on the next run, and plan-review re-writes its own. Code
/// (dual-review) findings and other features' files always survive.
fn remove_plan_findings(dirs: &RitualDirs, slug: &str) -> usize {
    let mut removed = 0;
    for lf in crate::findings::load_all(&dirs.findings_dir()).unwrap_or_default() {
        let belongs = !lf.file.branch.is_empty() && state::branch_slug(&lf.file.branch) == slug;
        if belongs
            && PLAN_FINDING_STAGES.contains(&lf.file.stage.as_str())
            && std::fs::remove_file(&lf.path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// A dry-run preview: does a plan exist, how many stages are past Pending, and
/// how many plan findings would be removed - used by the CLI/TUI confirm text.
pub fn preview(dirs: &RitualDirs, st: &State, branch: &str) -> ResetSummary {
    let slug = state::branch_slug(branch);
    let plan_deleted = dirs.plan_file(&slug).exists();
    let stages_reset = st
        .features
        .get(&slug)
        .map(|f| {
            PLAN_DERIVED
                .iter()
                .filter(|id| f.stage(**id).status != StageStatus::Pending)
                .count()
        })
        .unwrap_or(0);
    let findings_removed = crate::findings::load_all(&dirs.findings_dir())
        .unwrap_or_default()
        .iter()
        .filter(|lf| {
            !lf.file.branch.is_empty()
                && state::branch_slug(&lf.file.branch) == slug
                && PLAN_FINDING_STAGES.contains(&lf.file.stage.as_str())
        })
        .count();
    ResetSummary {
        plan_deleted,
        stages_reset,
        findings_removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs_tmp() -> (tempfile::TempDir, RitualDirs) {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(dirs.findings_dir()).unwrap();
        (tmp, dirs)
    }

    #[test]
    fn reset_wipes_plan_stages_and_plan_findings_but_keeps_code() {
        let (_t, dirs) = dirs_tmp();
        let mut st = State::default();
        let feat = st.feature_for_branch_mut("main");
        // Everything ran through coverage.
        for id in PLAN_DERIVED {
            feat.stages.entry(*id).or_default().status = StageStatus::Done;
        }
        feat.stages.entry(StageId::Spec).or_default().status = StageStatus::Done;
        // A plan + a plan-review finding + a coverage finding + a CODE finding.
        std::fs::create_dir_all(dirs.feature_dir("main")).unwrap();
        std::fs::write(dirs.plan_file("main"), "# Plan\n").unwrap();
        crate::undo::push(&dirs, "main", "plan", "old").unwrap();
        for (ts, stage) in [
            ("20260101T000000Z", "plan-review"),
            ("20260101T000001Z", "coverage"),
            ("20260101T000002Z", "dual-review"),
        ] {
            std::fs::write(
                dirs.findings_dir().join(format!("{ts}-{stage}.json")),
                format!(r#"{{"stage":"{stage}","branch":"main","findings":[]}}"#),
            )
            .unwrap();
        }

        let sum = reset_plan(&dirs, &mut st, "main");
        assert!(sum.plan_deleted);
        assert_eq!(sum.stages_reset, PLAN_DERIVED.len());
        assert_eq!(
            sum.findings_removed, 2,
            "plan-review + coverage, not dual-review"
        );

        // plan.md gone; spec-stage untouched; downstream all Pending.
        assert!(!dirs.plan_file("main").exists());
        let feat = st.features.get("main").unwrap();
        assert_eq!(feat.stage(StageId::Spec).status, StageStatus::Done);
        for id in PLAN_DERIVED {
            assert_eq!(feat.stage(*id).status, StageStatus::Pending);
        }
        // The dual-review (code) finding survives.
        let left = crate::findings::load_all(&dirs.findings_dir()).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].file.stage, "dual-review");
        // Undo stack cleared.
        assert!(!dirs.feature_dir("main").join(".undo").join("plan").exists());
    }

    #[test]
    fn reset_only_removes_exact_branch_matches_not_branch_less_findings() {
        let (_t, dirs) = dirs_tmp();
        let mut st = State::default();
        st.feature_for_branch_mut("main");
        // A branch-less coverage finding (ambiguous owner - could be another
        // feature's) and this feature's own branch-tagged one.
        std::fs::write(
            dirs.findings_dir().join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","branch":"","findings":[]}"#,
        )
        .unwrap();
        std::fs::write(
            dirs.findings_dir().join("20260101T000001Z-coverage.json"),
            r#"{"stage":"coverage","branch":"main","findings":[]}"#,
        )
        .unwrap();
        let sum = reset_plan(&dirs, &mut st, "main");
        assert_eq!(
            sum.findings_removed, 1,
            "only the exact-branch match is removed"
        );
        assert!(
            dirs.findings_dir()
                .join("20260101T000000Z-coverage.json")
                .exists(),
            "branch-less finding survives (may belong to another feature)"
        );
        assert!(
            !dirs
                .findings_dir()
                .join("20260101T000001Z-coverage.json")
                .exists()
        );
    }

    #[test]
    fn reset_is_idempotent_on_a_fresh_feature() {
        let (_t, dirs) = dirs_tmp();
        let mut st = State::default();
        st.feature_for_branch_mut("main");
        let sum = reset_plan(&dirs, &mut st, "main");
        assert!(!sum.plan_deleted);
        assert_eq!(sum.stages_reset, 0);
        assert_eq!(sum.findings_removed, 0);
    }
}
