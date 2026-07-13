//! Multi-level undo/redo for chat-edited documents. Snapshots are plain
//! files under `.ritual/features/<slug>/.undo/<doc>/` (newest = last in
//! lexicographic order), with a mirror `.redo/<doc>/` branch. Persisted so
//! the stack survives TUI restarts, capped so it can't grow unbounded.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use crate::state::RitualDirs;

/// Undo depth per document, enough for a whole chat session of edits.
pub const CAP: usize = 10;

fn undo_dir(dirs: &RitualDirs, slug: &str, doc_label: &str) -> PathBuf {
    dirs.feature_dir(slug).join(".undo").join(doc_label)
}

fn redo_dir(dirs: &RitualDirs, slug: &str, doc_label: &str) -> PathBuf {
    dirs.feature_dir(slug).join(".redo").join(doc_label)
}

/// Sortable snapshot name: millis + process-local sequence (two edits in the
/// same millisecond, e.g. a drained queue, must still order correctly).
fn snapshot_name() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{:04}.md",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Snapshot files, oldest first.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    out.sort();
    out
}

/// The pre-0.5 single-level `.{doc}.md.undo` becomes the stack's OLDEST
/// entry ("0-legacy" sorts before any timestamped name).
fn migrate_legacy(dirs: &RitualDirs, slug: &str, doc_label: &str) {
    let legacy = dirs.feature_dir(slug).join(format!(".{doc_label}.md.undo"));
    if legacy.is_file() {
        let dir = undo_dir(dirs, slug, doc_label);
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::rename(&legacy, dir.join("0-legacy.md"));
        }
    }
}

/// Record the pre-edit state (called before every chat edit). A new edit
/// invalidates the redo branch, standard editor semantics.
pub fn push(dirs: &RitualDirs, slug: &str, doc_label: &str, content: &str) -> Result<()> {
    migrate_legacy(dirs, slug, doc_label);
    let dir = undo_dir(dirs, slug, doc_label);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(snapshot_name()), content)?;
    let mut e = entries(&dir);
    while e.len() > CAP {
        let _ = std::fs::remove_file(e.remove(0));
    }
    let _ = std::fs::remove_dir_all(redo_dir(dirs, slug, doc_label));
    Ok(())
}

/// Walk one step back: the current doc content moves onto the redo branch,
/// the newest undo snapshot becomes the doc. false = stack empty.
pub fn undo(dirs: &RitualDirs, slug: &str, doc_label: &str, doc_path: &Path) -> Result<bool> {
    migrate_legacy(dirs, slug, doc_label);
    let mut e = entries(&undo_dir(dirs, slug, doc_label));
    let Some(snap) = e.pop() else {
        return Ok(false);
    };
    let current = std::fs::read_to_string(doc_path).unwrap_or_default();
    let rdir = redo_dir(dirs, slug, doc_label);
    std::fs::create_dir_all(&rdir)?;
    std::fs::write(rdir.join(snapshot_name()), current)?;
    let restored = std::fs::read_to_string(&snap).context("reading undo snapshot")?;
    std::fs::write(doc_path, restored)?;
    std::fs::remove_file(&snap)?;
    Ok(true)
}

/// Walk one step forward again (only meaningful right after undos; any new
/// edit clears this branch). false = nothing to redo.
pub fn redo(dirs: &RitualDirs, slug: &str, doc_label: &str, doc_path: &Path) -> Result<bool> {
    let mut e = entries(&redo_dir(dirs, slug, doc_label));
    let Some(snap) = e.pop() else {
        return Ok(false);
    };
    let current = std::fs::read_to_string(doc_path).unwrap_or_default();
    let udir = undo_dir(dirs, slug, doc_label);
    std::fs::create_dir_all(&udir)?;
    // Directly onto the undo stack. This must NOT clear the redo branch.
    std::fs::write(udir.join(snapshot_name()), current)?;
    let restored = std::fs::read_to_string(&snap).context("reading redo snapshot")?;
    std::fs::write(doc_path, restored)?;
    std::fs::remove_file(&snap)?;
    Ok(true)
}

