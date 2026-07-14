//! The pure gate logic for the code-fix batch, kept free of async and App so it
//! can be exhaustively unit-tested (the repo pattern: `answers`,
//! `spec::edits_confined_multi`). The App drives the phases through the AppMsg
//! channel and calls `advance`/`decide` at each transition.
//!
//! Flow: the LLM fixes all queued code findings in ONE run, then two gates run
//! in series - `./check.sh` (full), then an independent read-only re-review of
//! the produced diff. The attempt is ALWAYS left in the working tree (git is
//! the undo); accept/fail is pure bookkeeping over that one inseparable diff.
//! A finding is marked fixed only if it was claimed FIXED by the run AND
//! confirmed RESOLVED by the re-review; every other targeted finding stays
//! queued (annotated with the review's reason). Because the diff cannot be
//! split, a reported REGRESSION fails the WHOLE batch (nothing is accepted); so
//! do check.sh red, an empty change set, and the fixer moving HEAD (the latter
//! two are caught by the App before the gates even run).

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
    /// The fix run finished cleanly; `answers` are its parsed ANSWERS verdicts.
    /// Whether it actually changed anything is decided by the App via content
    /// hashing (`git::observed_change`), which works even on untracked or
    /// gitignored targets; an empty change set fails the batch before the gates.
    FixOk {
        answers: HashMap<u32, AnswerVerdict>,
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
    /// Accept these finding numbers: mark exactly them fixed. Any targeted
    /// finding NOT listed stays queued (the App annotates it with the reason).
    Accept(Vec<u32>),
    /// Fail the whole batch with this reason: NO finding is marked fixed and the
    /// attempt is LEFT in the working tree (git is the undo - despite the name,
    /// this never runs `git restore`).
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
        (CodePhase::Fixing, GateEvent::FixOk { .. }) => {
            // DECLINED findings are allowed through; the gates below (check.sh
            // + re-review) decide what is actually accepted.
            (Some(CodePhase::Checking), Step::SpawnCheck)
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

/// The per-finding accept rule. A reported regression fails the whole batch (the
/// single diff can't be split, so nothing can be safely kept). Otherwise accept
/// exactly the findings that were BOTH claimed FIXED by the run AND confirmed
/// RESOLVED by the re-review; every other targeted finding stays queued (the App
/// annotates it with the review's reason). If nothing qualifies, the batch fails
/// (the attempt stays in the tree either way). The "no observable change" and
/// "fixer moved HEAD" failures are caught earlier by the App, before the gates.
pub fn decide(
    answers: &HashMap<u32, AnswerVerdict>,
    review: &ReviewVerdict,
    numbers: &[u32],
) -> Step {
    if let Some(desc) = &review.regressions {
        return Step::Revert(format!("re-review flagged regressions: {desc}"));
    }
    let accepted: Vec<u32> = numbers
        .iter()
        .copied()
        .filter(|n| {
            matches!(answers.get(n), Some(AnswerVerdict::Fixed))
                && matches!(review.per_finding.get(n), Some(FindingReview::Resolved))
        })
        .collect();
    if accepted.is_empty() {
        return Step::Revert("no finding was both fixed by the run and confirmed resolved".into());
    }
    Step::Accept(accepted)
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
    fn advance_fixing_to_checking() {
        let a = fixed(&[1]);
        let (phase, step) = advance(
            CodePhase::Fixing,
            GateEvent::FixOk { answers: a.clone() },
            &[1],
            &a,
        );
        assert_eq!(phase, Some(CodePhase::Checking));
        assert_eq!(step, Step::SpawnCheck);
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
    fn decide_accepts_the_resolved_subset_and_requeues_the_rest() {
        let r = ReviewVerdict {
            per_finding: [
                (1, FindingReview::Resolved),
                (2, FindingReview::Unresolved("still broken".into())),
            ]
            .into(),
            regressions: None,
        };
        // #1 is accepted; #2 stays queued (the App annotates it with the reason).
        assert_eq!(decide(&fixed(&[1, 2]), &r, &[1, 2]), Step::Accept(vec![1]));
    }

    #[test]
    fn decide_accepts_the_fixed_one_and_requeues_a_declined_finding() {
        let mut a = fixed(&[1]);
        a.insert(2, AnswerVerdict::Declined("couldn't".into()));
        assert_eq!(
            decide(&a, &review(&[1, 2], None), &[1, 2]),
            Step::Accept(vec![1])
        );
    }

    #[test]
    fn decide_reverts_when_nothing_qualifies() {
        let r = ReviewVerdict {
            per_finding: [(1, FindingReview::Unresolved("nope".into()))].into(),
            regressions: None,
        };
        assert!(matches!(decide(&fixed(&[1]), &r, &[1]), Step::Revert(_)));
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
