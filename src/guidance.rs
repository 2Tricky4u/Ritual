//! Pipeline guidance: is a Done stage STALE (spec/plan edited or the tree
//! changed after it ran), what blocks the next stage, and what's left before
//! "done". `compute` is 100% pure over [`Inputs`] (unit-testable, no I/O);
//! [`collect`] gathers the cheap inputs (two stats + one plan read +
//! in-memory scans). The expensive git probes (tree fingerprint, dual-review
//! preflight) arrive via [`Probe`], gathered off-thread by the TUI at event
//! cadence - guidance is CACHED on App and only read at render time.
//!
//! Guidance informs, never blocks: no launch gate consults it.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::findings::LoadedFindings;
use crate::state::{PIPELINE, RitualDirs, StageId, StageStatus, State};

/// Why a Done stage should be re-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    SpecChanged,
    PlanChanged,
    CodeChanged,
}

impl StaleReason {
    pub fn text(&self) -> &'static str {
        match self {
            StaleReason::SpecChanged => "spec.md changed after this ran",
            StaleReason::PlanChanged => "plan.md changed after this ran",
            StaleReason::CodeChanged => "the tree changed after this review",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StageGuidance {
    /// Set only for Done stages whose inputs moved since they finished.
    pub stale: Option<StaleReason>,
    /// Read-only mirrors of the launch gates: what running this stage NOW
    /// would refuse on (plan missing, deliverables gate, review preflight).
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PipelineGuidance {
    pub stages: BTreeMap<StageId, StageGuidance>,
    /// The first actionable stage: Pending/Failed/NeedsAttention in pipeline
    /// order, else the first Done-but-stale one. None while a run is live.
    pub next: Option<StageId>,
    /// One line about `next`: its top blocker, or why it went stale.
    pub next_note: Option<String>,
    /// Pipeline-level: open confirmed findings, coverage gaps, red check.
    pub warnings: Vec<String>,
    /// Architecture-map freshness for the Plan stage detail (None when the
    /// nudges are disabled).
    pub arch: Option<crate::architect::ArchStatus>,
}

/// One stage's persisted facts, snapshot for the pure core.
#[derive(Debug, Clone, Default)]
pub struct StageSnap {
    pub status: StageStatus,
    pub finished_at: Option<DateTime<Utc>>,
    pub runs: usize,
    pub fingerprint: Option<String>,
}

/// The expensive, git-touching inputs (gathered off the UI thread).
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// `provenance::tree_fingerprint` of the feature's checkout, now.
    pub fingerprint: Option<String>,
    /// `provenance::arch_fingerprint` (the `.ritual`-scoped variant) for the
    /// architecture-map staleness comparison.
    pub arch_fingerprint: Option<String>,
    /// `git::dual_review_preflight` error text, if it would refuse.
    pub dual_review_blocker: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub stages: BTreeMap<StageId, StageSnap>,
    pub spec_mtime: Option<DateTime<Utc>>,
    pub plan_mtime: Option<DateTime<Utc>>,
    pub plan_exists: bool,
    /// `spec::deliverables_gate` error on the current plan (None = passes
    /// or no plan on disk - the missing-plan blocker covers that).
    pub deliverables_err: Option<String>,
    pub current_fingerprint: Option<String>,
    pub dual_review_blocker: Option<String>,
    pub open_confirmed: usize,
    /// Reconciled gap count of the newest coverage report (None = no report).
    pub coverage_gaps: Option<usize>,
    /// A pinned tests-red session exists for implement to resume.
    pub tests_red_session: bool,
    pub check_red: bool,
    pub anything_running: bool,
    /// The architecture map has real content ([`crate::architect`]).
    pub arch_meaningful: bool,
    /// The map's generation stamp (sidecar), if any.
    pub arch_stamp: Option<String>,
    /// `provenance::arch_fingerprint` now (via the probe).
    pub arch_fingerprint: Option<String>,
    /// `[architect] enabled`: gate the missing/stale nudges entirely.
    pub arch_nudges: bool,
}

/// The pure core. Staleness applies to DONE stages only; a None fingerprint
/// (legacy state, non-git) is "unknown" and never stale.
pub fn compute(i: &Inputs) -> PipelineGuidance {
    let snap = |id: StageId| i.stages.get(&id).cloned().unwrap_or_default();
    let doc_moved =
        |mtime: Option<DateTime<Utc>>, fin: DateTime<Utc>| mtime.is_some_and(|m| m > fin);

    let mut stages: BTreeMap<StageId, StageGuidance> = BTreeMap::new();
    for id in PIPELINE {
        let s = snap(*id);
        let mut g = StageGuidance::default();
        if s.status == StageStatus::Done
            && let Some(fin) = s.finished_at
        {
            g.stale = match id {
                StageId::Plan => doc_moved(i.spec_mtime, fin).then_some(StaleReason::SpecChanged),
                // Spec wins when both docs moved (it is the upstream input).
                StageId::PlanReview | StageId::TestsRed => {
                    if doc_moved(i.spec_mtime, fin) {
                        Some(StaleReason::SpecChanged)
                    } else if doc_moved(i.plan_mtime, fin) {
                        Some(StaleReason::PlanChanged)
                    } else {
                        None
                    }
                }
                StageId::DualReview | StageId::Coverage => {
                    match (&s.fingerprint, &i.current_fingerprint) {
                        (Some(then), Some(now)) if then != now => Some(StaleReason::CodeChanged),
                        _ => None,
                    }
                }
                _ => None,
            };
        }
        match id {
            StageId::PlanReview | StageId::TestsRed => {
                if !i.plan_exists {
                    g.blockers.push("plan.md missing - run plan first".into());
                }
            }
            StageId::Implement => {
                // Only meaningful once tests-red actually ran.
                if snap(StageId::TestsRed).status == StageStatus::Done && !i.tests_red_session {
                    g.blockers.push(
                        "no pinned tests-red session - implement opens the resume picker".into(),
                    );
                }
            }
            StageId::DualReview => {
                if let Some(b) = &i.dual_review_blocker {
                    g.blockers.push(b.clone());
                }
            }
            StageId::Coverage => {
                if !i.plan_exists {
                    g.blockers.push("plan.md missing - run plan first".into());
                } else if let Some(e) = &i.deliverables_err {
                    g.blockers.push(e.clone());
                }
            }
            _ => {}
        }
        stages.insert(*id, g);
    }

    let (next, next_note) = if i.anything_running {
        (None, Some("a run is in flight".to_string()))
    } else {
        let next = PIPELINE
            .iter()
            .copied()
            .find(|id| {
                matches!(
                    snap(*id).status,
                    StageStatus::Pending | StageStatus::Failed | StageStatus::NeedsAttention
                )
            })
            .or_else(|| {
                PIPELINE
                    .iter()
                    .copied()
                    .find(|id| stages[id].stale.is_some())
            });
        let note = next.and_then(|id| {
            let g = &stages[&id];
            g.blockers.first().cloned().or_else(|| {
                g.stale
                    .map(|r| format!("{} - rerun {}", r.text(), id.label()))
            })
        });
        (next, note)
    };

    let mut warnings = Vec::new();
    if i.open_confirmed > 0 {
        warnings.push(format!(
            "{} confirmed finding(s) open (tab 2)",
            i.open_confirmed
        ));
    }
    if let Some(gaps) = i.coverage_gaps
        && gaps > 0
    {
        warnings.push(format!("{gaps} coverage gap(s) open"));
    }
    if i.check_red {
        warnings.push("check.sh is red".into());
    }
    // Doc hygiene last: pipeline problems outrank the architecture map.
    let arch = i.arch_nudges.then(|| {
        crate::architect::status(
            i.arch_meaningful,
            i.arch_stamp.as_deref(),
            i.arch_fingerprint.as_deref(),
        )
    });
    if let Some(note) = arch.and_then(crate::architect::note) {
        warnings.push(note.into());
    }

    PipelineGuidance {
        stages,
        next,
        next_note,
        warnings,
        arch,
    }
}

/// Gather the CHEAP inputs (2 stats + one plan read + in-memory scans);
/// the git-touching pieces ride in via `probe`.
#[allow(clippy::too_many_arguments)] // one flag per cheap fact; a params struct would just rename them
pub fn collect(
    dirs: &RitualDirs,
    state: &State,
    findings: &[LoadedFindings],
    slug: &str,
    probe: &Probe,
    check_red: bool,
    running: bool,
    arch_nudges: bool,
) -> Inputs {
    let mut stages = BTreeMap::new();
    if let Some(f) = state.features.get(slug) {
        for id in PIPELINE {
            let s = f.stage(*id);
            stages.insert(
                *id,
                StageSnap {
                    status: s.status,
                    finished_at: s.finished_at,
                    runs: s.runs.len(),
                    fingerprint: s.fingerprint,
                },
            );
        }
    }
    let mtime = |p: &std::path::Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .map(DateTime::<Utc>::from)
    };
    let plan_path = dirs.plan_file(slug);
    let plan_text = std::fs::read_to_string(&plan_path).ok();
    let deliverables_err = plan_text
        .as_deref()
        .and_then(|t| crate::spec::deliverables_gate(t).err());
    let coverage_gaps = crate::coverage::latest_report(&dirs.findings_dir(), slug).map(|mut r| {
        if let Some(t) = plan_text.as_deref() {
            crate::coverage::reconcile_missing(&mut r, t);
        }
        r.gaps.len()
    });
    Inputs {
        spec_mtime: mtime(&dirs.spec_file(slug)),
        plan_mtime: mtime(&plan_path),
        plan_exists: plan_path.exists(),
        deliverables_err,
        current_fingerprint: probe.fingerprint.clone(),
        dual_review_blocker: probe.dual_review_blocker.clone(),
        open_confirmed: crate::findings::open_confirmed_count(findings, slug),
        coverage_gaps,
        tests_red_session: state.stage_session_id(slug, StageId::TestsRed).is_some(),
        check_red,
        anything_running: running,
        arch_meaningful: crate::stages::meaningful_architecture(dirs).is_some(),
        arch_stamp: crate::architect::read_stamp(dirs),
        arch_fingerprint: probe.arch_fingerprint.clone(),
        arch_nudges,
        stages,
    }
}

/// "just now" / "3m ago" / "2h ago" / "4d ago".
pub fn rel_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let s = (now - then).num_seconds().max(0);
    match s {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn t0() -> DateTime<Utc> {
        "2026-07-16T12:00:00Z".parse().unwrap()
    }

    fn done(fin: DateTime<Utc>) -> StageSnap {
        StageSnap {
            status: StageStatus::Done,
            finished_at: Some(fin),
            runs: 1,
            fingerprint: None,
        }
    }

    fn base_inputs() -> Inputs {
        let mut i = Inputs {
            plan_exists: true,
            ..Default::default()
        };
        for id in PIPELINE {
            i.stages.insert(*id, done(t0()));
        }
        i
    }

    #[test]
    fn doc_edits_flag_the_dependent_stages_stale() {
        let mut i = base_inputs();
        i.spec_mtime = Some(t0() + Duration::hours(1)); // spec touched AFTER runs
        let g = compute(&i);
        assert_eq!(
            g.stages[&StageId::Plan].stale,
            Some(StaleReason::SpecChanged)
        );
        assert_eq!(
            g.stages[&StageId::PlanReview].stale,
            Some(StaleReason::SpecChanged)
        );
        assert_eq!(
            g.stages[&StageId::TestsRed].stale,
            Some(StaleReason::SpecChanged)
        );
        assert_eq!(g.stages[&StageId::DualReview].stale, None);
        // next = the first stale stage; the note says why.
        assert_eq!(g.next, Some(StageId::Plan));
        assert!(g.next_note.unwrap().contains("spec.md changed"));
    }

    #[test]
    fn plan_edit_flags_review_and_tests_but_spec_wins_when_both_moved() {
        let mut i = base_inputs();
        i.plan_mtime = Some(t0() + Duration::minutes(5));
        let g = compute(&i);
        assert_eq!(
            g.stages[&StageId::Plan].stale,
            None,
            "plan itself not doc-stale"
        );
        assert_eq!(
            g.stages[&StageId::PlanReview].stale,
            Some(StaleReason::PlanChanged)
        );
        i.spec_mtime = Some(t0() + Duration::minutes(9));
        let g = compute(&i);
        assert_eq!(
            g.stages[&StageId::PlanReview].stale,
            Some(StaleReason::SpecChanged),
            "spec is the upstream input"
        );
    }

    #[test]
    fn code_staleness_needs_both_fingerprints_and_inequality() {
        let mut i = base_inputs();
        i.stages.get_mut(&StageId::DualReview).unwrap().fingerprint = Some("a:1".into());
        i.current_fingerprint = Some("a:2".into());
        let g = compute(&i);
        assert_eq!(
            g.stages[&StageId::DualReview].stale,
            Some(StaleReason::CodeChanged)
        );
        // Legacy None fingerprint: unknown, NEVER stale.
        i.stages.get_mut(&StageId::DualReview).unwrap().fingerprint = None;
        assert_eq!(compute(&i).stages[&StageId::DualReview].stale, None);
        // Equal fingerprints: fresh.
        i.stages.get_mut(&StageId::DualReview).unwrap().fingerprint = Some("a:2".into());
        assert_eq!(compute(&i).stages[&StageId::DualReview].stale, None);
    }

    #[test]
    fn only_done_stages_can_be_stale() {
        let mut i = base_inputs();
        i.spec_mtime = Some(t0() + Duration::hours(1));
        i.stages.get_mut(&StageId::Plan).unwrap().status = StageStatus::Failed;
        assert_eq!(compute(&i).stages[&StageId::Plan].stale, None);
    }

    #[test]
    fn next_prefers_unfinished_over_stale_and_running_suspends() {
        let mut i = base_inputs();
        i.spec_mtime = Some(t0() + Duration::hours(1)); // plan is stale...
        i.stages.get_mut(&StageId::DualReview).unwrap().status = StageStatus::Failed;
        let g = compute(&i);
        assert_eq!(
            g.next,
            Some(StageId::DualReview),
            "...but Failed comes first"
        );
        i.anything_running = true;
        let g = compute(&i);
        assert_eq!(g.next, None);
        assert_eq!(g.next_note.as_deref(), Some("a run is in flight"));
    }

    #[test]
    fn blockers_mirror_the_launch_gates() {
        let mut i = base_inputs();
        i.plan_exists = false;
        i.stages.get_mut(&StageId::PlanReview).unwrap().status = StageStatus::Pending;
        let g = compute(&i);
        assert!(g.stages[&StageId::PlanReview].blockers[0].contains("plan.md missing"));
        assert!(g.stages[&StageId::Coverage].blockers[0].contains("plan.md missing"));
        assert_eq!(g.next, Some(StageId::PlanReview));
        assert!(g.next_note.unwrap().contains("plan.md missing"));

        let mut i = base_inputs();
        i.deliverables_err = Some("plan has no `## Deliverables` section".into());
        i.dual_review_blocker = Some("nothing to review: tree matches merge-base".into());
        i.tests_red_session = false;
        let g = compute(&i);
        assert!(g.stages[&StageId::Coverage].blockers[0].contains("Deliverables"));
        assert!(g.stages[&StageId::DualReview].blockers[0].contains("nothing to review"));
        assert!(g.stages[&StageId::Implement].blockers[0].contains("tests-red session"));
    }

    #[test]
    fn warnings_cover_findings_gaps_and_check() {
        let mut i = base_inputs();
        i.open_confirmed = 3;
        i.coverage_gaps = Some(2);
        i.check_red = true;
        let w = compute(&i).warnings;
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w[0].contains("3 confirmed finding(s)"));
        assert!(w[1].contains("2 coverage gap(s)"));
        assert!(w[2].contains("check.sh is red"));
        // Zero-value inputs produce no noise.
        assert!(compute(&base_inputs()).warnings.is_empty());
    }