/// How many undo steps are available (TUI notes).
pub fn depth(dirs: &RitualDirs, slug: &str, doc_label: &str) -> usize {
    entries(&undo_dir(dirs, slug, doc_label)).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, RitualDirs, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        let doc = dirs.spec_file("s");
        (tmp, dirs, doc)
    }

    #[test]
    fn three_edits_walk_back_and_redo_replays() {
        let (_t, dirs, doc) = setup();
        // Simulate three edits, each snapshotting the pre-edit state.
        for (before, after) in [("v0", "v1"), ("v1", "v2"), ("v2", "v3")] {
            std::fs::write(&doc, before).unwrap();
            push(&dirs, "s", "spec", before).unwrap();
            std::fs::write(&doc, after).unwrap();
        }
        assert_eq!(depth(&dirs, "s", "spec"), 3);

        for expect in ["v2", "v1", "v0"] {
            assert!(undo(&dirs, "s", "spec", &doc).unwrap());
            assert_eq!(std::fs::read_to_string(&doc).unwrap(), expect);
        }
        assert!(!undo(&dirs, "s", "spec", &doc).unwrap(), "stack exhausted");

        for expect in ["v1", "v2", "v3"] {
            assert!(redo(&dirs, "s", "spec", &doc).unwrap());
            assert_eq!(std::fs::read_to_string(&doc).unwrap(), expect);
        }
        assert!(!redo(&dirs, "s", "spec", &doc).unwrap(), "redo exhausted");
    }

    #[test]
    fn a_new_edit_clears_the_redo_branch() {
        let (_t, dirs, doc) = setup();
        std::fs::write(&doc, "v1").unwrap();
        push(&dirs, "s", "spec", "v0").unwrap();
        assert!(undo(&dirs, "s", "spec", &doc).unwrap()); // doc = v0, redo has v1

        push(&dirs, "s", "spec", "v0").unwrap(); // a fresh edit
        std::fs::write(&doc, "v2").unwrap();
        assert!(!redo(&dirs, "s", "spec", &doc).unwrap(), "redo invalidated");
        assert_eq!(std::fs::read_to_string(&doc).unwrap(), "v2");
    }

    #[test]
    fn missing_doc_and_nonexistent_depth_are_harmless() {
        let (_t, dirs, doc) = setup();
        push(&dirs, "s", "spec", "v0").unwrap();
        // Doc file never written: undo restores the snapshot; redo brings
        // back the "missing" state as empty content.
        assert!(undo(&dirs, "s", "spec", &doc).unwrap());
        assert_eq!(std::fs::read_to_string(&doc).unwrap(), "v0");
        assert!(redo(&dirs, "s", "spec", &doc).unwrap());
        assert_eq!(std::fs::read_to_string(&doc).unwrap(), "");
        // Depth on a feature/doc that never had snapshots.
        assert_eq!(depth(&dirs, "elsewhere", "plan"), 0);
    }

    #[test]
    fn full_undo_redo_walk_after_cap_pruning() {
        let (_t, dirs, doc) = setup();
        for i in 0..CAP + 2 {
            push(&dirs, "s", "spec", &format!("v{i}")).unwrap();
        }
        std::fs::write(&doc, "final").unwrap();

        // Only CAP snapshots survive: walk all the way back...
        let mut undos = 0;
        while undo(&dirs, "s", "spec", &doc).unwrap() {
            undos += 1;
        }
        assert_eq!(undos, CAP);
        assert_eq!(std::fs::read_to_string(&doc).unwrap(), "v2", "oldest kept");
        // ...and redo all the way forward to the exact final content.
        let mut redos = 0;
        while redo(&dirs, "s", "spec", &doc).unwrap() {
            redos += 1;
        }
        assert_eq!(redos, CAP);
        assert_eq!(std::fs::read_to_string(&doc).unwrap(), "final");
    }

    #[test]
    fn cap_prunes_oldest_and_legacy_file_migrates() {
        let (_t, dirs, doc) = setup();
        // Pre-0.5 single-level snapshot becomes the deepest entry...
        std::fs::write(dirs.feature_dir("s").join(".spec.md.undo"), "ancient").unwrap();
        for i in 0..CAP + 3 {
            push(&dirs, "s", "spec", &format!("v{i}")).unwrap();
        }
        // ...but the cap has since pruned it (and the oldest pushes).
        assert_eq!(depth(&dirs, "s", "spec"), CAP);
        std::fs::write(&doc, "now").unwrap();
        assert!(undo(&dirs, "s", "spec", &doc).unwrap());
        assert_eq!(
            std::fs::read_to_string(&doc).unwrap(),
            format!("v{}", CAP + 2),
            "newest snapshot wins"
        );
        assert!(
            !dirs.feature_dir("s").join(".spec.md.undo").exists(),
            "legacy file consumed by migration"
        );
    }
}
