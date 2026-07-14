//! Minimal git helpers for the code-fix batch: snapshot the worktree before a
//! run, list what the run touched, produce a review diff, and scope-restore on
//! a failed gate. Every command runs with `current_dir(cwd)` (the checkout),
//! following the read-only inline style used elsewhere (`secrets::changed_files`).
//!
//! Restore is deliberately SCOPED to the files the fix touched so a user's
//! unrelated working-tree changes survive an auto-revert. `git stash create`
//! captures tracked modifications only; untracked files are handled explicitly.
//! Known limitation: a fix that MODIFIES a pre-existing *untracked* file cannot
//! be content-restored (it is not in the snapshot base); a newly-created
//! untracked file IS reverted by deletion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// The well-known SHA of git's empty tree, used as the snapshot base when the
/// repo has no commits yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A pre-fix worktree snapshot. `base` is a commit/tree to diff against;
/// `untracked_before` is the set of untracked files that already existed (so a
/// file the fix newly creates can be told apart); `before_hashes` is the content
/// hash of every pre-existing untracked file (so a MODIFICATION to one - which
/// `git diff` cannot see - is still detectable); `head` is the HEAD commit at
/// snapshot time (so a rogue commit/reset by the fixer can be caught).
#[derive(Debug, Clone)]
pub struct GitSnapshot {
    pub base: String,
    pub untracked_before: Vec<PathBuf>,
    pub before_hashes: HashMap<PathBuf, String>,
    pub head: Option<String>,
}

/// The observed change a run produced: the `git diff` of tracked files plus, for
/// each untracked file whose content moved (or that is brand-new), its path and
/// current contents. This is the evidence handed to the re-review, and it stays
/// non-empty even when the edited code is untracked in the repo - the blind spot
/// that let a "fix" pass verification with no real change behind it.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    pub diff: String,
    /// `(path, current contents)` for each changed/new untracked file.
    pub untracked_changed: Vec<(PathBuf, String)>,
}

impl ChangeSet {
    /// No tracked hunks, no new files, and no untracked content moved: the run
    /// changed nothing we can observe. The gate treats this as a failure.
    pub fn is_empty(&self) -> bool {
        self.diff.trim().is_empty() && self.untracked_changed.is_empty()
    }

    /// The full change rendered for the re-review prompt: the git diff followed
    /// by a labelled block per changed untracked file so the reviewer always
    /// sees real content, never an empty diff.
    pub fn render(&self) -> String {
        let mut out = self.diff.clone();
        for (p, contents) in &self.untracked_changed {
            out.push_str(&format!(
                "\n\n=== CHANGED (untracked): {} ===\n{}\n",
                p.display(),
                contents
            ));
        }
        out
    }
}

/// SHA-256 of a file's bytes, or None if it can't be read.
fn hash_file(cwd: &Path, rel: &Path) -> Option<String> {
    std::fs::read(cwd.join(rel))
        .ok()
        .map(|bytes| crate::provenance::sha256_hex(&bytes))
}

/// Run `git <args>` in `cwd`, returning trimmed stdout on success or an error.
fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    anyhow::ensure!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Like `git`, but a non-zero exit yields `None` instead of an error (for
/// probes such as `stash create` on a clean tree or `rev-parse HEAD` with no
/// commits).
fn git_opt(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        None
    }
}

fn lines_to_paths(s: &str) -> Vec<PathBuf> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn untracked(cwd: &Path) -> Result<Vec<PathBuf>> {
    Ok(lines_to_paths(&git(
        cwd,
        &["ls-files", "--others", "--exclude-standard"],
    )?))
}

