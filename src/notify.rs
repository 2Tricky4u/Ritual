//! Desktop notifications via notify-send (feature-detected, best-effort).

use std::sync::OnceLock;

fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("notify-send")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Fire-and-forget. Never blocks, never errors: a missing notify-send or a
/// dead session bus must not affect the pipeline.
pub fn notify(enabled: bool, summary: &str, body: &str) {
    if !enabled || !available() {
        return;
    }
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=ritual", "--expire-time=8000", summary, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
