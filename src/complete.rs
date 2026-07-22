//! The pure round-planner for `ritual complete`: given the latest coverage
//! report and the loop's running state, decide the next action - drive some
//! deliverables, declare the feature done, give up on stuck ones, or hit a
//! bound. Kept free of I/O so the loop's control flow is unit-testable (the repo
//! pattern: `code_fix`, `spec`). The async driver in `run_cmd` reuses this plus
//! the existing fix-command builders and `follow_run`.

use std::collections::{BTreeMap, BTreeSet};

use crate::coverage::CoverageReport;

/// The loop's hard bounds (from `[complete]` config).
#[derive(Debug, Clone)]
pub struct Bounds {
    pub max_rounds: u32,
    /// Consecutive zero-gap coverage runs required to declare done (loop-until-dry).
    pub clean_rounds: u32,
    /// Gaps driven per round (small batches force each fix pass to finish).
    pub round_scope: u32,
    /// Attempts on one deliverable before it is marked STUCK.
    pub max_attempts: u32,
}

/// The loop's running state. Per-invocation only - NOT persisted across separate
/// `ritual complete` runs; a re-run re-judges the tree from scratch (the round /
/// attempt / stuck counters reset), which is safe because coverage is idempotent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriveState {
    pub round: u32,
    pub clean_streak: u32,
    pub attempts: BTreeMap<String, u32>,
    pub stuck: BTreeSet<String>,
}

/// What the driver should do after a coverage run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundAction {
    /// Drive these deliverable ids this round. Empty = drive nothing, just
    /// re-run coverage (a confirming clean round, or a round whose whole batch
    /// just went stuck).
    Drive(Vec<String>),
    /// All deliverables satisfied for `clean_rounds` consecutive runs.
    Done,
    /// Only unfixable (stuck) deliverables remain; a human must step in.
    Stuck(Vec<String>),
    /// The round cap was hit before completion.
    MaxRounds,
}

/// Decide the next action from the latest report, mutating the loop state
/// (round counter, per-deliverable attempts, stuck set, clean streak).
pub fn plan_round(state: &mut DriveState, report: &CoverageReport, b: &Bounds) -> RoundAction {
    state.round += 1;
    if state.round > b.max_rounds {
        return RoundAction::MaxRounds;
    }

    // Deliverable ids with an open gap that we have not given up on.
    let active: Vec<String> = report
        .gaps
        .iter()
        .map(|g| g.deliverable.clone())
        .filter(|d| !state.stuck.contains(d))
        .collect();

    if active.is_empty() {
        if report.gaps.is_empty() {
            state.clean_streak += 1;
            if state.clean_streak >= b.clean_rounds.max(1) {
                return RoundAction::Done;
            }
            return RoundAction::Drive(Vec::new()); // confirming clean round
        }
        // Gaps remain but every one is stuck. Report the STILL-OPEN gaps,
        // not the historical stuck set: a deliverable that went stuck in an
        // earlier round but was since fixed incidentally is no longer the
        // human's problem.
        let still_open: BTreeSet<String> =
            report.gaps.iter().map(|g| g.deliverable.clone()).collect();
        return RoundAction::Stuck(still_open.into_iter().collect());
    }

    state.clean_streak = 0;
    let mut batch = Vec::new();
    for d in active.into_iter().take(b.round_scope.max(1) as usize) {
        let n = state.attempts.entry(d.clone()).or_insert(0);
        *n += 1;
        if *n > b.max_attempts {
            state.stuck.insert(d); // one too many attempts: give up on this one
        } else {
            batch.push(d);
        }
    }
    // `batch` may be empty (the whole scope just went stuck); the driver then
    // simply re-runs coverage and the next round re-evaluates.
    RoundAction::Drive(batch)
}

