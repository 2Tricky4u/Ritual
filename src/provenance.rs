//! Run provenance: reproducibility bundles (what exactly ran) and a
//! tamper-evident hash chain over the run archive (21 CFR Part 11-style
//! append-only audit trail, minus the ceremony).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::history::RunMeta;
use crate::state::RitualDirs;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    to_hex(&h.finalize())
}

fn to_hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Everything needed to answer "what exactly produced this run?".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReproBundle {
    #[serde(default)]
    pub git_commit: Option<String>,
    #[serde(default)]
    pub git_dirty_diff_sha256: Option<String>,
    #[serde(default)]
    pub claude_version: Option<String>,
    #[serde(default)]
    pub codex_version: Option<String>,
    /// skill name -> sha256 of its SKILL.md
    #[serde(default)]
    pub skill_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub config_snapshot: BTreeMap<String, String>,
}

fn cmd_line(bin: &str, args: &[&str], cwd: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Digest of the tree's dirt: the tracked `git diff HEAD` PLUS every
/// untracked file's path and content hash. `git diff` alone omits untracked
/// files, so two runs with different untracked inputs used to fingerprint
/// identically - a repro bundle that couldn't tell its inputs apart.
fn dirty_digest(root: &Path) -> Option<String> {
    // Unborn HEAD (fresh repo, no commits): `git diff HEAD` fails, but the
    // untracked inputs below still distinguish runs - diff against the
    // empty-tree sentinel instead of giving up (None == "clean", a lie here).
    let diff = cmd_line("git", &["diff", "HEAD"], root)
        .or_else(|| cmd_line("git", &["diff", crate::git::EMPTY_TREE], root))?;
    let mut input = diff;
    let untracked = cmd_line(
        "git",
        &[
            "-c",
            "core.quotepath=false",
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
        ],
        root,
    )
    .unwrap_or_default();
    let mut paths: Vec<&str> = untracked.split('\0').filter(|p| !p.is_empty()).collect();
    paths.sort_unstable();
    for p in paths {
        let hash = std::fs::read(root.join(p))
            .map(|b| sha256_hex(&b))
            .unwrap_or_default();
        input.push_str(&format!("\n{p}\n{hash}"));
    }
    (!input.is_empty()).then(|| sha256_hex(input.as_bytes()))
}

/// Best-effort collection: a missing tool yields None, never an error.
/// Compact tree identity for staleness comparison: "HEADsha:dirtydigest".
/// None outside a git work tree - callers must treat None as "unknown",
/// never as "clean" or "changed". Cost: 2-3 git subprocesses; call at
/// event cadence (stage completion, debounced refresh), never per frame.
pub fn tree_fingerprint(root: &Path) -> Option<String> {
    if !crate::git::in_work_tree(root) {
        return None;
    }
    let head = cmd_line("git", &["rev-parse", "HEAD"], root).unwrap_or_else(|| "unborn".into());
    let dirty = dirty_digest(root).unwrap_or_else(|| "clean".into());
    Some(format!("{head}:{dirty}"))
}

/// [`tree_fingerprint`] with everything under the root `.ritual/` excluded
/// from the dirt: the architecture map's staleness must reflect SOURCE
/// changes, not ritual's own artifacts (plans, findings, the map's sidecar) -
/// an unscoped stamp would go stale the moment the normal loop writes
/// plan.md. HEAD still participates: any commit is advisory staleness, same
/// semantics as the stage fingerprints.
pub fn arch_fingerprint(root: &Path) -> Option<String> {
    let _ = root;
    unimplemented!("phase 1 red")
}

pub fn collect(cfg: &Config, dirs: &RitualDirs) -> ReproBundle {
    let root = &dirs.work_root;
    let git_commit = cmd_line("git", &["rev-parse", "HEAD"], root);
    let git_dirty_diff_sha256 = dirty_digest(root);
    let claude_version = cmd_line(&cfg.claude_cmd[0], &["--version"], root);
    let codex_version = cmd_line(&cfg.codex_cmd[0], &["--version"], root);

    let mut skill_hashes = BTreeMap::new();
    // Resolve through the shared seam-aware helper: hashing a DIFFERENT
    // claude home than doctor/init use would make repro's "skill files
    // changed" verdict disagree with doctor about the same environment.
    if let Some(home) = crate::workbench::claude_home() {
        // Every vendored workbench skill (the installed set is the contract).
        for (skill, _) in crate::workbench::SKILLS {
            let p = home.join(format!("skills/{skill}/SKILL.md"));
            if let Ok(bytes) = std::fs::read(&p) {
                skill_hashes.insert(skill.to_string(), sha256_hex(&bytes));
            }
        }
    }

    let mut config_snapshot = BTreeMap::new();
    config_snapshot.insert("base_ref".into(), cfg.base_ref.clone());
    config_snapshot.insert("redaction".into(), cfg.redaction.to_string());
    config_snapshot.insert(
        "budget_plan_review_usd".into(),
        cfg.budget_plan_review_usd.to_string(),
    );
    config_snapshot.insert(
        "budget_dual_review_usd".into(),
        cfg.budget_dual_review_usd.to_string(),
    );
    for (stage, model) in &cfg.models {
        config_snapshot.insert(format!("model.{stage}"), model.clone());
    }

    ReproBundle {
        git_commit,
        git_dirty_diff_sha256,
        claude_version,
        codex_version,
        skill_hashes,
        config_snapshot,
    }
}

/// Chain entry stored in each run meta.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Chain {
    pub prev: String,
    pub this: String,
}

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// this = sha256(prev ‖ sha256(archive bytes) ‖ canonical(meta minus chain)).
pub fn compute_link(prev: &str, archive_bytes: &[u8], meta: &RunMeta) -> Result<Chain> {
    let mut unchained = meta.clone();
    unchained.chain = None;
    let canonical = serde_json::to_vec(&unchained).context("serializing meta for chain")?;
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(sha256_hex(archive_bytes).as_bytes());
    h.update(&canonical);
    Ok(Chain {
        prev: prev.to_string(),
        this: to_hex(&h.finalize()),
    })
}

