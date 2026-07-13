//! `ritual clean`: prune old run artifacts safely. Design cross-model
//! reviewed (plan-review, 2026-07-11): enumeration is by FILENAME (never by
//! `history::load_all`, which silently skips exactly the malformed metas that
//! most need cleaning), deletion ids come only from discovered filenames
//! (never from untrusted `RunMeta.run_id`), live runs are untouchable, and
//! chained runs are pruned only behind a tamper-evident [`Checkpoint`] so
//! `verify-log` stays intact forever.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::provenance::{self, Checkpoint, VerifyOutcome};
use crate::runner::{self, RunState};
use crate::state::{RitualDirs, State};

/// The four sidecar suffixes that make up one run group, in DELETION order:
/// meta first, so a partial failure leaves an orphan group the next clean
/// collects, and verify-log never observes meta-without-archive.
const SUFFIXES: [&str; 4] = [".meta.json", ".request.json", ".status", ".jsonl"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// Inside the newest-N retention window.
    KeepWindow,
    /// Referenced by a feature stage in state.json (takeover/reconcile need it).
    StateRef,
    /// Started today (UTC). Pruning it would silently reset the daily budget.
    Today,
    /// A live daemon owns it.
    Running,
    /// A newer chained run than this one is kept, so pruning this would punch a
    /// hole in the hash-chain walk that no checkpoint could represent.
    ChainContinuity,
}

impl KeepReason {
    pub fn label(&self) -> &'static str {
        match self {
            KeepReason::KeepWindow => "keep window",
            KeepReason::StateRef => "referenced by state.json",
            KeepReason::Today => "started today (budget ledger)",
            KeepReason::Running => "running",
            KeepReason::ChainContinuity => "chain continuity",
        }
    }
}

#[derive(Debug, Default)]
pub struct CleanReport {
    pub deleted_groups: Vec<String>,
    pub kept: Vec<(String, KeepReason)>,
    pub failures: Vec<(String, String)>,
    pub notices: Vec<String>,
    pub checkpoint: Option<Checkpoint>,
    pub dry_run: bool,
}

