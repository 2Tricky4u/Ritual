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
        // Gaps remain but every one is stuck.
        return RoundAction::Stuck(state.stuck.iter().cloned().collect());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::{CoverageReport, Gap};
    use crate::findings::Finding;

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
