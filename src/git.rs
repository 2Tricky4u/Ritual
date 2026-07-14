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

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// The well-known SHA of git's empty tree, used as the snapshot base when the
/// repo has no commits yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A pre-fix worktree snapshot: a base commit/tree to diff and restore against,
/// plus the set of untracked files that already existed (so files the fix
/// newly creates can be told apart).
#[derive(Debug, Clone)]
pub struct GitSnapshot {
    pub base: String,
    pub untracked_before: Vec<PathBuf>,
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
    Ok(GitSnapshot {
        base,
        untracked_before: untracked(cwd)?,
    })
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
}
