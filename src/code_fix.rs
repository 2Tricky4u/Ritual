//! The pure gate logic for the code-fix batch, kept free of async and App so it
//! can be exhaustively unit-tested (the repo pattern: `answers`,
//! `spec::edits_confined_multi`). The App drives the phases through the AppMsg
//! channel and calls `advance`/`decide` at each transition.
//!
//! Flow: the LLM fixes all queued code findings in ONE run, then two gates run
//! in series - `./check.sh` (full), then an independent read-only re-review of
//! the diff. A batch is accepted only if BOTH gates pass and every finding is
//! confirmed resolved; ANY failure reverts the WHOLE batch (findings stay
//! queued). Accept is strict all-or-nothing.

use std::collections::HashMap;

use crate::answers::AnswerVerdict;
use crate::review::{FindingReview, ReviewVerdict};

/// Which leg of the pipeline is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePhase {
    Fixing,
    Checking,
    Reviewing,
}

/// What just happened, fed to `advance`.
#[derive(Debug, Clone)]
pub enum GateEvent {
    /// The fix run finished; `answers` are its parsed ANSWERS verdicts,
    /// `tree_changed` is whether it actually edited anything.
    FixOk {
        answers: HashMap<u32, AnswerVerdict>,
        tree_changed: bool,
    },
    FixFailed(String),
    CheckGreen,
    CheckRed(String),
    ReviewOk(ReviewVerdict),
    ReviewFailed(String),
}

/// What the App should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    SpawnCheck,
    SpawnReview,
    /// Accept the batch: mark exactly these finding numbers fixed.
    Accept(Vec<u32>),
    /// Revert the whole batch (git restore) with this reason for the status.
    Revert(String),
}

/// Advance the state machine. Returns the next phase (`None` = terminal) and the
/// step the App must take. `numbers` is the full set of targeted finding
/// numbers and `answers` the stored ANSWERS verdicts from the fix run, both
/// needed for the strict `decide` in the Reviewing leg.
pub fn advance(
    phase: CodePhase,
    event: GateEvent,
    numbers: &[u32],
    answers: &HashMap<u32, AnswerVerdict>,
) -> (Option<CodePhase>, Step) {
    match (phase, event) {
        (CodePhase::Fixing, GateEvent::FixOk { tree_changed, .. }) => {
            if !tree_changed {
                // A "FIXED" claim with no edits can't have fixed anything.
                (None, Step::Revert("the fix run changed nothing".into()))
            } else {
                // DECLINED findings are allowed through here; the gates below
                // (check.sh + re-review) decide what is actually accepted.
                (Some(CodePhase::Checking), Step::SpawnCheck)
            }
        }
        (CodePhase::Fixing, GateEvent::FixFailed(r)) => (None, Step::Revert(r)),
        (CodePhase::Checking, GateEvent::CheckGreen) => {
            (Some(CodePhase::Reviewing), Step::SpawnReview)
        }
        (CodePhase::Checking, GateEvent::CheckRed(tail)) => {
            (None, Step::Revert(format!("check.sh failed: {tail}")))
        }
        (CodePhase::Reviewing, GateEvent::ReviewOk(v)) => (None, decide(answers, &v, numbers)),
        (CodePhase::Reviewing, GateEvent::ReviewFailed(r)) => {
            (None, Step::Revert(format!("re-review run failed: {r}")))
        }
        // Any out-of-order event is a defensive revert.
        (_, other) => (
            None,
            Step::Revert(format!("unexpected gate event: {other:?}")),
        ),
    }
}