/// Prune `.ritual/runs`, keeping the newest `keep` finished runs plus every
/// additively-protected run (state-referenced, today's, live). Chained runs
/// prune only as a contiguous oldest prefix, covered by a checkpoint written
/// BEFORE any deletion.
pub fn clean(dirs: &RitualDirs, keep: usize, dry_run: bool) -> Result<CleanReport> {
    let runs_dir = dirs.runs_dir();
    let mut report = CleanReport {
        dry_run,
        ..Default::default()
    };
    if !runs_dir.is_dir() {
        report
            .notices
            .push("no runs directory, nothing to do".into());
        return Ok(report);
    }

    // 1. Enumerate by filename. Ids come ONLY from what's on disk.
    let mut groups: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(&runs_dir)?.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = SUFFIXES.iter().find_map(|s| name.strip_suffix(s)) else {
            continue; // checkpoint.json, tmp files, anything unknown
        };
        if !id.is_empty() {
            groups.insert(id.to_string());
        }
    }

    // 2. Protection inputs.
    let state_refs: HashSet<String> = if dirs.state_file().exists() {
        match State::load(dirs) {
            Ok(st) => st
                .features
                .values()
                .flat_map(|f| f.stages.values())
                .flat_map(|s| s.runs.iter().cloned())
                .collect(),
            Err(e) => {
                report
                    .notices
                    .push(format!("state.json unreadable ({e:#}); protecting nothing"));
                HashSet::new()
            }
        }
    } else {
        report
            .notices
            .push("no state.json, protecting nothing".into());
        HashSet::new()
    };
    let today = Utc::now().date_naive();

    // 3. Classify. BTreeMap iterates ids ascending = oldest first.
    let mut candidates: Vec<String> = Vec::new(); // deletable, oldest first
    let mut chained: BTreeMap<String, String> = BTreeMap::new(); // id -> chain.this
    let mut finished_unprotected: Vec<String> = Vec::new(); // newest-first below
    for id in groups.iter() {
        match runner::run_state(dirs, id) {
            RunState::Running(_) => report.kept.push((id.clone(), KeepReason::Running)),
            RunState::Vanished => candidates.push(id.clone()), // garbage
            RunState::Finished(meta) => {
                if let Some(chain) = &meta.chain {
                    chained.insert(id.clone(), chain.this.clone());
                }
                if state_refs.contains(id) || state_refs.contains(&meta.run_id) {
                    report.kept.push((id.clone(), KeepReason::StateRef));
                } else if meta.started_at.is_some_and(|t| t.date_naive() == today) {
                    report.kept.push((id.clone(), KeepReason::Today));
                } else {
                    finished_unprotected.push(id.clone());
                }
            }
        }
    }
    // Newest-N of the unprotected finished runs stay; the rest are candidates.
    finished_unprotected.sort();
    finished_unprotected.reverse();
    for (i, id) in finished_unprotected.iter().enumerate() {
        if i < keep {
            report.kept.push((id.clone(), KeepReason::KeepWindow));
        } else {
            candidates.push(id.clone());
        }
    }
    candidates.sort(); // oldest first

    // 4. Chained candidates prune only as a contiguous oldest prefix.
    let old_checkpoint = provenance::load_checkpoint(&runs_dir).unwrap_or_default();
    let covered = |id: &str| {
        old_checkpoint
            .as_ref()
            .is_some_and(|cp| id <= cp.as_of_run_id.as_str())
    };
    let candidate_set: HashSet<&String> = candidates.iter().collect();
    // Uncovered chained runs, oldest first; the prunable prefix must be an
    // unbroken run of candidates starting at the chain's current base.
    let mut chain_verified = true;
    let uncovered_chained: Vec<&String> = chained.keys().filter(|id| !covered(id)).collect();
    let mut prunable_chained: HashSet<String> = HashSet::new();
    if uncovered_chained
        .iter()
        .any(|id| candidate_set.contains(*id))
    {
        // Never checkpoint over a chain that is already broken.
        match provenance::verify_log(&runs_dir)? {
            VerifyOutcome::Ok { .. } => {}
            VerifyOutcome::Broken { run_id, .. } => {
                chain_verified = false;
                report.notices.push(format!(
                    "chain already broken at {run_id}: not checkpointing over it; chained runs kept"
                ));
            }
        }
        if chain_verified {
            for id in &uncovered_chained {
                if candidate_set.contains(*id) {
                    prunable_chained.insert((*id).clone());
                } else {
                    break; // first chained keeper ends the prefix
                }
            }
        }
    }
    // Chained candidates outside the prefix (or with a broken chain) are kept.
    candidates.retain(|id| {
        let is_chained_uncovered = chained.contains_key(id) && !covered(id);
        if is_chained_uncovered && !prunable_chained.contains(id) {
            report.kept.push((id.clone(), KeepReason::ChainContinuity));
            false
        } else {
            true
        }
    });

    // 5. New checkpoint covering the pruned chained prefix, written BEFORE
    //    any deletion, and never moving backwards.
    if !prunable_chained.is_empty() {
        let newest_pruned = prunable_chained.iter().max().unwrap().clone();
        let link_hash = chained[&newest_pruned].clone();
        let mut cp = Checkpoint {
            as_of_run_id: newest_pruned,
            link_hash,
            pruned_runs: old_checkpoint.as_ref().map(|c| c.pruned_runs).unwrap_or(0)
                + prunable_chained.len(),
            created_at: Utc::now(),
            prev_checkpoint: old_checkpoint
                .as_ref()
                .map(|c| c.self_hash.clone())
                .unwrap_or_else(|| provenance::GENESIS.to_string()),
            self_hash: String::new(),
        };
        cp.self_hash = provenance::compute_checkpoint_hash(&cp)?;
        let moves_forward = old_checkpoint
            .as_ref()
            .is_none_or(|old| cp.as_of_run_id > old.as_of_run_id);
        if moves_forward {
            if !dry_run {
                provenance::write_checkpoint(&runs_dir, &cp)
                    .context("writing checkpoint before pruning")?;
            }
            report.checkpoint = Some(cp);
        }
    }

    // 6. Delete, meta-first, continuing past failures. Every target is built
    //    from a discovered filename and asserted to stay inside runs_dir.
    for id in &candidates {
        let mut group_failed = false;
        for suffix in SUFFIXES {
            let path = runs_dir.join(format!("{id}{suffix}"));
            assert!(
                path.starts_with(&runs_dir),
                "deletion target escaped runs dir: {}",
                path.display()
            );
            if !path.exists() {
                continue;
            }
            if dry_run {
                continue;
            }
            if let Err(e) = std::fs::remove_file(&path) {
                report.failures.push((id.clone(), format!("{suffix}: {e}")));
                group_failed = true;
            }
        }
        if !group_failed {
            report.deleted_groups.push(id.clone());
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::RunMeta;
    use crate::provenance::{GENESIS, compute_link};
    use crate::state::{StageId, StageStatus};

    fn dirs(tmp: &tempfile::TempDir) -> RitualDirs {
        let d = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(d.runs_dir()).unwrap();
        d
    }

    /// A finished, UNchained run group dated yesterday (prunable by default).
    fn mk_finished(d: &RitualDirs, id: &str) {
        let meta = RunMeta {
            run_id: id.into(),
            stage: "plan-review".into(),
            ok: true,
            started_at: Some(Utc::now() - chrono::Duration::days(1)),
            ..Default::default()
        };
        std::fs::write(
            d.runs_dir().join(format!("{id}.meta.json")),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(d.runs_dir().join(format!("{id}.jsonl")), "line\n").unwrap();
    }

    /// A finished CHAINED run; returns the new link for the next one.
    fn mk_chained(d: &RitualDirs, id: &str, prev: &str) -> String {
        let archive = d.runs_dir().join(format!("{id}.jsonl"));
        std::fs::write(&archive, format!("line-of-{id}\n")).unwrap();
        let mut meta = RunMeta {
            run_id: id.into(),
            stage: "plan-review".into(),
            ok: true,
            started_at: Some(Utc::now() - chrono::Duration::days(1)),
            ..Default::default()
        };
        let chain = compute_link(prev, &std::fs::read(&archive).unwrap(), &meta).unwrap();
        meta.chain = Some(chain.clone());
        std::fs::write(
            d.runs_dir().join(format!("{id}.meta.json")),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        chain.this
    }

    fn ids(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn keep_count_and_keep_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        for i in 1..=4 {
            mk_finished(&d, &format!("20260701T00000{i}Z-x"));
        }
        let r = clean(&d, 2, false).unwrap();
        assert_eq!(
            ids(&r.deleted_groups),
            ["20260701T000001Z-x", "20260701T000002Z-x"]
        );
        assert!(!d.runs_dir().join("20260701T000001Z-x.meta.json").exists());
        assert!(d.runs_dir().join("20260701T000004Z-x.meta.json").exists());

        let r = clean(&d, 0, false).unwrap();
        assert_eq!(r.deleted_groups.len(), 2, "--keep 0 prunes the rest");
    }

    #[test]
    fn state_referenced_runs_are_protected_additively() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        for i in 1..=3 {
            mk_finished(&d, &format!("20260701T00000{i}Z-x"));
        }
        // Reference the OLDEST run from a stage; keep window = 1.
        let mut st = State::default();
        let f = st.feature_for_branch_mut("main");
        f.stages.entry(StageId::PlanReview).or_default().runs = vec!["20260701T000001Z-x".into()];
        f.stages.entry(StageId::PlanReview).or_default().status = StageStatus::Done;
        st.save(&d).unwrap();

        let r = clean(&d, 1, false).unwrap();
        // Oldest survives via StateRef without consuming the keep slot;
        // newest survives via the window; the middle one is pruned.
        assert_eq!(ids(&r.deleted_groups), ["20260701T000002Z-x"]);
        assert!(
            r.kept
                .iter()
                .any(|(id, why)| id == "20260701T000001Z-x" && *why == KeepReason::StateRef)
        );
    }

    #[test]
    fn state_ref_wins_over_today_in_keep_reasons() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        // A run dated TODAY that is ALSO state-referenced: classification
        // order makes StateRef the recorded reason (both protect it).
        let id = "20260712T000001Z-x";
        let meta = RunMeta {
            run_id: id.into(),
            stage: "plan-review".into(),
            ok: true,
            started_at: Some(Utc::now()),
            ..Default::default()
        };
        std::fs::write(
            d.runs_dir().join(format!("{id}.meta.json")),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let mut st = State::default();
        let f = st.feature_for_branch_mut("main");
        f.stages.entry(StageId::PlanReview).or_default().runs = vec![id.into()];
        st.save(&d).unwrap();

        let r = clean(&d, 0, false).unwrap();
        assert!(r.deleted_groups.is_empty());
        assert!(
            r.kept
                .iter()
                .any(|(k, why)| k == id && *why == KeepReason::StateRef),
            "{:?}",
            r.kept
        );
    }

    #[test]
    fn corrupt_checkpoint_does_not_block_unchained_pruning() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        std::fs::write(d.runs_dir().join("checkpoint.json"), "not json").unwrap();
        for i in 1..=2 {
            mk_finished(&d, &format!("20260701T00000{i}Z-x"));
        }
        let r = clean(&d, 0, false).unwrap();
        assert_eq!(r.deleted_groups.len(), 2, "unchained garbage still prunes");
    }

    #[test]
    fn partial_suffix_failure_excludes_the_group_and_records_it() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        mk_finished(&d, "20260701T000001Z-x");
        mk_finished(&d, "20260701T000002Z-x");
        // Make ONE suffix un-deletable: a non-empty DIRECTORY named like the
        // archive, so remove_file() fails on it while the meta unlinks fine.
        let blocker = d.runs_dir().join("20260701T000001Z-x.jsonl");
        std::fs::remove_file(&blocker).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("keep"), "x").unwrap();

        let r = clean(&d, 0, false).unwrap();
        assert_eq!(ids(&r.deleted_groups), ["20260701T000002Z-x"]);
        assert!(
            r.failures
                .iter()
                .any(|(id, why)| id == "20260701T000001Z-x" && why.contains(".jsonl")),
            "{:?}",
            r.failures
        );
    }

    #[test]
    fn dry_run_over_a_chained_prefix_reports_but_never_writes_the_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let l1 = mk_chained(&d, "20260701T000001Z-c", GENESIS);
        let l2 = mk_chained(&d, "20260701T000002Z-c", &l1);
        mk_chained(&d, "20260701T000003Z-c", &l2);

        let r = clean(&d, 1, true).unwrap();
        let cp = r
            .checkpoint
            .expect("dry-run reports the would-be checkpoint");
        assert_eq!(cp.as_of_run_id, "20260701T000002Z-c");
        assert!(
            crate::provenance::load_checkpoint(&d.runs_dir())
                .unwrap()
                .is_none(),
            "nothing written on disk"
        );
        // Everything still verifies exactly as before.
        assert_eq!(
            crate::provenance::verify_log(&d.runs_dir()).unwrap(),
            crate::provenance::VerifyOutcome::Ok {
                runs: 3,
                checkpoint: None
            }
        );
    }

    #[test]
    fn second_clean_never_rewrites_the_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let l1 = mk_chained(&d, "20260701T000001Z-c", GENESIS);
        let l2 = mk_chained(&d, "20260701T000002Z-c", &l1);
        mk_chained(&d, "20260701T000003Z-c", &l2);

        let r = clean(&d, 1, false).unwrap();
        let first = r.checkpoint.expect("first clean checkpoints");
        // No new candidates: the checkpoint must stay byte-identical (the
        // as_of can never regress, and an idle clean never rewrites it).
        let r = clean(&d, 1, false).unwrap();
        assert!(r.checkpoint.is_none());
        assert_eq!(
            crate::provenance::load_checkpoint(&d.runs_dir())
                .unwrap()
                .unwrap()
                .self_hash,
            first.self_hash
        );
    }

    #[test]
    fn dry_run_mutates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        for i in 1..=3 {
            mk_finished(&d, &format!("20260701T00000{i}Z-x"));
        }
        let before: Vec<_> = std::fs::read_dir(d.runs_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let r = clean(&d, 1, true).unwrap();
        assert_eq!(r.deleted_groups.len(), 2, "reports what WOULD go");
        let after: Vec<_> = std::fs::read_dir(d.runs_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before.len(), after.len(), "dry-run deleted something");
    }

    #[test]
    fn malformed_meta_and_orphan_sidecars_are_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        // Malformed meta (load_all would skip it, but filename enumeration must not).
        std::fs::write(d.runs_dir().join("20260701T000001Z-bad.meta.json"), "{oops").unwrap();
        std::fs::write(d.runs_dir().join("20260701T000001Z-bad.jsonl"), "x\n").unwrap();
        // Orphan sidecars from a crashed launch (dead pid).
        std::fs::write(
            d.runs_dir().join("20260701T000002Z-orphan.status"),
            r#"{"pid":999999999,"stage":"x","branch":"m"}"#,
        )
        .unwrap();
        std::fs::write(
            d.runs_dir().join("20260701T000003Z-orphan.request.json"),
            "{}",
        )
        .unwrap();
        let r = clean(&d, 50, false).unwrap();
        assert_eq!(r.deleted_groups.len(), 3, "{r:?}");
        assert_eq!(std::fs::read_dir(d.runs_dir()).unwrap().count(), 0);
    }

    #[test]
    fn live_runs_are_never_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        std::fs::write(
            d.runs_dir().join("20260701T000001Z-live.status"),
            format!(
                r#"{{"pid":{},"stage":"plan-review","branch":"m"}}"#,
                std::process::id()
            ),
        )
        .unwrap();
        let r = clean(&d, 0, false).unwrap();
        assert!(r.deleted_groups.is_empty());
        assert!(r.kept.iter().any(|(_, why)| *why == KeepReason::Running));
        assert!(d.runs_dir().join("20260701T000001Z-live.status").exists());
    }

    #[test]
    fn todays_runs_are_protected_for_the_budget_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let meta = RunMeta {
            run_id: "t".into(),
            stage: "plan-review".into(),
            ok: true,
            started_at: Some(Utc::now()),
            total_cost_usd: Some(3.0),
            ..Default::default()
        };
        std::fs::write(
            d.runs_dir().join("20260712T000001Z-today.meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let r = clean(&d, 0, false).unwrap();
        assert!(r.deleted_groups.is_empty());
        assert!(r.kept.iter().any(|(_, why)| *why == KeepReason::Today));
    }

    #[test]
    fn chained_runs_prune_as_prefix_with_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let l1 = mk_chained(&d, "20260701T000001Z-a", GENESIS);
        let l2 = mk_chained(&d, "20260701T000002Z-b", &l1);
        let _l3 = mk_chained(&d, "20260701T000003Z-c", &l2);

        let r = clean(&d, 1, false).unwrap();
        assert_eq!(
            ids(&r.deleted_groups),
            ["20260701T000001Z-a", "20260701T000002Z-b"]
        );
        let cp = r.checkpoint.expect("checkpoint written");
        assert_eq!(cp.as_of_run_id, "20260701T000002Z-b");
        assert_eq!(cp.link_hash, l2);
        assert_eq!(cp.pruned_runs, 2);
        // The chain still verifies from the checkpoint.
        assert!(matches!(
            provenance::verify_log(&d.runs_dir()).unwrap(),
            VerifyOutcome::Ok { runs: 1, .. }
        ));
    }

    #[test]
    fn protected_chained_run_blocks_newer_chained_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let l1 = mk_chained(&d, "20260701T000001Z-a", GENESIS);
        let l2 = mk_chained(&d, "20260701T000002Z-b", &l1);
        let _l3 = mk_chained(&d, "20260701T000003Z-c", &l2);
        // Protect the OLDEST via state.json; keep window 1 (newest only).
        let mut st = State::default();
        let f = st.feature_for_branch_mut("main");
        f.stages.entry(StageId::PlanReview).or_default().runs = vec!["20260701T000001Z-a".into()];
        st.save(&d).unwrap();

        let r = clean(&d, 1, false).unwrap();
        // b is a candidate but sits AFTER the kept a in the chain: pruning it
        // would hole the walk; kept with ChainContinuity, nothing deleted.
        assert!(r.deleted_groups.is_empty(), "{r:?}");
        assert!(
            r.kept
                .iter()
                .any(|(id, why)| id == "20260701T000002Z-b" && *why == KeepReason::ChainContinuity)
        );
        assert!(r.checkpoint.is_none());
        assert!(matches!(
            provenance::verify_log(&d.runs_dir()).unwrap(),
            VerifyOutcome::Ok { runs: 3, .. }
        ));
    }

    #[test]
    fn checkpoint_lineage_accumulates_across_cleans() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let l1 = mk_chained(&d, "20260701T000001Z-a", GENESIS);
        let l2 = mk_chained(&d, "20260701T000002Z-b", &l1);
        let r1 = clean(&d, 1, false).unwrap();
        let cp1 = r1.checkpoint.unwrap();
        assert_eq!(cp1.prev_checkpoint, GENESIS);

        let l3 = mk_chained(&d, "20260701T000003Z-c", &l2);
        let _l4 = mk_chained(&d, "20260701T000004Z-e", &l3);
        let r2 = clean(&d, 1, false).unwrap();
        let cp2 = r2.checkpoint.unwrap();
        assert_eq!(cp2.prev_checkpoint, cp1.self_hash, "lineage chains");
        assert_eq!(cp2.pruned_runs, cp1.pruned_runs + 2);
        assert!(matches!(
            provenance::verify_log(&d.runs_dir()).unwrap(),
            VerifyOutcome::Ok { runs: 1, .. }
        ));
    }

    #[test]
    fn broken_chain_refuses_chained_pruning() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let l1 = mk_chained(&d, "20260701T000001Z-a", GENESIS);
        let _l2 = mk_chained(&d, "20260701T000002Z-b", &l1);
        mk_finished(&d, "20260701T000003Z-plain"); // unchained garbage
        // Tamper the first archive.
        std::fs::write(d.runs_dir().join("20260701T000001Z-a.jsonl"), "edited\n").unwrap();

        let r = clean(&d, 0, false).unwrap();
        assert!(r.checkpoint.is_none());
        assert!(r.notices.iter().any(|n| n.contains("chain already broken")));
        // Unchained run still pruned; chained ones kept.
        assert_eq!(ids(&r.deleted_groups), ["20260701T000003Z-plain"]);
        assert!(d.runs_dir().join("20260701T000001Z-a.meta.json").exists());
    }

    #[test]
    fn path_escape_in_meta_run_id_cannot_delete_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        // A victim file OUTSIDE the runs dir.
        let victim = tmp.path().join("victim.jsonl");
        std::fs::write(&victim, "precious").unwrap();
        // A meta whose run_id field tries to escape. Deletion ids come from
        // filenames only, so this must be irrelevant.
        let meta = RunMeta {
            run_id: "../../victim".into(),
            stage: "plan-review".into(),
            ok: true,
            started_at: Some(Utc::now() - chrono::Duration::days(1)),
            ..Default::default()
        };
        std::fs::write(
            d.runs_dir().join("20260701T000001Z-evil.meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let r = clean(&d, 0, false).unwrap();
        assert_eq!(ids(&r.deleted_groups), ["20260701T000001Z-evil"]);
        assert!(victim.exists(), "file outside runs_dir was deleted");
    }

    #[test]
    fn missing_state_json_yields_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        mk_finished(&d, "20260701T000001Z-x");
        let r = clean(&d, 0, false).unwrap();
        assert!(r.notices.iter().any(|n| n.contains("no state.json")));
        assert_eq!(r.deleted_groups.len(), 1);
    }

    #[test]
    fn partial_failure_is_reported_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        mk_finished(&d, "20260701T000001Z-x");
        mk_finished(&d, "20260701T000002Z-y");
        // Make the runs dir read-only so unlink fails.
        let dir = d.runs_dir();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o555);
        std::fs::set_permissions(&dir, perms).unwrap();

        let r = clean(&d, 0, false).unwrap();

        let mut restore = std::fs::metadata(&dir).unwrap().permissions();
        restore.set_mode(0o755);
        std::fs::set_permissions(&dir, restore).unwrap();

        assert!(!r.failures.is_empty(), "failures recorded");
        assert!(r.deleted_groups.is_empty());
    }
}