/// Snapshot the current worktree state so a later `diff_since` can show what a
/// run changed. `git stash create` captures tracked working-tree+index changes
/// as a commit without altering the tree; it prints nothing on a clean tree, so
/// we fall back to HEAD, then to the empty-tree sentinel for a repo with no
/// commits.
pub fn snapshot(cwd: &Path) -> Result<GitSnapshot> {
    let base = git_opt(cwd, &["stash", "create"])
        .filter(|s| !s.is_empty())
        .or_else(|| git_opt(cwd, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| EMPTY_TREE.to_string());
    let head = git_opt(cwd, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty());
    let untracked_before = untracked(cwd)?;
    let before_hashes = untracked_before
        .iter()
        .filter_map(|p| hash_file(cwd, p).map(|h| (p.clone(), h)))
        .collect();
    Ok(GitSnapshot {
        base,
        untracked_before,
        before_hashes,
        head,
    })
}

/// Everything the run changed since the snapshot, detected without relying on
/// git tracking: the tracked-file `git diff` plus, for every untracked file that
/// is new or whose content hash moved, its path and current contents. Gitignored
/// files (excluded from `git ls-files --others --exclude-standard`) are the one
/// residual blind spot; a fix that touches only those yields an empty ChangeSet
/// and the gate fails closed.
pub fn observed_change(cwd: &Path, snap: &GitSnapshot) -> Result<ChangeSet> {
    let diff = git(cwd, &["-c", "core.quotepath=false", "diff", &snap.base])?;
    let now_untracked = untracked(cwd)?;
    let mut untracked_changed = Vec::new();
    for p in &now_untracked {
        let now = hash_file(cwd, p);
        let moved = match snap.before_hashes.get(p) {
            Some(before) => now.as_deref() != Some(before.as_str()),
            None => true, // brand-new untracked file
        };
        if moved {
            let contents = std::fs::read_to_string(cwd.join(p)).unwrap_or_default();
            untracked_changed.push((p.clone(), contents));
        }
    }
    // A DELETION of a pre-existing untracked file is invisible to both git and
    // the loop above (the path is simply gone), so a deletion-only fix would
    // read as "no change". Detect it: a snapshot-hashed path that is neither
    // still untracked nor present on disk was removed.
    let now_set: std::collections::HashSet<&PathBuf> = now_untracked.iter().collect();
    for p in snap.before_hashes.keys() {
        if !now_set.contains(p) && !cwd.join(p).exists() {
            untracked_changed.push((p.clone(), "(file deleted)".to_string()));
        }
    }
    Ok(ChangeSet {
        diff,
        untracked_changed,
    })
}

/// Did HEAD move since the snapshot? True when the fixer committed, reset, or
/// checked out despite the prompt forbidding it - grounds to abort the batch.
/// Fails OPEN on an unreadable HEAD (a transient git error must not trigger a
/// false abort), matching `observed_change`'s error handling.
pub fn head_moved(cwd: &Path, snap: &GitSnapshot) -> bool {
    let now = git_opt(cwd, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty());
    match (now, &snap.head) {
        (Some(a), Some(b)) => &a != b, // both readable: moved iff different
        (Some(_), None) => true,       // was unborn, now has a commit
        (None, _) => false,            // can't read HEAD now: don't falsely abort
    }
}

/// A human-readable diff of everything the run changed since the snapshot, for
/// the re-review agent. `git diff` omits untracked files, so newly-created
/// files are listed under a `NEW FILES:` trailer. NOTE: git cannot show
/// modifications to files that are UNTRACKED in this repo (e.g. a code subtree
/// that was never `git add`ed) - the reviewer is told to read the code directly
/// to cover that gap.
pub fn diff_since(cwd: &Path, snap: &GitSnapshot) -> Result<String> {
    let mut out = git(cwd, &["-c", "core.quotepath=false", "diff", &snap.base])?;
    let new_files: Vec<PathBuf> = untracked(cwd)?
        .into_iter()
        .filter(|p| !snap.untracked_before.contains(p))
        .collect();
    if !new_files.is_empty() {
        out.push_str("\n\nNEW FILES:\n");
        for p in new_files {
            out.push_str(&format!("  {}\n", p.display()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "t"][..],
        ] {
            Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
        tmp
    }

    fn commit(p: &Path, file: &str, body: &str) {
        std::fs::write(p.join(file), body).unwrap();
        git(p, &["add", file]).unwrap();
        git(p, &["commit", "-qm", "x"]).unwrap();
    }

    #[test]
    fn diff_since_shows_hunks_and_new_files() {
        let t = init_repo();
        let p = t.path();
        commit(p, "a.rs", "one\n");
        let snap = snapshot(p).unwrap();
        std::fs::write(p.join("a.rs"), "two\n").unwrap();
        std::fs::write(p.join("added.rs"), "brand new\n").unwrap();
        let d = diff_since(p, &snap).unwrap();
        assert!(d.contains("a.rs"), "modified file in diff");
        assert!(d.contains("+two"), "hunk present");
        assert!(
            d.contains("NEW FILES:") && d.contains("added.rs"),
            "new file listed"
        );
    }

    #[test]
    fn snapshot_falls_back_to_head_then_empty_tree() {
        let t = init_repo();
        let p = t.path();
        // No commits → empty-tree sentinel.
        assert_eq!(snapshot(p).unwrap().base, EMPTY_TREE);
        // With a commit and a clean tree → HEAD (stash create is empty).
        commit(p, "a.rs", "A\n");
        let head = git(p, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(snapshot(p).unwrap().base, head);
    }

    #[test]
    fn observed_change_sees_a_modified_untracked_file() {
        // The exact homeserver blind spot: git diff shows nothing for an
        // untracked file, but the content hash catches the edit.
        let t = init_repo();
        let p = t.path();
        commit(p, "a.rs", "tracked\n");
        std::fs::write(p.join("u.rs"), "before\n").unwrap(); // untracked
        let snap = snapshot(p).unwrap();
        std::fs::write(p.join("u.rs"), "after\n").unwrap(); // modify untracked
        let cs = observed_change(p, &snap).unwrap();
        assert!(!cs.is_empty(), "untracked edit is observed");
        assert!(
            cs.untracked_changed
                .iter()
                .any(|(pp, c)| pp == Path::new("u.rs") && c.contains("after")),
        );
        let r = cs.render();
        assert!(r.contains("CHANGED (untracked): u.rs") && r.contains("after"));
    }

    #[test]
    fn observed_change_sees_a_deleted_untracked_file() {
        let t = init_repo();
        let p = t.path();
        commit(p, "a.rs", "x\n");
        std::fs::write(p.join("scratch.rs"), "junk\n").unwrap(); // untracked
        let snap = snapshot(p).unwrap();
        std::fs::remove_file(p.join("scratch.rs")).unwrap(); // deletion-only fix
        let cs = observed_change(p, &snap).unwrap();
        assert!(!cs.is_empty(), "a deletion is an observable change");
        assert!(
            cs.untracked_changed
                .iter()
                .any(|(pp, c)| pp == Path::new("scratch.rs") && c.contains("deleted")),
        );
    }

    #[test]
    fn observed_change_is_empty_when_nothing_moved() {
        let t = init_repo();
        let p = t.path();
        commit(p, "a.rs", "x\n");
        std::fs::write(p.join("u.rs"), "keep\n").unwrap();
        let snap = snapshot(p).unwrap();
        assert!(observed_change(p, &snap).unwrap().is_empty());
    }

    #[test]
    fn observed_change_shows_tracked_hunks_and_new_files() {
        let t = init_repo();
        let p = t.path();
        commit(p, "a.rs", "one\n");
        let snap = snapshot(p).unwrap();
        std::fs::write(p.join("a.rs"), "two\n").unwrap();
        std::fs::write(p.join("new.rs"), "brand new\n").unwrap();
        let cs = observed_change(p, &snap).unwrap();
        assert!(cs.diff.contains("+two"), "tracked hunk present");
        assert!(
            cs.untracked_changed
                .iter()
                .any(|(pp, c)| pp == Path::new("new.rs") && c.contains("brand new")),
            "new untracked file carried with contents",
        );
    }

    #[test]
    fn head_moved_detects_a_commit_or_reset() {
        let t = init_repo();
        let p = t.path();
        commit(p, "a.rs", "x\n");
        let snap = snapshot(p).unwrap();
        assert!(!head_moved(p, &snap));
        commit(p, "b.rs", "y\n");
        assert!(head_moved(p, &snap), "a commit moves HEAD");

        // From an unborn HEAD, the first commit also counts as movement.
        let t2 = init_repo();
        let p2 = t2.path();
        let snap2 = snapshot(p2).unwrap();
        assert_eq!(snap2.head, None);
        commit(p2, "a.rs", "x\n");
        assert!(head_moved(p2, &snap2));
    }
}