/// The strict accept/reject rule (locked): the batch is accepted ONLY if the
/// re-review flags no regressions AND every targeted finding is BOTH claimed
/// FIXED by the fix run AND confirmed RESOLVED by the re-review. Otherwise the
/// whole batch is reverted.
pub fn decide(
    answers: &HashMap<u32, AnswerVerdict>,
    review: &ReviewVerdict,
    numbers: &[u32],
) -> Step {
    if let Some(desc) = &review.regressions {
        return Step::Revert(format!("re-review flagged regressions: {desc}"));
    }
    for &n in numbers {
        let claimed_fixed = matches!(answers.get(&n), Some(AnswerVerdict::Fixed));
        let confirmed = matches!(review.per_finding.get(&n), Some(FindingReview::Resolved));
        if !claimed_fixed {
            return Step::Revert(format!("#{n} was not fixed by the run"));
        }
        if !confirmed {
            let why = match review.per_finding.get(&n) {
                Some(FindingReview::Unresolved(r)) => r.clone(),
                _ => "re-review gave no verdict".to_string(),
            };
            return Step::Revert(format!("#{n} not confirmed resolved: {why}"));
        }
    }
    Step::Accept(numbers.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(nums: &[u32]) -> HashMap<u32, AnswerVerdict> {
        nums.iter().map(|&n| (n, AnswerVerdict::Fixed)).collect()
    }

    fn review(resolved: &[u32], regressions: Option<&str>) -> ReviewVerdict {
        ReviewVerdict {
            per_finding: resolved
                .iter()
                .map(|&n| (n, FindingReview::Resolved))
                .collect(),
            regressions: regressions.map(str::to_string),
        }
    }

    #[test]
    fn advance_fixing_to_checking_when_tree_changed() {
        let a = fixed(&[1]);
        let (phase, step) = advance(
            CodePhase::Fixing,
            GateEvent::FixOk {
                answers: a.clone(),
                tree_changed: true,
            },
            &[1],
            &a,
        );
        assert_eq!(phase, Some(CodePhase::Checking));
        assert_eq!(step, Step::SpawnCheck);
    }

    #[test]
    fn advance_fix_no_change_reverts() {
        let a = fixed(&[1]);
        let (phase, step) = advance(
            CodePhase::Fixing,
            GateEvent::FixOk {
                answers: a.clone(),
                tree_changed: false,
            },
            &[1],
            &a,
        );
        assert_eq!(phase, None);
        assert!(matches!(step, Step::Revert(_)));
    }

    #[test]
    fn advance_fix_failed_reverts() {
        let a = fixed(&[1]);
        let (_, step) = advance(
            CodePhase::Fixing,
            GateEvent::FixFailed("budget".into()),
            &[1],
            &a,
        );
        assert_eq!(step, Step::Revert("budget".into()));
    }

    #[test]
    fn advance_check_red_reverts_green_reviews() {
        let a = fixed(&[1]);
        let (p, s) = advance(CodePhase::Checking, GateEvent::CheckGreen, &[1], &a);
        assert_eq!(p, Some(CodePhase::Reviewing));
        assert_eq!(s, Step::SpawnReview);
        let (p, s) = advance(
            CodePhase::Checking,
            GateEvent::CheckRed("clippy".into()),
            &[1],
            &a,
        );
        assert_eq!(p, None);
        assert!(matches!(s, Step::Revert(r) if r.contains("check.sh")));
    }

    #[test]
    fn advance_review_ok_runs_decide() {
        let a = fixed(&[1]);
        let (p, s) = advance(
            CodePhase::Reviewing,
            GateEvent::ReviewOk(review(&[1], None)),
            &[1],
            &a,
        );
        assert_eq!(p, None);
        assert_eq!(s, Step::Accept(vec![1]));
    }

    #[test]
    fn decide_accepts_only_when_all_fixed_and_resolved_and_no_regressions() {
        assert_eq!(
            decide(&fixed(&[1, 2]), &review(&[1, 2], None), &[1, 2]),
            Step::Accept(vec![1, 2])
        );
    }

    #[test]
    fn decide_reverts_on_any_unresolved() {
        let r = ReviewVerdict {
            per_finding: [
                (1, FindingReview::Resolved),
                (2, FindingReview::Unresolved("still broken".into())),
            ]
            .into(),
            regressions: None,
        };
        assert!(matches!(
            decide(&fixed(&[1, 2]), &r, &[1, 2]),
            Step::Revert(_)
        ));
    }

    #[test]
    fn decide_reverts_when_a_finding_was_declined() {
        let mut a = fixed(&[1]);
        a.insert(2, AnswerVerdict::Declined("couldn't".into()));
        assert!(matches!(
            decide(&a, &review(&[1, 2], None), &[1, 2]),
            Step::Revert(_)
        ));
    }

    #[test]
    fn decide_reverts_on_regressions_even_if_all_resolved() {
        assert!(matches!(
            decide(&fixed(&[1]), &review(&[1], Some("breaks the parser")), &[1]),
            Step::Revert(r) if r.contains("regressions")
        ));
    }

    #[test]
    fn decide_reverts_on_empty_review() {
        assert!(matches!(
            decide(&fixed(&[1]), &ReviewVerdict::default(), &[1]),
            Step::Revert(_)
        ));
    }
}