    #[test]
    fn architect_nudges_ride_the_warnings() {
        let arch_warn = |i: &Inputs| {
            compute(i)
                .warnings
                .iter()
                .find(|w| w.contains("architecture.md"))
                .cloned()
        };

        // Missing map: nudge to generate one.
        let mut i = base_inputs();
        i.arch_nudges = true;
        let w = arch_warn(&i).expect("missing map nudges");
        assert!(w.contains("ritual architect"), "{w}");

        // Stale map: stamp != current scoped fingerprint.
        i.arch_meaningful = true;
        i.arch_stamp = Some("a:1".into());
        i.arch_fingerprint = Some("a:2".into());
        let w = arch_warn(&i).expect("stale map nudges");
        assert!(w.contains("stale"), "{w}");

        // Fresh: silent. Unknown (no stamp / non-git): silent, never stale.
        i.arch_fingerprint = Some("a:1".into());
        assert_eq!(arch_warn(&i), None, "fresh");
        i.arch_stamp = None;
        assert_eq!(arch_warn(&i), None, "no stamp = unknown");
        i.arch_stamp = Some("a:1".into());
        i.arch_fingerprint = None;
        assert_eq!(arch_warn(&i), None, "non-git = unknown");

        // [architect] enabled=false silences even a missing map.
        let mut off = base_inputs();
        off.arch_nudges = false;
        assert_eq!(arch_warn(&off), None, "nudges disabled");

        // Pipeline problems outrank doc hygiene: check_red comes first.
        let mut both = base_inputs();
        both.arch_nudges = true;
        both.check_red = true;
        let w = compute(&both).warnings;
        assert!(w[0].contains("check.sh"), "{w:?}");
        assert!(w[1].contains("architecture.md"), "{w:?}");
    }

