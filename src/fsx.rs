//! Crash- and concurrency-safe file writes.
//!
//! Every persistent artifact (state.json, findings files, checkpoint.json,
//! secrets findings) must be written atomically AND with a tmp name unique to
//! the writer: with a deterministic tmp name, a TUI and a CLI saving the same
//! target concurrently share the tmp file - interleaved writes can install
//! merged/truncated bytes and the loser's rename fails ENOENT.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically: unique tmp file in the SAME directory
/// (rename is only atomic within a filesystem), then rename over the target.
/// The tmp name folds in pid + a process-global counter so no two writers can
/// ever share a tmp file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        // No tmp litter left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/out.json");
        atomic_write(&path, b"x").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
    }

    #[test]
    fn concurrent_writers_never_corrupt_the_target() {
        // The defect this module closes: two writers with a DETERMINISTIC tmp
        // name interleave into merged bytes. With unique tmp names the target
        // must always parse as exactly one writer's payload.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let a = serde_json::to_vec(&serde_json::json!({"writer": "a", "pad": "x".repeat(4096)}))
            .unwrap();
        let b = serde_json::to_vec(&serde_json::json!({"writer": "b", "pad": "y".repeat(4096)}))
            .unwrap();
        std::thread::scope(|s| {
            for payload in [&a, &b] {
                s.spawn(|| {
                    for _ in 0..200 {
                        atomic_write(&path, payload).unwrap();
                    }
                });
            }
        });
        let end: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(matches!(end["writer"].as_str(), Some("a") | Some("b")));
    }
}