/// Rolling genesis written by `ritual clean`: stands in for pruned chained
/// runs so pruning never breaks `verify-log`. Only the latest checkpoint is
/// kept on disk; lineage is carried by `prev_checkpoint` (the replaced
/// checkpoint's self_hash, or GENESIS for the first). Trust model: the
/// checkpoint is the trust anchor for everything it covers, like a git
/// shallow clone, history behind it is attested by one hash, everything
/// after it stays fully tamper-evident.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checkpoint {
    /// run_id of the NEWEST pruned chained run this checkpoint covers.
    pub as_of_run_id: String,
    /// chain.this of that run: the link the oldest surviving run chains from.
    pub link_hash: String,
    /// Cumulative chained runs pruned under this lineage.
    pub pruned_runs: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// self_hash of the checkpoint this replaced, or GENESIS for the first.
    pub prev_checkpoint: String,
    /// sha256 of the canonical JSON of self with self_hash blanked.
    /// Mirrors compute_link's "canonical minus the hash field" pattern.
    pub self_hash: String,
}

pub fn checkpoint_path(runs_dir: &Path) -> std::path::PathBuf {
    runs_dir.join("checkpoint.json")
}

pub fn compute_checkpoint_hash(cp: &Checkpoint) -> Result<String> {
    let mut blank = cp.clone();
    blank.self_hash = String::new();
    let canonical = serde_json::to_vec(&blank).context("serializing checkpoint for hash")?;
    Ok(sha256_hex(&canonical))
}

/// Ok(None) when absent; Err when present but unreadable/unparseable:
/// verify_log treats that as a broken chain, not a missing checkpoint.
pub fn load_checkpoint(runs_dir: &Path) -> Result<Option<Checkpoint>> {
    let path = checkpoint_path(runs_dir);
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cp: Checkpoint =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cp))
}

/// Atomic write (writer-unique tmp + rename), like State::save.
pub fn write_checkpoint(runs_dir: &Path, cp: &Checkpoint) -> Result<()> {
    crate::fsx::atomic_write(
        &checkpoint_path(runs_dir),
        serde_json::to_string_pretty(cp)?.as_bytes(),
    )
}