    #[test]
    fn arch_status_is_cached_for_the_stage_detail() {
        use crate::architect::ArchStatus;
        let mut i = base_inputs();
        i.arch_nudges = true;
        i.arch_meaningful = true;
        i.arch_stamp = Some("a:1".into());
        i.arch_fingerprint = Some("a:2".into());
        assert_eq!(compute(&i).arch, Some(ArchStatus::Stale));
        i.arch_fingerprint = Some("a:1".into());
        assert_eq!(compute(&i).arch, Some(ArchStatus::Fresh));
        i.arch_meaningful = false;
        assert_eq!(compute(&i).arch, Some(ArchStatus::Missing));
        // Disabled nudges: the detail line disappears entirely.
        i.arch_nudges = false;
        assert_eq!(compute(&i).arch, None);
    }

    #[test]
    fn rel_time_buckets() {
        let now = t0();
        assert_eq!(rel_time(now - Duration::seconds(5), now), "just now");
        assert_eq!(rel_time(now - Duration::minutes(3), now), "3m ago");
        assert_eq!(rel_time(now - Duration::hours(2), now), "2h ago");
        assert_eq!(rel_time(now - Duration::days(4), now), "4d ago");
        // Clock skew (then in the future) never underflows.
        assert_eq!(rel_time(now + Duration::hours(1), now), "just now");
    }
}