/// Self-certification gate for the plan-fix: a checklist item the fix flipped
/// from `- [ ]` to `- [x]`. CHECKED items are trusted by
/// `coverage::reconcile_missing` and skipped by the judge, so an agent ticking
/// its own deliverable fabricates completeness with zero verification - the
/// caller reverts the plan when this returns Some. Duplicate item texts are
/// handled by count (a flip raises the checked count of a previously
/// unchecked text).
pub fn illegal_tick(before: &str, after: &str) -> Option<String> {
    fn items(text: &str, checked: bool) -> std::collections::HashMap<String, usize> {
        let mark = if checked { "[x]" } else { "[ ]" };
        let mut map = std::collections::HashMap::new();
        for line in text.lines() {
            let t = line.trim_start();
            for pre in ["- ", "* "] {
                if let Some(rest) = t.strip_prefix(pre)
                    && rest.to_ascii_lowercase().starts_with(mark)
                {
                    *map.entry(rest[3..].trim().to_string()).or_insert(0usize) += 1;
                }
            }
        }
        map
    }
    let checked_before = items(before, true);
    let unchecked_before = items(before, false);
    for (item, n_after) in items(after, true) {
        let n_before = checked_before.get(&item).copied().unwrap_or(0);
        if n_after > n_before && unchecked_before.contains_key(&item) {
            return Some(item);
        }
    }
    // Aggregate backstop: the unchecked count may only grow (unchecking,
    // adding items). A net decrease is a tick under a reworded text or a
    // deleted deliverable - self-certification the exact-text check above
    // can't see. Legitimate rewording keeps the item unchecked, so the
    // total is preserved and passes.
    let unchecked_after = items(after, false);
    let n_before: usize = unchecked_before.values().sum();
    let n_after: usize = unchecked_after.values().sum();
    if n_after < n_before {
        return unchecked_before
            .iter()
            .find(|(t, n)| unchecked_after.get(*t).copied().unwrap_or(0) < **n)
            .map(|(t, _)| t.clone())
            .or_else(|| Some(String::new()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::{CoverageReport, Gap};
    use crate::findings::Finding;

    #[test]
    fn illegal_tick_catches_a_self_certified_deliverable() {
        let before = "## Deliverables\n- [ ] D1: parser handles drift\n- [x] D2: docs\n";
        let after = "## Deliverables\n- [x] D1: parser handles drift\n- [x] D2: docs\n";
        assert_eq!(
            illegal_tick(before, after).as_deref(),
            Some("D1: parser handles drift")
        );
        // Case-insensitive mark, `*` bullets, indentation.
        let b2 = "  * [ ] item\n";
        let a2 = "  * [X] item\n";
        assert_eq!(illegal_tick(b2, a2).as_deref(), Some("item"));
    }

    #[test]
    fn illegal_tick_catches_reword_and_tick_and_deletion() {
        let before = "## Deliverables\n- [ ] D1: parser handles drift\n- [x] D2: docs\n";
        // Reword-and-tick: the unchecked text vanishes and a checked one
        // appears - the unchecked count dropped, so the gate fires.
        let reworded = "## Deliverables\n- [x] D1: parser handles input drift\n- [x] D2: docs\n";
        assert_eq!(
            illegal_tick(before, reworded).as_deref(),
            Some("D1: parser handles drift")
        );
        // Deleting the unchecked deliverable outright is the same dodge.
        let deleted = "## Deliverables\n- [x] D2: docs\n";
        assert_eq!(
            illegal_tick(before, deleted).as_deref(),
            Some("D1: parser handles drift")
        );
    }

    #[test]
    fn illegal_tick_allows_everything_else() {
        // Unchanged, text edits, UNchecking, and adding new unchecked items
        // are all legitimate plan-fix moves.
        let before = "- [ ] D1\n- [x] D2\n";
        assert_eq!(illegal_tick(before, before), None);
        assert_eq!(
            illegal_tick(before, "- [ ] D1 (reworded)\n- [x] D2\n"),
            None
        );
        assert_eq!(illegal_tick(before, "- [ ] D1\n- [ ] D2\n"), None);
        assert_eq!(illegal_tick(before, "- [ ] D1\n- [x] D2\n- [ ] D3\n"), None);
        // A brand-new CHECKED item was never unchecked before: not a flip.
        assert_eq!(illegal_tick(before, "- [ ] D1\n- [x] D2\n- [x] D9\n"), None);
        // Duplicate texts: flipping one of two identical unchecked items IS
        // caught (checked count rose for a text that was unchecked).
        let dup_b = "- [ ] same\n- [ ] same\n";
        let dup_a = "- [x] same\n- [ ] same\n";
        assert_eq!(illegal_tick(dup_b, dup_a).as_deref(), Some("same"));
    }

    fn report(ids: &[&str]) -> CoverageReport {
        CoverageReport {
            satisfied: Vec::new(),
            gaps: ids
                .iter()
                .map(|d| Gap {
                    deliverable: d.to_string(),
                    finding: Finding::default(),
                })
                .collect(),
        }
    }

    fn bounds() -> Bounds {
        Bounds {
            max_rounds: 5,
            clean_rounds: 1,
            round_scope: 2,
            max_attempts: 2,
        }
    }

    #[test]
    fn drives_up_to_scope_then_declares_done_on_a_clean_run() {
        let mut st = DriveState::default();
        let a = plan_round(&mut st, &report(&["D1", "D2", "D3"]), &bounds());
        assert_eq!(
            a,
            RoundAction::Drive(vec!["D1".into(), "D2".into()]),
            "scope=2"
        );
        // Next run: no gaps -> done (clean_rounds=1).
        assert_eq!(
            plan_round(&mut st, &report(&[]), &bounds()),
            RoundAction::Done
        );
    }

    #[test]
    fn needs_two_clean_runs_when_clean_rounds_is_two() {
        let mut b = bounds();
        b.clean_rounds = 2;
        let mut st = DriveState::default();
        assert_eq!(
            plan_round(&mut st, &report(&[]), &b),
            RoundAction::Drive(vec![])
        );
        assert_eq!(plan_round(&mut st, &report(&[]), &b), RoundAction::Done);
    }

    #[test]
    fn a_recurring_gap_goes_stuck_after_the_attempt_cap() {
        let b = bounds(); // max_attempts=2
        let mut st = DriveState::default();
        // D1 keeps coming back each round.
        assert_eq!(
            plan_round(&mut st, &report(&["D1"]), &b),
            RoundAction::Drive(vec!["D1".into()])
        );
        assert_eq!(
            plan_round(&mut st, &report(&["D1"]), &b),
            RoundAction::Drive(vec!["D1".into()])
        );
        // Third sighting exceeds max_attempts=2 -> stuck, batch empty this round.
        assert_eq!(
            plan_round(&mut st, &report(&["D1"]), &b),
            RoundAction::Drive(vec![])
        );
        // Now only-stuck gaps remain -> Stuck.
        assert_eq!(
            plan_round(&mut st, &report(&["D1"]), &b),
            RoundAction::Stuck(vec!["D1".into()])
        );
    }

    #[test]
    fn other_gaps_progress_while_one_goes_stuck() {
        let b = Bounds {
            max_rounds: 9,
            clean_rounds: 1,
            round_scope: 3,
            max_attempts: 1,
        };
        let mut st = DriveState::default();
        // D1 driven once (attempt 1).
        assert_eq!(
            plan_round(&mut st, &report(&["D1"]), &b),
            RoundAction::Drive(vec!["D1".into()])
        );
        // D1 recurs -> exceeds max_attempts=1 -> stuck; D2 newly appears -> driven.
        let a = plan_round(&mut st, &report(&["D1", "D2"]), &b);
        assert_eq!(
            a,
            RoundAction::Drive(vec!["D2".into()]),
            "D1 stuck, D2 progresses"
        );
        assert!(st.stuck.contains("D1"));
    }

    #[test]
    fn caps_at_max_rounds() {
        let mut b = bounds();
        b.max_rounds = 1;
        let mut st = DriveState::default();
        assert!(matches!(
            plan_round(&mut st, &report(&["D1"]), &b),
            RoundAction::Drive(_)
        ));
        assert_eq!(
            plan_round(&mut st, &report(&["D1"]), &b),
            RoundAction::MaxRounds
        );
    }
}