/// Cross-process exclusive lock serializing "read [`last_link`] -> write
/// chained meta" as one atomic unit (and clean's verify -> checkpoint
/// window). flock(2) auto-releases when the fd closes, so a SIGKILLed
/// daemon can never wedge the chain. Same-host only (flock over NFS is
/// unreliable) - consistent with the pid-liveness assumptions elsewhere.
/// The lock file lives beside the metas and is ignored by clean/load_all.
pub fn with_chain_lock<T>(runs_dir: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::fs::create_dir_all(runs_dir)?;
    let file = std::fs::File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(runs_dir.join("chain.lock"))?;
    let _guard = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, errno)| anyhow::anyhow!("locking chain.lock: {errno}"))?;
    f()
}

/// Chained metas covered by a checkpoint: the backward linkage walk from its
/// `link_hash` (this -> prev). During clean's crash window (checkpoint
/// written, files not yet deleted) these are still on disk and must be
/// skipped by verify - same trust model as the old `run_id <= as_of_run_id`
/// skip: covered content is attested by the checkpoint alone.
pub(crate) fn covered_run_ids(
    metas: &[RunMeta],
    link_hash: &str,
) -> std::collections::HashSet<String> {
    let by_this: std::collections::HashMap<&str, &RunMeta> = metas
        .iter()
        .filter_map(|m| m.chain.as_ref().map(|c| (c.this.as_str(), m)))
        .collect();
    let mut covered = std::collections::HashSet::new();
    let mut cur = link_hash;
    while let Some(m) = by_this.get(cur) {
        if !covered.insert(m.run_id.clone()) {
            break; // cycle paranoia: impossible to build honestly
        }
        cur = m.chain.as_ref().unwrap().prev.as_str();
    }
    covered
}

