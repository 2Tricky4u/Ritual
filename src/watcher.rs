//! Source watcher: debounced file events -> "rerun check.sh fast" requests.
//! Paused while any agent run is active (the PostToolUse hook already checks
//! agent edits; a concurrent watcher run would fight over build locks).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use tokio::sync::mpsc;

use crate::ui::app::AppMsg;

/// Directories never worth a check run.
const IGNORED_SEGMENTS: &[&str] = &[
    ".git",
    ".ritual",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];

fn interesting(path: &Path, root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    !rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        IGNORED_SEGMENTS.contains(&s.as_ref()) || s.starts_with('.')
    })
}

/// Handle keeps the watcher alive; drop to stop watching.
pub struct Watcher {
    _debouncer: notify_debouncer_full::Debouncer<
        notify_debouncer_full::notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    pub paused: Arc<AtomicBool>,
}

pub fn spawn(project_root: PathBuf, tx: mpsc::Sender<AppMsg>) -> Result<Watcher> {
    let paused = Arc::new(AtomicBool::new(false));
    let paused2 = paused.clone();
    let root = project_root.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(400),
        None,
        move |result: DebounceEventResult| {
            if paused2.load(Ordering::SeqCst) {
                return;
            }
            let Ok(events) = result else { return };
            let relevant = events
                .iter()
                .flat_map(|e| e.paths.iter())
                .any(|p| interesting(p, &root));
            if relevant {
                let _ = tx.blocking_send(AppMsg::FileChanged);
            }
        },
    )?;
    debouncer.watch(&project_root, RecursiveMode::Recursive)?;
    Ok(Watcher {
        _debouncer: debouncer,
        paused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_noise_paths() {
        let root = Path::new("/proj");
        assert!(interesting(Path::new("/proj/src/main.rs"), root));
        assert!(interesting(Path::new("/proj/check.sh"), root));
        assert!(!interesting(Path::new("/proj/.git/index"), root));
        assert!(!interesting(Path::new("/proj/.ritual/runs/x.jsonl"), root));
        assert!(!interesting(Path::new("/proj/target/debug/foo"), root));
        assert!(!interesting(Path::new("/proj/node_modules/x/y.js"), root));
        assert!(!interesting(Path::new("/other/src/main.rs"), root));
        assert!(!interesting(Path::new("/proj/.hidden/file"), root));
    }
}