/// Chained run_ids in LINKAGE order (oldest -> newest) from `start`. Stops
/// at a fork (two successors) or a missing link; callers needing a total
/// order run [`verify_log`] first. Chain order is FINISH order, which under
/// parallel runs (audit lanes) is NOT run_id order.
pub fn chain_order(metas: &[RunMeta], start: &str) -> Vec<String> {
    let mut by_prev: std::collections::HashMap<&str, Vec<&RunMeta>> =
        std::collections::HashMap::new();
    for m in metas {
        if let Some(c) = &m.chain {
            by_prev.entry(c.prev.as_str()).or_default().push(m);
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut prev = start.to_string();
    while let Some(succ) = by_prev.get(prev.as_str()) {
        if succ.len() != 1 || out.contains(&succ[0].run_id) {
            break; // fork or cycle: not a total order
        }
        prev = succ[0].chain.as_ref().unwrap().this.clone();
        out.push(succ[0].run_id.clone());
    }
    out
}

/// The chain TIP's `this` hash - the link no other meta chains from - else
/// the checkpoint link (pruned-everything case: the next run chains onto
/// the checkpoint), else GENESIS. Writers call this UNDER [`with_chain_lock`],
/// making "exactly one tip" an invariant; on a legacy multi-tip fork the
/// largest-run_id tip wins deterministically (matches the old behavior;
/// verify_log reports the fork itself).
pub fn last_link(runs_dir: &Path) -> String {
    if let Ok(metas) = crate::history::load_all(runs_dir) {
        // load_all is newest-first, so `find` picks the largest-run_id tip.
        let chained: Vec<&RunMeta> = metas.iter().filter(|m| m.chain.is_some()).collect();
        if let Some(first) = chained.first() {
            let prevs: std::collections::HashSet<&str> = chained
                .iter()
                .map(|m| m.chain.as_ref().unwrap().prev.as_str())
                .collect();
            let tip = chained
                .iter()
                .find(|m| !prevs.contains(m.chain.as_ref().unwrap().this.as_str()))
                .unwrap_or(first); // all-cycle degenerate: newest wins
            return tip.chain.as_ref().unwrap().this.clone();
        }
    }
    if let Ok(Some(cp)) = load_checkpoint(runs_dir) {
        return cp.link_hash;
    }
    GENESIS.to_string()
}

#[derive(Debug, PartialEq)]
pub enum VerifyOutcome {
    Ok {
        runs: usize,
        checkpoint: Option<Checkpoint>,
    },
    Broken {
        run_id: String,
        reason: String,
    },
}

/// Walk the chain oldest→newest, recomputing every link. When a checkpoint
/// exists it becomes the starting link, and chained metas it covers
/// (run_id <= as_of_run_id) are skipped; that makes a crash between
/// "checkpoint written" and "files deleted" recoverable instead of Broken.
pub fn verify_log(runs_dir: &Path) -> Result<VerifyOutcome> {
    let checkpoint = match load_checkpoint(runs_dir) {
        Ok(cp) => cp,
        Err(e) => {
            return Ok(VerifyOutcome::Broken {
                run_id: "checkpoint.json".into(),
                reason: format!("unreadable checkpoint: {e:#}"),
            });
        }
    };
    if let Some(cp) = &checkpoint
        && compute_checkpoint_hash(cp)? != cp.self_hash
    {
        return Ok(VerifyOutcome::Broken {
            run_id: "checkpoint.json".into(),
            reason: "checkpoint self-hash mismatch (checkpoint.json was modified)".into(),
        });
    }

    let metas = crate::history::load_all(runs_dir)?;
    // Skip metas the checkpoint covers - by LINKAGE (backward walk from its
    // anchor), not by run_id: chain order is finish order, and under
    // parallel runs a covered link can carry a larger run_id than a live one.
    let covered = checkpoint
        .as_ref()
        .map(|cp| covered_run_ids(&metas, &cp.link_hash))
        .unwrap_or_default();
    let chained: Vec<&RunMeta> = metas
        .iter()
        .filter(|m| m.chain.is_some())
        .filter(|m| !covered.contains(&m.run_id))
        .collect();
    // Walk prev -> this LINKAGE from the anchor: legacy sequential chains
    // are linkage-ordered lists too, so one algorithm covers both, and
    // out-of-order finishes (parallel audit lanes) verify clean.
    let mut by_prev: std::collections::BTreeMap<&str, Vec<&RunMeta>> =
        std::collections::BTreeMap::new();
    for m in &chained {
        by_prev
            .entry(m.chain.as_ref().unwrap().prev.as_str())
            .or_default()
            .push(m);
    }
    for succ in by_prev.values_mut() {
        succ.sort_by(|a, b| a.run_id.cmp(&b.run_id)); // deterministic fork naming
    }
    let mut prev = checkpoint
        .as_ref()
        .map(|cp| cp.link_hash.clone())
        .unwrap_or_else(|| GENESIS.to_string());
    let mut walked = 0usize;
    while let Some(succ) = by_prev.remove(prev.as_str()) {
        if succ.len() > 1 {
            return Ok(VerifyOutcome::Broken {
                run_id: succ[0].run_id.clone(),
                reason: format!("chain fork: {} runs share prev-link {prev}", succ.len()),
            });
        }
        let meta = succ[0];
        let chain = meta.chain.as_ref().unwrap();
        let archive = runs_dir.join(format!("{}.jsonl", meta.run_id));
        let bytes = std::fs::read(&archive).unwrap_or_default();
        let expected = compute_link(&prev, &bytes, meta)?;
        if expected.this != chain.this {
            return Ok(VerifyOutcome::Broken {
                run_id: meta.run_id.clone(),
                reason: "content hash mismatch (archive or meta was modified)".into(),
            });
        }
        prev = chain.this.clone();
        walked += 1;
    }
    if walked != chained.len() {
        // A chained meta the walk never consumed: bad start hash, deleted
        // middle link, or a meta imported from elsewhere.
        let orphan = by_prev
            .values()
            .flatten()
            .map(|m| m.run_id.clone())
            .min()
            .unwrap_or_default();
        return Ok(VerifyOutcome::Broken {
            run_id: orphan,
            reason: "orphaned link (prev-hash not reachable from the chain start)".into(),
        });
    }
    Ok(VerifyOutcome::Ok {
        runs: walked,
        checkpoint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_run(dir: &Path, run_id: &str, prev: &str) -> Chain {
        let archive = dir.join(format!("{run_id}.jsonl"));
        std::fs::write(&archive, format!("line-of-{run_id}\n")).unwrap();
        let mut meta = RunMeta {
            run_id: run_id.into(),
            stage: "test".into(),
            ok: true,
            ..Default::default()
        };
        let chain = compute_link(prev, &std::fs::read(&archive).unwrap(), &meta).unwrap();
        meta.chain = Some(chain.clone());
        std::fs::write(
            dir.join(format!("{run_id}.meta.json")),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        chain
    }

    #[test]
    fn dirty_digest_sees_untracked_inputs_on_an_unborn_head() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(p)
            .output()
            .unwrap();
        // No commits: `git diff HEAD` fails. The untracked input must still
        // fingerprint (None would make two different inputs look identical).
        std::fs::write(p.join("input-a.txt"), "a\n").unwrap();
        let da = dirty_digest(p).expect("digest on unborn HEAD");
        std::fs::write(p.join("input-a.txt"), "b\n").unwrap();
        let db = dirty_digest(p).expect("digest on unborn HEAD");
        assert_ne!(da, db, "different untracked content must differ");
    }

    #[test]
    fn tree_fingerprint_tracks_edits_and_is_none_outside_git() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(tree_fingerprint(tmp.path()), None, "non-git = unknown");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let clean = tree_fingerprint(tmp.path()).expect("git tree fingerprints");
        std::fs::write(tmp.path().join("input.txt"), "changed\n").unwrap();
        let dirty = tree_fingerprint(tmp.path()).expect("still Some");
        assert_ne!(clean, dirty, "an edit must change the fingerprint");
    }

    #[test]
    fn arch_fingerprint_ignores_ritual_dirt_but_tracks_source() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        assert_eq!(arch_fingerprint(p), None, "non-git = unknown");
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "t"][..],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
        std::fs::write(p.join("src.rs"), "one\n").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "x"][..]] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
        let base = arch_fingerprint(p).expect("git tree fingerprints");
        assert_eq!(
            arch_fingerprint(p).as_deref(),
            Some(base.as_str()),
            "stable"
        );

        // Untracked .ritual/ churn (plans, findings, the sidecar itself) is
        // invisible: the normal loop must not stale the map it just stamped.
        std::fs::create_dir_all(p.join(".ritual/features/main")).unwrap();
        std::fs::write(p.join(".ritual/features/main/plan.md"), "# plan\n").unwrap();
        std::fs::write(p.join(".ritual/architecture.fingerprint"), "x\n").unwrap();
        assert_eq!(arch_fingerprint(p).as_deref(), Some(base.as_str()));

        // A lookalike prefix OUTSIDE .ritual/ is real source dirt.
        std::fs::write(p.join(".ritualized"), "not ours\n").unwrap();
        let with_lookalike = arch_fingerprint(p).expect("still Some");
        assert_ne!(with_lookalike, base, ".ritualized is not .ritual/");
        std::fs::remove_file(p.join(".ritualized")).unwrap();

        // Tracked source edits + new untracked source both move it.
        std::fs::write(p.join("src.rs"), "two\n").unwrap();
        let tracked_edit = arch_fingerprint(p).expect("still Some");
        assert_ne!(tracked_edit, base, "tracked source edit");
        std::fs::write(p.join("new.rs"), "fresh\n").unwrap();
        let untracked_add = arch_fingerprint(p).expect("still Some");
        assert_ne!(untracked_add, tracked_edit, "untracked source add");

        // Reverting the source dirt returns to the baseline identity.
        std::fs::remove_file(p.join("new.rs")).unwrap();
        std::fs::write(p.join("src.rs"), "one\n").unwrap();
        assert_eq!(
            arch_fingerprint(p).as_deref(),
            Some(base.as_str()),
            "revert"
        );

        // Any commit moves HEAD and therefore the fingerprint (advisory).
        std::process::Command::new("git")
            .args(["commit", "-qm", "empty", "--allow-empty"])
            .current_dir(p)
            .output()
            .unwrap();
        assert_ne!(
            arch_fingerprint(p).as_deref(),
            Some(base.as_str()),
            "HEAD moved"
        );
    }

    #[test]
    fn arch_fingerprint_ignores_tracked_ritual_changes_too() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "t"][..],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
        std::fs::create_dir_all(p.join(".ritual")).unwrap();
        std::fs::write(p.join(".ritual/invariants.md"), "- rule\n").unwrap();
        std::fs::write(p.join("src.rs"), "one\n").unwrap();
        for args in [&["add", "-f", "."][..], &["commit", "-qm", "x"][..]] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
        let base = arch_fingerprint(p).expect("git tree fingerprints");
        // A TRACKED .ritual file's diff is excluded too, not just untracked.
        std::fs::write(p.join(".ritual/invariants.md"), "- rule\n- more\n").unwrap();
        assert_eq!(arch_fingerprint(p).as_deref(), Some(base.as_str()));
    }

    #[test]
    fn chain_verifies_and_detects_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = mk_run(tmp.path(), "20260711T000001Z-a", GENESIS);
        let c2 = mk_run(tmp.path(), "20260711T000002Z-b", &c1.this);
        assert_eq!(last_link(tmp.path()), c2.this);
        assert_eq!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok {
                runs: 2,
                checkpoint: None
            }
        );

        // Tamper with the first archive: verification must break at run a.
        std::fs::write(tmp.path().join("20260711T000001Z-a.jsonl"), "edited!\n").unwrap();
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, .. } => assert!(run_id.ends_with("-a")),
            other => panic!("expected broken chain, got {other:?}"),
        }
    }

    fn mk_checkpoint(as_of: &str, link: &str, pruned: usize, prev_cp: &str) -> Checkpoint {
        let mut cp = Checkpoint {
            as_of_run_id: as_of.into(),
            link_hash: link.into(),
            pruned_runs: pruned,
            created_at: chrono::Utc::now(),
            prev_checkpoint: prev_cp.into(),
            self_hash: String::new(),
        };
        cp.self_hash = compute_checkpoint_hash(&cp).unwrap();
        cp
    }

    #[test]
    fn checkpoint_roundtrip_and_hash() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_checkpoint(tmp.path()).unwrap().is_none());
        let cp = mk_checkpoint("20260711T000001Z-a", "deadbeef", 3, GENESIS);
        write_checkpoint(tmp.path(), &cp).unwrap();
        let loaded = load_checkpoint(tmp.path()).unwrap().unwrap();
        assert_eq!(loaded, cp);
        assert_eq!(compute_checkpoint_hash(&loaded).unwrap(), loaded.self_hash);
        // Garbage on disk -> Err (verify_log maps it to Broken).
        std::fs::write(checkpoint_path(tmp.path()), "not json").unwrap();
        assert!(load_checkpoint(tmp.path()).is_err());
    }

    #[test]
    fn verify_starts_from_checkpoint_and_skips_covered_runs() {
        let tmp = tempfile::tempdir().unwrap();
        // Full chain a -> b -> c.
        let c1 = mk_run(tmp.path(), "20260711T000001Z-a", GENESIS);
        let c2 = mk_run(tmp.path(), "20260711T000002Z-b", &c1.this);
        let c3 = mk_run(tmp.path(), "20260711T000003Z-c", &c2.this);
        // Checkpoint covering a+b, as clean would write it.
        let cp = mk_checkpoint("20260711T000002Z-b", &c2.this, 2, GENESIS);
        write_checkpoint(tmp.path(), &cp).unwrap();

        // Crash-recovery window: a and b still on disk but covered -> skipped.
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Ok { runs, checkpoint } => {
                assert_eq!(runs, 1); // only c is walked
                assert_eq!(checkpoint.unwrap().pruned_runs, 2);
            }
            other => panic!("expected ok, got {other:?}"),
        }

        // After deletion (the normal post-clean state) it still verifies.
        for id in ["20260711T000001Z-a", "20260711T000002Z-b"] {
            std::fs::remove_file(tmp.path().join(format!("{id}.meta.json"))).unwrap();
            std::fs::remove_file(tmp.path().join(format!("{id}.jsonl"))).unwrap();
        }
        assert!(matches!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok { runs: 1, .. }
        ));
        // last_link is still the newest surviving run.
        assert_eq!(last_link(tmp.path()), c3.this);
    }

    #[test]
    fn tampered_checkpoint_breaks_verification() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = mk_run(tmp.path(), "20260711T000001Z-a", GENESIS);
        let c2 = mk_run(tmp.path(), "20260711T000002Z-b", &c1.this);
        let cp = mk_checkpoint("20260711T000001Z-a", &c1.this, 1, GENESIS);
        write_checkpoint(tmp.path(), &cp).unwrap();
        let _ = c2;

        // Field edit without re-hashing -> self-hash mismatch at the checkpoint.
        let mut forged = cp.clone();
        forged.pruned_runs = 999;
        write_checkpoint(tmp.path(), &forged).unwrap();
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, reason } => {
                assert_eq!(run_id, "checkpoint.json");
                assert!(reason.contains("self-hash"));
            }
            other => panic!("expected broken, got {other:?}"),
        }

        // Consistently re-hashed forgery of link_hash: the anchor covers
        // NOTHING (backward linkage walk finds no meta), so every run on
        // disk is unreachable from it - broken at the earliest orphan.
        let forged = mk_checkpoint("20260711T000001Z-a", "0000forged", 1, GENESIS);
        write_checkpoint(tmp.path(), &forged).unwrap();
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, reason } => {
                assert!(run_id.ends_with("-a"), "{run_id}");
                assert!(reason.contains("orphaned link"), "{reason}");
            }
            other => panic!("expected broken, got {other:?}"),
        }
    }

    #[test]
    fn out_of_order_finish_verifies_by_linkage() {
        // Parallel audit lanes: B (larger run_id) finishes FIRST and chains
        // from GENESIS; A (smaller run_id) finishes later and chains from B.
        // The old run_id-ordered walk called this CHAIN BROKEN.
        let tmp = tempfile::tempdir().unwrap();
        let cb = mk_run(tmp.path(), "20260716T000002Z-b", GENESIS);
        let ca = mk_run(tmp.path(), "20260716T000001Z-a", &cb.this);
        assert_eq!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok {
                runs: 2,
                checkpoint: None
            }
        );
        // And the tip is A (by linkage), not B (largest run_id).
        assert_eq!(last_link(tmp.path()), ca.this);
    }

    #[test]
    fn chain_lock_serializes_concurrent_appends() {
        // 8 threads race "read last_link -> write chained meta"; flock
        // contends across separate opens even in one process. Without the
        // lock two writers read the same prev and fork the chain.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::thread::scope(|s| {
            for i in 0..8 {
                let dir = dir.clone();
                s.spawn(move || {
                    with_chain_lock(&dir, || {
                        let prev = last_link(&dir);
                        mk_run(&dir, &format!("20260716T00000{i}Z-t{i}"), &prev);
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok {
                runs: 8,
                checkpoint: None
            },
            "eight serialized appends form one unforked line"
        );
    }

    #[test]
    fn fork_is_broken_and_last_link_picks_newest_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let _f1 = mk_run(tmp.path(), "20260716T000001Z-a", GENESIS);
        let f2 = mk_run(tmp.path(), "20260716T000002Z-b", GENESIS);
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, reason } => {
                assert!(run_id.ends_with("-a"), "smallest run_id named: {run_id}");
                assert!(reason.contains("chain fork"), "{reason}");
            }
            other => panic!("expected fork, got {other:?}"),
        }
        // Writers stay deterministic on legacy forks: largest-run_id tip.
        assert_eq!(last_link(tmp.path()), f2.this);
    }

    #[test]
    fn orphaned_meta_is_broken() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = mk_run(tmp.path(), "20260716T000001Z-a", GENESIS);
        let _ = c1;
        mk_run(tmp.path(), "20260716T000002Z-b", "0000nowhere");
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, reason } => {
                assert!(run_id.ends_with("-b"), "{run_id}");
                assert!(reason.contains("orphaned link"), "{reason}");
            }
            other => panic!("expected orphan, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_skips_covered_by_linkage_not_run_id() {
        // Interleaved chain: X (LARGE run_id) is linkage-oldest, then Y
        // (small run_id), then Z. A checkpoint anchored at Y covers X and Y
        // by LINKAGE even though X's run_id is larger than Y's - the old
        // `run_id <= as_of` skip got this wrong.
        let tmp = tempfile::tempdir().unwrap();
        let cx = mk_run(tmp.path(), "20260716T000009Z-x", GENESIS);
        let cy = mk_run(tmp.path(), "20260716T000001Z-y", &cx.this);
        let cz = mk_run(tmp.path(), "20260716T000002Z-z", &cy.this);
        let cp = mk_checkpoint("20260716T000001Z-y", &cy.this, 2, GENESIS);
        write_checkpoint(tmp.path(), &cp).unwrap();
        // Crash window: X and Y still on disk.
        assert!(matches!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok { runs: 1, .. }
        ));
        // After the deletion finishes: same verdict.
        for id in ["20260716T000009Z-x", "20260716T000001Z-y"] {
            std::fs::remove_file(tmp.path().join(format!("{id}.meta.json"))).unwrap();
            std::fs::remove_file(tmp.path().join(format!("{id}.jsonl"))).unwrap();
        }
        assert!(matches!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok { runs: 1, .. }
        ));
        assert_eq!(last_link(tmp.path()), cz.this);
    }

    #[test]
    fn last_link_falls_back_to_checkpoint_when_all_runs_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(last_link(tmp.path()), GENESIS);
        let cp = mk_checkpoint("20260711T000005Z-x", "cafebabe", 5, GENESIS);
        write_checkpoint(tmp.path(), &cp).unwrap();
        assert_eq!(last_link(tmp.path()), "cafebabe");

        // The next run chains onto the checkpoint link and verifies.
        let c = mk_run(tmp.path(), "20260711T000006Z-y", "cafebabe");
        assert!(matches!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok { runs: 1, .. }
        ));
        assert_eq!(last_link(tmp.path()), c.this);
    }

    #[test]
    fn collect_is_best_effort() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let cfg = Config {
            claude_cmd: vec!["/nonexistent/claude".into()],
            codex_cmd: vec!["/nonexistent/codex".into()],
            ..Default::default()
        };
        let b = collect(&cfg, &dirs);
        assert!(b.claude_version.is_none());
        assert!(b.config_snapshot.contains_key("redaction"));
    }

    #[test]
    fn verify_maps_unreadable_checkpoint_to_broken() {
        let tmp = tempfile::tempdir().unwrap();
        mk_run(tmp.path(), "20260711T000001Z-a", GENESIS);
        std::fs::write(checkpoint_path(tmp.path()), "not json").unwrap();
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, .. } => assert_eq!(run_id, "checkpoint.json"),
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[test]
    fn missing_archive_breaks_the_chain_at_that_run() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = mk_run(tmp.path(), "20260711T000001Z-a", GENESIS);
        mk_run(tmp.path(), "20260711T000002Z-b", &c1.this);
        // The meta survives but its .jsonl vanishes (partial deletion, disk
        // repair, hand-tampering): content hash can no longer match.
        std::fs::remove_file(tmp.path().join("20260711T000001Z-a.jsonl")).unwrap();
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, .. } => assert!(run_id.ends_with("-a"), "{run_id}"),
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[test]
    fn last_link_skips_newer_unchained_metas() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = mk_run(tmp.path(), "20260711T000001Z-a", GENESIS);
        // A NEWER meta without a chain (failed write, foreign copy) must not
        // reset the chain to GENESIS: the newest CHAINED link wins.
        let meta = RunMeta {
            run_id: "20260711T000002Z-b".into(),
            ..Default::default()
        };
        std::fs::write(
            tmp.path().join("20260711T000002Z-b.meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        assert_eq!(last_link(tmp.path()), c1.this);
    }

    #[test]
    fn collect_captures_git_state_and_model_routing() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let mut cfg = Config {
            claude_cmd: vec!["/nonexistent/claude".into()],
            codex_cmd: vec!["/nonexistent/codex".into()],
            ..Default::default()
        };
        cfg.models.insert("plan-review".into(), "opus".into());

        // Outside a git repo: git fields stay None, snapshot still filled.
        let b = collect(&cfg, &dirs);
        assert!(b.git_commit.is_none());
        assert!(b.git_dirty_diff_sha256.is_none());
        assert_eq!(b.config_snapshot["base_ref"], "main");
        assert_eq!(b.config_snapshot["model.plan-review"], "opus");

        // A repo with a commit and a dirty tracked file fills both.
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "x",
        ]);
        std::fs::write(tmp.path().join("f.txt"), "two\n").unwrap();
        let b = collect(&cfg, &dirs);
        assert!(b.git_commit.is_some());
        assert!(b.git_dirty_diff_sha256.is_some(), "dirty diff hashed");

        // Untracked files are part of the run's input: they must dirty the
        // digest (git diff alone can't see them), and different untracked
        // content must fingerprint differently.
        git(&["checkout", "-q", "--", "f.txt"]); // tracked tree clean again
        std::fs::write(tmp.path().join("extra.txt"), "input A\n").unwrap();
        let a = collect(&cfg, &dirs).git_dirty_diff_sha256;
        assert!(a.is_some(), "untracked-only dirt still digests");
        std::fs::write(tmp.path().join("extra.txt"), "input B\n").unwrap();
        let b2 = collect(&cfg, &dirs).git_dirty_diff_sha256;
        assert_ne!(a, b2, "different untracked content, different digest");
    }
}
